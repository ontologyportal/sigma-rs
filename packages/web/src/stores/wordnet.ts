/** The WordNet-SUMO synonym lexicon: fetching the upstream mapping files and
 *  installing/clearing them in the worker's session. Same repo/ref as the
 *  KIF constituents (see `WORDNET_DIR`), so a WordNet fetch tracks whatever
 *  branch the KIF fetch does.
 *
 *  The four mapping files are also tracked changes (see stores/changes.ts),
 *  named by repo path: the WordNet tab rewrites a synset's anchors here, the
 *  saved text outranks upstream on the next fetch, and the Contribute panel
 *  proposes it like any edited constituent. */

import { defineStore } from "pinia";
import { toRaw } from "vue";
import { call } from "../services/sigma";
import { fetchText } from "../services/sources";
import { rawUrl, WORDNET_DIR, WORDNET_ENABLED_KEY } from "../constants";
import { GitOrigin } from "../models/Origin";
import { useChangesStore } from "./changes";
import {
  rewriteAnchors,
  synsetKey,
  type Anchor,
} from "../utils/wordnetMappings";
import type { WordNetFiles } from "sigmakee/sdk";

/** The four required mapping files, in `(file name, pos)` order -- mirrors
 *  `MAPPING_FILES` in crates/sdk/src/lexicon.rs. */
type MappingPos = "noun" | "verb" | "adj" | "adv";
const MAPPING_FILES: [string, MappingPos][] = [
  ["WordNetMappings30-noun.txt", "noun"],
  ["WordNetMappings30-verb.txt", "verb"],
  ["WordNetMappings30-adj.txt", "adj"],
  ["WordNetMappings30-adv.txt", "adv"],
];
const INDEX_SENSE = "index.sense";
const EXC_FILES = ["noun.exc", "verb.exc"];
let installation: Promise<void> | null = null;
/** How long after the last mapping edit the worker's lexicon is rebuilt. */
const RELOAD_DELAY_MS = 800;
let reloadTimer: ReturnType<typeof setTimeout> | null = null;
let saving: Promise<unknown> = Promise.resolve();
/** Each mapping file's text split into lines, kept in step with edits so a
 *  one-line rewrite never re-splits a 15 MB file. */
const lineCache = new Map<string, { text: string; lines: string[] }>();

/** A mapping file's repo path -- also its tracked-change name. */
export const wordnetPath = (file: string) => `${WORDNET_DIR}/${file}`;

const fileByPath = (path: string) =>
  MAPPING_FILES.find(([file]) => wordnetPath(file) === path);

/** Whether `name` is one of the four tracked mapping files. */
export const isWordNetFile = (name: string) => !!fileByPath(name);

function linesOf(file: string, text: string): string[] {
  const cached = lineCache.get(file);
  if (cached?.text === text) return cached.lines;
  const lines = text.split("\n");
  lineCache.set(file, { text, lines });
  return lines;
}

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

/** The mapping-file texts the `loadWordNet` RPC installs -- the SDK's own
 *  shape, so the payload this store assembles is the payload the worker
 *  takes. */
type WordNetPayload = WordNetFiles;

export const useWordNetStore = defineStore("wordnet", {
  state: () => ({
    /** Persisted on/off for the synonym-search lexicon. Defaults on; only
     *  the literal string 'false' turns it off. */
    enabled: localStorage.getItem(WORDNET_ENABLED_KEY) !== "false",
    /** [{ name, size }] the mapping files last fetched this session, for the
     *  Knowledge base tab's WordNet view. Empty before the first fetch resolves. */
    files: [] as WordNetFile[],
    /** The fetched-and-parsed payload, cached for the life of the page so a
     *  later reinstall (session rebuild, re-enabling) never re-fetches the
     *  ~23 MB of mapping text. */
    payload: null as WordNetPayload | null,
    installed: false,
    loading: false,
    /** Upstream text per mapping file name, for files fetched from upstream
     *  this session -- the base a first edit is stamped with. */
    pristine: {} as Record<string, string>,
    /** Bumped on every mapping edit, for views that derive from the texts. */
    revision: 0,
  }),
  getters: {
    /** The mapping files as tracked files, once fetched. */
    trackedFiles(state): { name: string; origin: GitOrigin; text: string }[] {
      const payload = state.payload;
      if (!payload) return [];
      const origin = GitOrigin.default();
      return MAPPING_FILES.map(([file, pos]) => ({
        name: wordnetPath(file),
        origin,
        text: payload[pos],
      }));
    },
  },
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
      const changes = useChangesStore();
      const pristine: Record<string, string> = {};
      const texts = await Promise.all(
        MAPPING_FILES.map(async ([name, pos]) => {
          const edited = await changes.readEdit(wordnetPath(name), "sumo");
          if (edited !== null) return [edited, pos, name] as const;
          const text = await fetchText(rawUrl(wordnetPath(name)));
          pristine[name] = text;
          return [text, pos, name] as const;
        }),
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
        indexSense: indexSenseText ?? undefined,
        exceptions,
      };
      this.pristine = pristine;
      this.files = [...mappingFiles, ...indexSenseFile, ...excFiles];
      return this.payload;
    },

    /** Fetch + install the lexicon into the worker's current session.
     *  Best-effort: WordNet synonym expansion is an optional search
     *  enhancement, never a hard dependency of a usable KB, so a fetch/
     *  install failure is logged and swallowed. No-op when disabled. */
    async install() {
      if (!this.enabled || this.installed) return;
      if (installation) return installation;
      this.loading = true;
      installation = (async () => {
        try {
          const payload = await this.fetchPayload();
          if (!this.enabled) return;
          // A reactive proxy cannot be structured-cloned across postMessage.
          await call("loadWordNet", { ...toRaw(payload) });
          this.installed = true;
        } catch (e) {
          console.warn("WordNet lexicon install failed:", e);
        } finally {
          this.loading = false;
          installation = null;
        }
      })();
      return installation;
    },

    /** Restore a used lexicon after session replacement, without first-use downloads. */
    async reinstall() {
      this.installed = false;
      if (this.payload) await this.install();
    },

    /** Drop the currently installed lexicon from the worker's session. */
    async clear() {
      await installation;
      if (reloadTimer) clearTimeout(reloadTimer);
      reloadTimer = null;
      await call("clearWordNet");
      this.installed = false;
    },

    /** A mapping file's current text by repo path, or null if not fetched. */
    fileText(path: string): string | null {
      const entry = fileByPath(path);
      return entry && this.payload ? this.payload[entry[1]] : null;
    },

    /** Line `line` (1-based) of mapping file `file` (a
     *  `WordNetMappings30-*.txt` name), or null if not loaded. */
    lineAt(file: string, line: number): string | null {
      const text = this.fileText(wordnetPath(file));
      return text === null ? null : (linesOf(file, text)[line - 1] ?? null);
    },

    /** Upstream's text for a mapping file: the copy fetched this session, or
     *  a fresh read when the session started from a saved edit. */
    async baseText(path: string): Promise<string> {
      const entry = fileByPath(path);
      const fetched = entry ? this.pristine[entry[0]] : undefined;
      return fetched ?? useChangesStore().fetchUpstreamText(path);
    },

    /**
     * Replace the SUMO anchors of the synset at `line` of mapping file `file`
     * (a `WordNetMappings30-*.txt` name, as `WordNetSense.file` reports it)
     * and save. `key` is the synset's id (`02084071-n`); a line that no
     * longer holds it -- the file was replaced since the sense was looked up
     * -- is refused rather than rewriting the wrong record.
     */
    async setMappings(
      file: string,
      line: number,
      key: string,
      anchors: Anchor[],
    ): Promise<void> {
      const path = wordnetPath(file);
      const text = this.fileText(path);
      if (text === null) throw new Error(`${file} is not loaded`);
      const lines = linesOf(file, text);
      const old = lines[line - 1];
      if (old === undefined || synsetKey(old) !== key)
        throw new Error(
          `${file} changed since ${key} was looked up -- search again.`,
        );
      const updated = rewriteAnchors(old, anchors);
      if (updated === old) return;
      lines[line - 1] = updated;
      const next = lines.join("\n");
      lineCache.set(file, { text: next, lines });
      await this.saveFile(path, next);
    },

    /**
     * Save a mapping file's full text: update the payload, record it as a
     * tracked change (stored in OPFS, so it survives a reload), and rebuild
     * the worker's lexicon once edits pause. Saves are serialized so two
     * quick edits never interleave their OPFS writes.
     */
    async saveFile(path: string, text: string): Promise<void> {
      const entry = fileByPath(path);
      if (!entry || !this.payload)
        throw new Error(`${path} is not a loaded WordNet mapping file`);
      const [file, pos] = entry;
      this.payload[pos] = text;
      this.revision += 1;
      const pristine = this.pristine[file];
      const save = saving.then(() =>
        useChangesStore().recordSave(path, "sumo", text, pristine),
      );
      saving = save.catch(() => {});
      this.scheduleReload();
      await save;
    },

    /** Rebuild the worker's lexicon from the current texts after edits pause. */
    scheduleReload() {
      if (reloadTimer) clearTimeout(reloadTimer);
      reloadTimer = setTimeout(() => {
        reloadTimer = null;
        if (!this.installed || !this.payload) return;
        call("loadWordNet", { ...toRaw(this.payload) }).catch((e) =>
          console.warn("WordNet lexicon reload failed:", e),
        );
      }, RELOAD_DELAY_MS);
    },
  },
});
