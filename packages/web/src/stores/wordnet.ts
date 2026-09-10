/** The WordNet-SUMO synonym lexicon: fetching the upstream mapping files and
 *  installing/clearing them in the worker's session. Same repo/ref as the
 *  KIF constituents (see `WORDNET_DIR`), so a WordNet fetch tracks whatever
 *  branch the KIF fetch does. */

import { defineStore } from "pinia";
import { toRaw } from "vue";
import { call } from "../services/sigma";
import { fetchText } from "../services/sources";
import { rawUrl, WORDNET_DIR, WORDNET_ENABLED_KEY } from "../constants";

/** The four required mapping files, in `(file name, pos)` order -- mirrors
 *  `MAPPING_FILES` in crates/sdk/src/lexicon.rs. */
const MAPPING_FILES: [string, string][] = [
  ["WordNetMappings30-noun.txt", "noun"],
  ["WordNetMappings30-verb.txt", "verb"],
  ["WordNetMappings30-adj.txt", "adj"],
  ["WordNetMappings30-adv.txt", "adv"],
];
const INDEX_SENSE = "index.sense";
const EXC_FILES = ["noun.exc", "verb.exc"];

/** Byte size of `text` as UTF-8 -- `.length` undercounts any non-ASCII
 *  content (rare in these files, but the glosses do carry the occasional
 *  accented name or symbol). */
function byteSize(text: string): number {
  return new TextEncoder().encode(text).length;
}

export interface WordNetFile {
  name: string;
  size: number;
}

interface WordNetPayload {
  noun: string;
  verb: string;
  adj: string;
  adv: string;
  indexSense: string | null;
  exceptions: string;
}

export const useWordNetStore = defineStore("wordnet", {
  state: () => ({
    /** Persisted on/off for the synonym-search lexicon. Defaults on; only
     *  the literal string 'false' turns it off. */
    enabled: localStorage.getItem(WORDNET_ENABLED_KEY) !== "false",
    /** [{ name, size }] the mapping files last fetched this session, for the
     *  KB tab's WordNet panel. Empty before the first fetch resolves. */
    files: [] as WordNetFile[],
    /** The fetched-and-parsed payload, cached for the life of the page so a
     *  later reinstall (session rebuild, re-enabling) never re-fetches the
     *  ~23 MB of mapping text. */
    payload: null as WordNetPayload | null,
  }),
  actions: {
    setEnabled(enabled: boolean) {
      this.enabled = enabled;
      try {
        localStorage.setItem(WORDNET_ENABLED_KEY, String(enabled));
      } catch {
        /* private mode */
      }
    },

    /** Fetch (or reuse the cached fetch of) the WordNet-SUMO mapping files
     *  and populate `files` with their sizes. */
    async fetchPayload(): Promise<WordNetPayload> {
      if (this.payload) return this.payload;
      // Concurrent fetches settle in arrival order, not declaration order --
      // Promise.all preserves the latter, so `files` stays in a stable order.
      const texts = await Promise.all(
        MAPPING_FILES.map(([name, pos]) =>
          fetchText(rawUrl(`${WORDNET_DIR}/${name}`)).then(
            (text) => [text, pos, name] as const,
          ),
        ),
      );
      const mappingFiles = texts.map(([text, , name]) => ({
        name,
        size: byteSize(text),
      }));

      const indexSenseText = await fetchText(
        rawUrl(`${WORDNET_DIR}/${INDEX_SENSE}`),
      ).catch(() => null);
      const indexSenseFile =
        indexSenseText === null
          ? []
          : [{ name: INDEX_SENSE, size: byteSize(indexSenseText) }];

      const excResults = await Promise.all(
        EXC_FILES.map((name) =>
          fetchText(rawUrl(`${WORDNET_DIR}/${name}`))
            .then((text) => ({ name, text }))
            .catch(() => null),
        ),
      );
      const excHits = excResults.filter(
        (r): r is { name: string; text: string } => r !== null,
      );
      const excFiles = excHits.map(({ name, text }) => ({
        name,
        size: byteSize(text),
      }));
      const exceptions = excHits.map((r) => r.text).join("\n");

      const byPos = Object.fromEntries(texts.map(([text, pos]) => [pos, text]));
      this.payload = {
        noun: byPos.noun,
        verb: byPos.verb,
        adj: byPos.adj,
        adv: byPos.adv,
        indexSense: indexSenseText,
        exceptions,
      };
      this.files = [...mappingFiles, ...indexSenseFile, ...excFiles];
      return this.payload;
    },

    /** Fetch + install the lexicon into the worker's current session.
     *  Best-effort: WordNet synonym expansion is an optional search
     *  enhancement, never a hard dependency of a usable KB, so a fetch/
     *  install failure is logged and swallowed. No-op when disabled. */
    async install() {
      if (!this.enabled) return;
      try {
        const payload = await this.fetchPayload();
        // A reactive proxy cannot be structured-cloned across postMessage.
        await call("loadWordNet", { ...toRaw(payload) });
      } catch (e) {
        console.warn("WordNet lexicon install failed:", e);
      }
    },

    /** Drop the currently installed lexicon from the worker's session. */
    async clear() {
      await call("clearWordNet");
    },
  },
});
