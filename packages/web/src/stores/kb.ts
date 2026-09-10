/** The knowledge base's lifecycle: constituent mutations, and the deferred
 *  promote + validate they all funnel through.
 *
 * Mutations ingest (fast) but do not promote; each one runs `reprocess()`
 * once to promote + validate under the `promoting` flag views react to. */

import { defineStore } from "pinia";
import { MERGE, SUMO_FILE_SETTING } from "../constants";
import { Constituent } from "../models/Constituent";
import {
  Origin,
  GitOrigin,
  OriginKind,
  OriginJson,
  parseOrigin,
  serializeOrigin,
} from "../models/Origin";
import { call } from "../services/sigma";
import { fromOrigin } from "../services/sources";
import { lspReset, lspSyncDocument } from "../services/lsp";
import { scheduleSave as scheduleKbCacheSave } from "../services/kb-cache";
import { errMsg } from "../utils/format";
import { useWordNetStore } from "./wordnet";
import { useChangesStore } from "./changes";
import { useLibraryStore } from "./library";

/** One validation finding, as the worker's `validate` reports it. */
export interface Diagnostic {
  file?: string;
  line?: number;
  col?: number;
  end_line?: number;
  end_col?: number;
  severity: "error" | "warning" | "info" | "hint";
  kind: string;
  code: string;
  message: string;
}

/** What's mirrored to localStorage -- just enough to reconstruct an `Origin`
 *  and refetch its text on the next boot. */
export interface SavedConstituent {
  name: string;
  origin: OriginJson;
}

// Keep the post-processing state visible at least this long, so it is
// perceptible even when promote+validate finish in well under one frame.
const MIN_PROMOTING_MS = 650;

function loadSavedConstituents(): SavedConstituent[] {
  try {
    const raw = JSON.parse(localStorage.getItem(SUMO_FILE_SETTING) || "null");
    if (Array.isArray(raw) && raw.length) {
      // Entries saved before the library carried a bare kind string.
      return raw
        .filter((c: any) => c && typeof c.name === "string")
        .map((c: any) => ({
          name: c.name,
          origin: serializeOrigin(parseOrigin(c.origin, c.name)),
        }));
    }
  } catch {
    /* corrupt value */
  }
  return Constituent.defaults().map((c) => ({
    name: c.name,
    origin: serializeOrigin(c.origin),
  }));
}

function persistSaved(saved: SavedConstituent[]) {
  localStorage.setItem(SUMO_FILE_SETTING, JSON.stringify(saved));
}

export const useKBStore = defineStore("kb", {
  state: () => ({
    /** The page's source of truth for what is loaded, with fetched text. */
    constituents: [] as Constituent[],
    /** Mirrored to localStorage -- what the next boot reloads. */
    saved: loadSavedConstituents() as SavedConstituent[],
    /** The KB's validation findings, as of the last validate(). */
    diagnostics: [] as Diagnostic[],
    /** True while promote + validate is in flight (the post-processing window). */
    promoting: false,
    /** Settings: the language for term/format rendering and NL paraphrases. */
    uiLanguage: "EnglishLanguage",
    /** Render NL paraphrase variables as generic noun phrases instead of `?VarName`. */
    genericVars: false,
    /** The KB's `NaturalLanguage` instances, refreshed after each promote. */
    languages: [] as { symbol: string; label: string }[],
    /** The last `stats` payload, for the Browse home tiles. */
    stats: null as any,
    /** Counts change only when the KB does; `refreshStats` re-asks only when set. */
    statsStale: true,
  }),
  getters: {
    isLoaded: (state) => (name: string) =>
      state.constituents.some((c) => c.name === name),
    find:
      (state) =>
      (name: string, kind?: OriginKind): Constituent | undefined =>
        state.constituents.find(
          (c) => c.name === name && (!kind || c.origin.kind === kind),
        ),
    langLabel: (state) => (symbol: string) =>
      state.languages.find((l) => l.symbol === symbol)?.label ?? symbol,
    sumoNames: (state) =>
      state.constituents
        .filter((c) => c.origin.kind === "sumo")
        .map((c) => c.name),
  },
  actions: {
    /** Ingest one constituent's text into the worker session and track it.
     *  The constituent is tracked once ingested -- ingest still accepts
     *  content that carries non-fatal notices (e.g. "duplicate formula
     *  ignored"). */
    async ingest(
      name: string,
      text: string,
      origin: Origin = GitOrigin.default(),
    ): Promise<{ added: boolean; notices: string[] }> {
      if (this.isLoaded(name))
        return { added: false, notices: [`${name}: already loaded`] };
      const { notices } = await call<{ notices: string[] }>("ingest", {
        name,
        text,
      });
      this.constituents.push(new Constituent(name, origin, text));
      if (
        !this.saved.some(
          (c) => c.name === name && c.origin.kind === origin.kind,
        )
      ) {
        this.saved.push({ name, origin: serializeOrigin(origin) });
        persistSaved(this.saved);
      }
      return { added: true, notices };
    },

    /** Save `text` as constituent `name`/`origin` -- in place if loaded, else
     *  added. `file`-origin text is persisted to the library FIRST (a `file`
     *  entry with no OPFS copy would throw on next boot and abort loading
     *  every other constituent too); a `sumo`-origin save goes to the
     *  separate edit store and stays local until pushed. */
    async updateConstituentText(
      name: string,
      text: string,
      origin: Origin,
    ): Promise<{ added: boolean; notices: string[] }> {
      const changes = useChangesStore();
      if (origin.kind === "file")
        await useLibraryStore().writeLocal(name, text);
      const idx = this.constituents.findIndex(
        (c) => c.name === name && c.origin.kind === origin.kind,
      );
      // The text this edit descends from, which only the FIRST save of a
      // file can observe -- by the second, the constituent already holds
      // the first save.
      const pristine = idx === -1 ? undefined : this.constituents[idx].text;
      await changes.recordSave(name, origin.kind, text, pristine);
      if (idx === -1) {
        const r = await this.ingest(name, text, origin);
        await this.reprocess();
        return r;
      }
      this.constituents[idx] = new Constituent(
        name,
        this.constituents[idx].origin,
        text,
      );
      // In-place diff-commit instead of rebuildSession(): the LSP didChange
      // lane reconciles the buffer under the file's own name, so only the
      // changed sentences are processed. reprocess() then re-promotes
      // (no-op for untouched files) and re-validates for whole-KB diagnostics.
      await lspSyncDocument(name, text);
      await this.reprocess();
      return { added: false, notices: [] };
    },

    /** Rebuild the worker session from the currently tracked constituents --
     *  used by remove/reset. */
    async rebuildSession() {
      await call("newSession");
      lspReset();
      for (const c of this.constituents)
        await call("ingest", { name: c.name, text: c.text });
      // newSession() drops any installed WordNet lexicon; reinstall it (from
      // the cached fetch, so this never re-downloads).
      await useWordNetStore().install();
    },

    /** Drop every constituent and start an empty session (presets replace
     *  the whole KB rather than merging into it). */
    async replaceAll() {
      const changes = useChangesStore();
      for (const c of this.constituents)
        await changes.forget(c.name, c.origin.kind);
      this.constituents = [];
      this.saved = [];
      persistSaved(this.saved);
      await call("newSession");
      lspReset();
      await useWordNetStore().install();
    },

    /** Untrack `entries`, then rebuild the session and post-process ONCE for
     *  the whole batch. Library copies are kept. */
    async removeMany(entries: { name: string; kind: OriginKind }[]) {
      if (!entries.length) return;
      await this.untrack(entries);
      await this.rebuildSession();
      await this.reprocess();
    },

    async remove(name: string, kind: OriginKind = "sumo") {
      await this.removeMany([{ name, kind }]);
    },

    /** The bookkeeping half of removal: drop `entries` from the tracked and
     *  persisted lists and forget their edits. Their library copies stay
     *  (unloading is not deleting). The session is left stale -- callers
     *  rebuild it. */
    async untrack(entries: { name: string; kind: OriginKind }[]) {
      const changes = useChangesStore();
      const gone = (name: string, kind: OriginKind) =>
        entries.some((e) => e.name === name && e.kind === kind);
      this.constituents = this.constituents.filter(
        (c) => !gone(c.name, c.origin.kind),
      );
      this.saved = this.saved.filter((c) => !gone(c.name, c.origin.kind));
      persistSaved(this.saved);
      for (const { name, kind } of entries) await changes.forget(name, kind);
    },

    /** One batched mutation: ingest every `add` (a per-file failure is
     *  collected, not thrown), untrack every `remove`, and post-process
     *  exactly once. The session is rebuilt only when something was
     *  removed. Callers fetch the texts themselves. */
    async applyChanges({
      add = [],
      remove = [],
    }: {
      add?: { name: string; text: string; origin: Origin }[];
      remove?: { name: string; kind: OriginKind }[];
    }): Promise<{
      added: number;
      removed: number;
      notices: number;
      failed: string[];
    }> {
      let added = 0;
      let notices = 0;
      const failed: string[] = [];
      for (const { name, text, origin } of add) {
        try {
          const r = await this.ingest(name, text, origin);
          if (r.added) added += 1;
          notices += r.notices.length;
        } catch (e) {
          failed.push(`${name}: ${errMsg(e)}`);
        }
      }
      let removed = 0;
      if (remove.length) {
        const before = this.constituents.length;
        await this.untrack(remove);
        removed = before - this.constituents.length;
        await this.rebuildSession();
      }
      await this.reprocess();
      return { added, removed, notices, failed };
    },

    async resetToMerge() {
      const changes = useChangesStore();
      const merge = this.constituents.find((c) => c.name === MERGE);
      const dropped = this.constituents.filter((c) => c !== merge);
      for (const c of dropped) await changes.forget(c.name, c.origin.kind);
      this.constituents = merge ? [merge] : [];
      this.saved = merge
        ? [{ name: MERGE, origin: serializeOrigin(merge.origin) }]
        : [];
      persistSaved(this.saved);
      await this.rebuildSession();
      await this.reprocess();
    },

    /** Populate `languages` from the KB's `NaturalLanguage` instances,
     *  preserving the current `uiLanguage` when it's still valid. Only
     *  meaningful where the list can have changed (boot, promote). */
    async refreshLangSelect() {
      let languages: { symbol: string; label: string }[] | undefined;
      try {
        ({ languages } = await call("naturalLanguages"));
      } catch {
        return;
      }
      if (!languages?.length) return;
      this.languages = languages;
      const has = (v: string) => languages!.some((l) => l.symbol === v);
      this.uiLanguage = has(this.uiLanguage)
        ? this.uiLanguage
        : has("EnglishLanguage")
          ? "EnglishLanguage"
          : languages[0].symbol;
    },

    /** Re-run validation only (the Diagnostics tab's button). */
    async validate() {
      this.diagnostics = (
        await call<{ diagnostics: Diagnostic[] }>("validate")
      ).diagnostics;
      this.statsStale = true;
    },

    /** Whole-KB counts, re-asked only after the KB changed. */
    async refreshStats() {
      if (!this.statsStale && this.stats) return this.stats;
      const { stats } = await call("stats");
      this.stats = stats;
      this.statsStale = false;
      return stats;
    },

    // Promote every ingested constituent into the axiom base, THEN validate
    // once, THEN refresh the language list -- promote and validate are the
    // KB-size-bound steps, validation runs exactly once here.
    async promoteAndValidate() {
      await call("promoteAll", { names: this.constituents.map((c) => c.name) });
      this.diagnostics = (
        await call<{ diagnostics: Diagnostic[] }>("validate")
      ).diagnostics;
      this.statsStale = true;
      await this.refreshLangSelect();
      // Queued, not awaited: every mutation path funnels through here, so
      // the boot cache tracks the last successful promote.
      scheduleKbCacheSave();
    },

    /** Run promote+validate under the `promoting` flag (views grey out
     *  promote-dependent tabs and show a toast while it's set). Re-entrant:
     *  a nested call runs inside the outer window. */
    async reprocess() {
      const outer = !this.promoting;
      if (outer) this.promoting = true;
      const shownAt = performance.now();
      try {
        await this.promoteAndValidate();
      } finally {
        if (outer) {
          const held = performance.now() - shownAt;
          if (held < MIN_PROMOTING_MS) {
            await new Promise((r) => setTimeout(r, MIN_PROMOTING_MS - held));
          }
          this.promoting = false;
        }
      }
    },

    /** Fetch every saved constituent's text and ingest it, in order --
     *  boot's fetch+ingest loop. `onProgress` fires once per constituent,
     *  before its fetch starts. */
    async loadSavedConstituents(
      onProgress?: (name: string, index: number, total: number) => void,
    ) {
      const total = this.saved.length;
      let i = 0;
      for (const { name, origin: json } of this.saved) {
        i += 1;
        onProgress?.(name, i, total);
        const origin = parseOrigin(json, name);
        const text = await fromOrigin(name, origin);
        await this.ingest(name, text, origin);
      }
    },
  },
});
