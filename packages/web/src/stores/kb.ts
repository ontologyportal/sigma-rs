/** The knowledge base's lifecycle: constituent mutations, and the deferred
 *  promote + validate they all funnel through.
 *
 * Mutations ingest (fast) but do not promote; each one runs `reprocess()`
 * once to promote + validate under the `promoting` flag views react to. */

import { defineStore } from "pinia";
import {
  MERGE,
  SUMO_FILE_SETTING,
  UPDATE_BASELINES_KEY,
  UPDATE_PREFS_KEY,
} from "../constants";
import { Constituent } from "../models/Constituent";
import {
  Origin,
  GitOrigin,
  OriginKind,
  OriginJson,
  RemoteOrigin,
  isPersistedRow,
  originId,
  parseOrigin,
  serializeOrigin,
  sourceLabel,
} from "../models/Origin";
import { fetchRepoLastCommit } from "../api/github";
import { call } from "../services/sigma";
import { fetchAllTexts, fetchText, fromOrigin } from "../services/sources";
import { lspReset, lspSyncDocument } from "../services/lsp";
import {
  scheduleSave as scheduleKbCacheSave,
  flushSave as flushKbCacheSave,
} from "../services/kb-cache";
import { errMsg } from "../utils/format";
import { useWordNetStore } from "./wordnet";
import { blobSha, useChangesStore } from "./changes";
import { useLibraryStore } from "./library";
import { KbStats } from "sigmakee/sdk";
import type { Diagnostic as EngineDiagnostic, SumoSymbols } from "sigmakee/sdk";

/** One validation finding. The engine's own `validate` always reports a
 *  source location; the LSP lane (`lspSyncDocument`) reports buffer-relative
 *  findings for a document that may not be a loaded constituent, so the
 *  location fields are optional here. Everything else is the engine's shape,
 *  derived from it so the two cannot drift. */
type Located = "file" | "line" | "col" | "end_line" | "end_col";
export type Diagnostic = Omit<EngineDiagnostic, Located> &
  Partial<Pick<EngineDiagnostic, Located>>;

/** What's mirrored to localStorage -- just enough to reconstruct an `Origin`
 *  and refetch its text on the next boot. */
export interface SavedConstituent {
  name: string;
  origin: OriginJson;
}

/** Every loaded constituent that shares one origin instance (same repo,
 *  same URL, or every local upload), for the Sources card. */
export interface SourceGroup {
  key: string;
  origin: Origin;
  label: string;
  files: string[];
}

/** How a `sumo`/`url` source is watched for upstream changes. `file` sources
 *  have no upstream, so this never applies to them. */
export type UpdatePref = "auto-update" | "auto-check" | "no-check";

/** Cycle order for the Sources card's per-source button. */
const UPDATE_PREF_CYCLE: readonly UpdatePref[] = [
  "auto-update",
  "auto-check",
  "no-check",
];

/** Git sources default to always tracking upstream; URL sources have no
 *  freshness signal until a baseline is recorded, so they default to inert. */
function defaultUpdatePref(kind: OriginKind): UpdatePref {
  return kind === "sumo" ? "auto-update" : "no-check";
}

/** One changed file awaiting review in `UpdatePreviewDialog` -- `current` is
 *  what's loaded now, `incoming` is the freshly fetched update. Carries its
 *  own baseline so `acknowledgeReview` can advance a git group's shared
 *  commit or a URL file's own hash uniformly, once the file has been
 *  reviewed (applied or skipped -- either way, it shouldn't re-alert for
 *  the same version). */
export interface PendingUpdateFile {
  name: string;
  origin: Origin;
  current: string;
  incoming: string;
  baselineKey: string;
  baselineValue: string;
}

/** One update-check result surfaced to the Sources card, until dismissed.
 *  `review`, when present, is what a "Review" action opens in
 *  `UpdatePreviewDialog` instead of the alert being purely informational. */
export interface UpdateAlert {
  key: string;
  message: string;
  review?: PendingUpdateFile[];
}

function loadUpdatePrefs(): Record<string, UpdatePref> {
  try {
    const raw: unknown = JSON.parse(
      localStorage.getItem(UPDATE_PREFS_KEY) || "null",
    );
    if (raw && typeof raw === "object")
      return raw as Record<string, UpdatePref>;
  } catch {
    /* corrupt value */
  }
  return {};
}

function persistUpdatePrefs(prefs: Record<string, UpdatePref>) {
  localStorage.setItem(UPDATE_PREFS_KEY, JSON.stringify(prefs));
}

function loadUpdateBaselines(): Record<string, string> {
  try {
    const raw: unknown = JSON.parse(
      localStorage.getItem(UPDATE_BASELINES_KEY) || "null",
    );
    if (raw && typeof raw === "object") return raw as Record<string, string>;
  } catch {
    /* corrupt value */
  }
  return {};
}

function persistUpdateBaselines(baselines: Record<string, string>) {
  localStorage.setItem(UPDATE_BASELINES_KEY, JSON.stringify(baselines));
}

// Keep the post-processing state visible at least this long, so it is
// perceptible even when promote+validate finish in well under one frame.
const MIN_PROMOTING_MS = 650;

function loadSavedConstituents(): SavedConstituent[] {
  try {
    const raw: unknown = JSON.parse(
      localStorage.getItem(SUMO_FILE_SETTING) || "null",
    );
    if (Array.isArray(raw) && raw.length) {
      // Entries saved before the library carried a bare kind string.
      return raw.filter(isPersistedRow).map((c) => ({
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
    /** The engine's compiled-in SUMO symbol names, set at boot. Until then
     *  the SDK's own default stands in so nothing reads an empty symbol. */
    symbols: {
      defaultLanguage: "EnglishLanguage",
      naturalLanguageClass: "NaturalLanguage",
    } as SumoSymbols,
    /** Settings: the language for term/format rendering and NL paraphrases. */
    uiLanguage: "EnglishLanguage",
    /** Render NL paraphrase variables as generic noun phrases instead of `?VarName`. */
    genericVars: false,
    /** The KB's `NaturalLanguage` instances, refreshed after each promote. */
    languages: [] as { symbol: string; label: string }[],
    /** The last `stats` payload, for the Browse home tiles. */
    stats: null as KbStats | null,
    /** Counts change only when the KB does; `refreshStats` re-asks only when set. */
    statsStale: true,
    /** How each `sumo`/`url` source is watched, keyed by `originId`.
     *  Mirrored to localStorage; a missing entry means `defaultUpdatePref`. */
    updatePrefs: loadUpdatePrefs() as Record<string, UpdatePref>,
    /** The upstream commit (git, keyed by `originId`) or content hash (URL,
     *  keyed by constituent name) last seen by `checkForUpdates`. Mirrored to
     *  localStorage; recorded lazily, the first time each source is checked. */
    updateBaselines: loadUpdateBaselines() as Record<string, string>,
    /** Update-check results awaiting the user's attention, newest last. */
    updateAlerts: [] as UpdateAlert[],
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
    /** Loaded constituents bucketed by which exact source they came from,
     *  for the Sources card -- one group per repo/URL/`Local`. */
    sourceGroups: (state): SourceGroup[] => {
      const groups = new Map<string, SourceGroup>();
      for (const c of state.constituents) {
        const key = originId(c.origin);
        let g = groups.get(key);
        if (!g) {
          g = {
            key,
            origin: c.origin,
            label: sourceLabel(c.origin),
            files: [],
          };
          groups.set(key, g);
        }
        g.files.push(c.name);
      }
      return [...groups.values()].sort((a, b) =>
        a.label.localeCompare(b.label),
      );
    },
    /** The active update preference for one source, falling back to its
     *  kind's default when the user has never touched it. */
    prefFor:
      (state) =>
      (origin: Origin): UpdatePref =>
        state.updatePrefs[originId(origin)] ?? defaultUpdatePref(origin.kind),
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
      const { notices } = await call("ingest", {
        name,
        text,
      });
      this.constituents.push(new Constituent(name, origin, text));
      useLibraryStore().ensureEntry(name, origin, text.length);
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
        // An explicit save is rare enough to afford waiting out a real
        // write instead of the debounce, which a refresh right after
        // saving can otherwise beat (see flushSave).
        await flushKbCacheSave();
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
      await flushKbCacheSave();
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

    /** Fetch every `entries` file's text (concurrency-limited) and load them
     *  into the KB in one `applyChanges` batch -- shared by the KB tab's
     *  "load a standard set" presets and the Import dialog's "load into the
     *  knowledge base" checkbox. A per-file fetch failure is collected
     *  alongside any `applyChanges` failure, not thrown. */
    async loadFiles(
      entries: { name: string; origin: Origin }[],
      onProgress?: (done: number, total: number) => void,
    ): Promise<{ added: number; failed: string[] }> {
      if (!entries.length) return { added: 0, failed: [] };
      const texts = await fetchAllTexts(entries, 6, (n) =>
        onProgress?.(n, entries.length),
      );
      const add: { name: string; text: string; origin: Origin }[] = [];
      const failed: string[] = [];
      entries.forEach(({ name, origin }, i) => {
        const t = texts[i];
        if (t instanceof Error) failed.push(`${name}: ${t.message}`);
        else add.push({ name, text: t, origin });
      });
      const r = await this.applyChanges({ add });
      failed.push(...r.failed);
      return { added: r.added, failed };
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
      // Upstream SUMO's language symbol is `EnglishWrittenLanguage`; older
      // constituents say `EnglishLanguage`. Prefer whichever English the KB
      // actually documents before settling for the first listed language.
      const { defaultLanguage } = this.symbols;
      const english = languages.find((l) => /^English/.test(l.symbol));
      this.uiLanguage = has(this.uiLanguage)
        ? this.uiLanguage
        : has(defaultLanguage)
          ? defaultLanguage
          : (english?.symbol ?? languages[0].symbol);
    },

    /** Re-run validation only (the Diagnostics tab's button). */
    async validate() {
      this.diagnostics = (await call("validate")).diagnostics;
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
      this.diagnostics = (await call("validate")).diagnostics;
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

    /** Advance one source's update preference to the next in the cycle
     *  (auto-update -> auto-check -> no-check -> ...), from the Sources
     *  card's per-row button. */
    cycleUpdatePref(origin: Origin) {
      const key = originId(origin);
      const cur = this.prefFor(origin);
      const next =
        UPDATE_PREF_CYCLE[
          (UPDATE_PREF_CYCLE.indexOf(cur) + 1) % UPDATE_PREF_CYCLE.length
        ];
      this.updatePrefs[key] = next;
      persistUpdatePrefs(this.updatePrefs);
    },

    dismissUpdateAlert(key: string) {
      this.updateAlerts = this.updateAlerts.filter((a) => a.key !== key);
    },

    /** Overwrite `entries` in place (or ingest them, if not yet loaded) and
     *  post-process exactly once for the whole batch -- `checkForUpdates`'s
     *  apply step, batched the same way `applyChanges` batches additions. */
    async applyUpdates(
      entries: { name: string; text: string; origin: Origin }[],
    ) {
      const changes = useChangesStore();
      let changed = false;
      for (const { name, text, origin } of entries) {
        const idx = this.constituents.findIndex((c) => c.name === name);
        if (idx === -1) {
          const r = await this.ingest(name, text, origin);
          changed ||= r.added;
          continue;
        }
        if (this.constituents[idx].text === text) continue;
        this.constituents[idx] = new Constituent(name, origin, text);
        // Taking upstream's copy retires the local edit it replaces -- left
        // tracked, its text would outrank upstream in `fromOrigin` and undo
        // this update on the next boot.
        await changes.forget(name, origin.kind);
        await lspSyncDocument(name, text);
        changed = true;
      }
      if (changed) await this.reprocess();
    },

    /** One `sumo` source: compare the repo/branch's latest commit against
     *  the last-seen baseline. First check just records the baseline (there
     *  is nothing to diff against yet). A later mismatch either re-fetches
     *  and applies every file silently (`auto-update`, background only) or
     *  gathers per-file diffs for review -- pushed as a dismissible alert in
     *  the background (`auto-check`), or handed straight back to the caller
     *  for a manual "Update now". `no-check` sources never reach here.
     *  Returns the files awaiting review from a manual call, else null. */
    async checkGitSource(
      group: SourceGroup,
      pref: UpdatePref,
      manual = false,
    ): Promise<PendingUpdateFile[] | null> {
      const g = group.origin as GitOrigin;
      let latest: { sha: string | null };
      try {
        latest = await fetchRepoLastCommit(g.owner, g.repo, g.branch, {
          force: manual,
        });
      } catch {
        if (manual)
          this.updateAlerts.push({
            key: group.key,
            message: `${group.label}: couldn't check for an update.`,
          });
        return null;
      }
      if (!latest.sha) return null;
      const baseline = this.updateBaselines[group.key];
      if (!baseline) {
        this.updateBaselines[group.key] = latest.sha;
        persistUpdateBaselines(this.updateBaselines);
        if (manual)
          this.updateAlerts.push({
            key: group.key,
            message: `${group.label}: already up to date.`,
          });
        return null;
      }
      if (baseline === latest.sha) {
        if (manual)
          this.updateAlerts.push({
            key: group.key,
            message: `${group.label}: already up to date.`,
          });
        return null;
      }

      if (pref === "auto-update" && !manual) {
        const entries: { name: string; text: string; origin: Origin }[] = [];
        for (const name of group.files) {
          try {
            entries.push({
              name,
              text: await fromOrigin(name, group.origin),
              origin: group.origin,
            });
          } catch {
            /* one file's fetch failing shouldn't block the rest */
          }
        }
        await this.applyUpdates(entries);
        this.updateBaselines[group.key] = latest.sha;
        persistUpdateBaselines(this.updateBaselines);
        this.updateAlerts.push({
          key: group.key,
          message: `${group.label}: updated to the latest commit.`,
        });
        return null;
      }

      // The commit moved -- gather per-file diffs rather than assuming
      // every file in the group actually changed.
      const review: PendingUpdateFile[] = [];
      for (const name of group.files) {
        try {
          const incoming = await fromOrigin(name, group.origin);
          const current = this.find(name)?.text ?? "";
          if (incoming !== current)
            review.push({
              name,
              origin: group.origin,
              current,
              incoming,
              baselineKey: group.key,
              baselineValue: latest.sha,
            });
        } catch {
          /* one file's fetch failing shouldn't block the rest */
        }
      }
      if (!review.length) {
        // The commit moved but touched nothing this KB has loaded.
        this.updateBaselines[group.key] = latest.sha;
        persistUpdateBaselines(this.updateBaselines);
        if (manual)
          this.updateAlerts.push({
            key: group.key,
            message: `${group.label}: already up to date.`,
          });
        return null;
      }
      if (manual) return review;
      this.updateAlerts.push({
        key: group.key,
        message: `${group.label}: a new commit is available (${latest.sha.slice(0, 7)}).`,
        review,
      });
      return null;
    },

    /** One `url` source: no upstream version signal exists for a plain file,
     *  so freshness is a content hash instead -- re-fetched and compared
     *  against the hash recorded the first time this exact file was seen
     *  (never the CURRENT text's hash, which a local edit could have
     *  changed independently of upstream). Same review-vs-apply split as
     *  `checkGitSource`; returns the files awaiting review from a manual
     *  call, else null. */
    async checkUrlSource(
      group: SourceGroup,
      pref: UpdatePref,
      manual = false,
    ): Promise<PendingUpdateFile[] | null> {
      const origin = group.origin as RemoteOrigin;
      const review: PendingUpdateFile[] = [];
      for (const name of group.files) {
        const key = `url:${name}`;
        let baseline = this.updateBaselines[key];
        if (!baseline) {
          // Nothing was recorded before this feature existed (or before this
          // file was first ingested this session) -- the text already
          // loaded IS "the original" as far as the user's concerned. Hash
          // THAT rather than a fresh network fetch: a fresh fetch could
          // already be a newer upstream version, which would silently
          // adopt it as the baseline and mask a change that happened
          // before this check ever ran.
          const current = this.find(name, "url")?.text;
          if (current === undefined) continue;
          const currentHash = await blobSha(current);
          if (!currentHash) continue; // no SubtleCrypto: freshness can't be judged
          this.updateBaselines[key] = currentHash;
          persistUpdateBaselines(this.updateBaselines);
          baseline = currentHash;
        }
        let incoming: string;
        try {
          incoming = await fetchText(origin.url || name);
        } catch {
          if (manual)
            this.updateAlerts.push({
              key,
              message: `${name}: couldn't check for an update.`,
            });
          continue;
        }
        const hash = await blobSha(incoming);
        if (!hash) continue; // no SubtleCrypto: freshness can't be judged
        if (baseline === hash) {
          if (manual)
            this.updateAlerts.push({
              key,
              message: `${name}: already up to date.`,
            });
          continue;
        }

        // An unpushed local edit is never silently replaced: the change falls
        // through to the review path below, where the user chooses.
        const edited = Boolean(useChangesStore().record(name, "url"));
        if (pref === "auto-update" && !manual && !edited) {
          await this.applyUpdates([{ name, text: incoming, origin }]);
          this.updateBaselines[key] = hash;
          persistUpdateBaselines(this.updateBaselines);
          this.updateAlerts.push({ key, message: `${name}: updated.` });
          continue;
        }

        const current = this.find(name, "url")?.text ?? "";
        if (current === incoming) {
          // The hash moved from what's recorded, but what's actually loaded
          // already matches it -- nothing to show.
          this.updateBaselines[key] = hash;
          persistUpdateBaselines(this.updateBaselines);
          if (manual)
            this.updateAlerts.push({
              key,
              message: `${name}: already up to date.`,
            });
          continue;
        }
        const file: PendingUpdateFile = {
          name,
          origin,
          current,
          incoming,
          baselineKey: key,
          baselineValue: hash,
        };
        if (manual) {
          review.push(file);
          continue;
        }
        this.updateAlerts.push({
          key,
          message: `${name}: a new version is available.`,
          review: [file],
        });
      }
      return manual && review.length ? review : null;
    },

    /** Check every `sumo`/`url` source whose preference isn't `no-check`,
     *  applying or alerting per source as its preference dictates. Run once
     *  in the background after boot; never blocks the app becoming usable. */
    async checkForUpdates() {
      for (const group of this.sourceGroups) {
        if (group.origin.kind === "file") continue;
        const pref = this.prefFor(group.origin);
        if (pref === "no-check") continue;
        if (group.origin.kind === "sumo")
          await this.checkGitSource(group, pref);
        else await this.checkUrlSource(group, pref);
      }
    },

    /** Check one source right now, regardless of its stored preference
     *  (including `no-check`) -- the Sources card's "Update now" button.
     *  Never applies silently: returns the changed files for the caller to
     *  open in `UpdatePreviewDialog`, or null if there's nothing to review
     *  (already up to date, or the check failed -- either way an alert was
     *  already pushed). Does not change the stored preference. */
    async updateNow(group: SourceGroup): Promise<PendingUpdateFile[] | null> {
      const pref = this.prefFor(group.origin);
      if (group.origin.kind === "sumo")
        return await this.checkGitSource(group, pref, true);
      if (group.origin.kind === "url")
        return await this.checkUrlSource(group, pref, true);
      return null;
    },

    /** Apply one reviewed file's incoming text -- `UpdatePreviewDialog`'s
     *  "Apply". */
    async applyReviewFile(f: PendingUpdateFile) {
      await this.applyUpdates([
        { name: f.name, text: f.incoming, origin: f.origin },
      ]);
    },

    /** Advance every reviewed file's baseline once `UpdatePreviewDialog`
     *  closes, whether each was applied or skipped -- mirrors `DiffDialog`'s
     *  "keep mine" acknowledgement, so a deliberately-skipped file doesn't
     *  re-alert forever. Dismissing the alert banner WITHOUT reviewing does
     *  not call this -- that leaves the baseline alone so it re-alerts. */
    acknowledgeReview(files: PendingUpdateFile[]) {
      for (const f of files)
        this.updateBaselines[f.baselineKey] = f.baselineValue;
      persistUpdateBaselines(this.updateBaselines);
    },
  },
});
