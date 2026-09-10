/** The knowledge base's lifecycle: constituent mutations, and the deferred
 *  promote + validate they all funnel through.
 *
 * Mutations ingest (fast) but do not promote; each one runs `reprocess()`
 * once to promote + validate under the `promoting` flag views can react to. */

import { defineStore } from "pinia";
import { MERGE, SUMO_FILE_SETTING } from "../constants";
import { Constituent } from "../models/Constituent";
import { Origin, GitOrigin, OriginKind, originForKind } from "../models/Origin";
import { call } from "../services/sigma";
import { fromOrigin } from "../services/sources";
import { useWordNetStore } from "./wordnet";

/** What's mirrored to localStorage -- just enough to reconstruct an `Origin`
 *  and refetch its text on the next boot. */
interface SavedConstituent {
  name: string;
  origin: OriginKind;
}

function loadSavedConstituents(): SavedConstituent[] {
  try {
    const raw = JSON.parse(localStorage.getItem(SUMO_FILE_SETTING) || "null");
    if (Array.isArray(raw) && raw.length) return raw;
  } catch {
    /* corrupt value */
  }
  return Constituent.defaults().map((c) => ({ name: c.name, origin: c.origin.kind }));
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
    diagnostics: [] as unknown[],
    /** True while promote + validate is in flight (the post-processing window). */
    promoting: false,
    /** Header selector: the language for term/format rendering and NL paraphrases. */
    uiLanguage: "EnglishLanguage",
    /** Render NL paraphrase variables as generic noun phrases instead of `?VarName`. */
    genericVars: false,
  }),
  getters: {
    isLoaded: (state) => (name: string) => state.constituents.some((c) => c.name === name),
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
      if (this.isLoaded(name)) return { added: false, notices: [`${name}: already loaded`] };
      const { notices } = await call<{ notices: string[] }>("ingest", { name, text });
      this.constituents.push(new Constituent(name, origin, text));
      if (!this.saved.some((c: SavedConstituent) => c.name === name && c.origin === origin.kind)) {
        this.saved.push({ name, origin: origin.kind });
        persistSaved(this.saved);
      }
      return { added: true, notices };
    },

    /** Rebuild the worker session from the currently tracked constituents --
     *  used by remove/reset. */
    async rebuildSession() {
      await call("newSession");
      for (const c of this.constituents) await call("ingest", { name: c.name, text: c.text });
      // newSession() drops any installed WordNet lexicon; reinstall it (from
      // the cached fetch, so this never re-downloads).
      await useWordNetStore().install();
    },

    async remove(name: string, origin: OriginKind = "sumo") {
      this.constituents = this.constituents.filter(
        (c: Constituent) => !(c.name === name && c.origin.kind === origin),
      );
      this.saved = this.saved.filter(
        (c: SavedConstituent) => !(c.name === name && c.origin === origin),
      );
      persistSaved(this.saved);
      await this.rebuildSession();
      await this.reprocess();
    },

    async resetToMerge() {
      const merge = this.constituents.find((c: Constituent) => c.name === MERGE);
      this.constituents = merge ? [merge] : [];
      this.saved = merge ? [{ name: MERGE, origin: merge.origin.kind }] : [];
      persistSaved(this.saved);
      await this.rebuildSession();
      await this.reprocess();
    },

    /** Populate `uiLanguage`'s valid choices from the KB's `NaturalLanguage`
     *  instances, preserving the current choice when it's still valid. Only
     *  meaningful to call where the language list can actually have changed
     *  (boot, promote). */
    async refreshLangSelect() {
      let languages: { symbol: string; label: string }[] | undefined;
      try {
        ({ languages } = await call("naturalLanguages"));
      } catch {
        return;
      }
      if (!languages?.length) return;
      const has = (v: string) => languages!.some((l) => l.symbol === v);
      this.uiLanguage = has(this.uiLanguage)
        ? this.uiLanguage
        : has("EnglishLanguage")
          ? "EnglishLanguage"
          : languages[0].symbol;
    },

    // Promote every ingested constituent into the axiom base, THEN validate
    // once, THEN refresh the language list -- promote and validate are the
    // KB-size-bound steps, validation runs exactly once here.
    async promoteAndValidate() {
      await call("promoteAll", { names: this.constituents.map((c) => c.name) });
      this.diagnostics = (await call<{ diagnostics: unknown[] }>("validate")).diagnostics;
      await this.refreshLangSelect();
    },

    /** Run promote+validate under the `promoting` flag (views grey out
     *  promote-dependent tabs and show a toast while it's set). Re-entrant:
     *  a nested call runs inside the outer window. */
    async reprocess() {
      const outer = !this.promoting;
      if (outer) this.promoting = true;
      try {
        await this.promoteAndValidate();
      } finally {
        if (outer) this.promoting = false;
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
      for (const { name, origin: kind } of this.saved) {
        i += 1;
        onProgress?.(name, i, total);
        const origin = originForKind(kind);
        const text = await fromOrigin(name, origin);
        await this.ingest(name, text, origin);
      }
    },
  },
});
