/**
 * The page's mutable state, for the values more than one module both reads and
 * writes. Anything only one module touches stays a local `let` in that module.
 *
 * A single object rather than exported `let`s: an ES module's live bindings are
 * read-only for importers, so a shared value that any importer has to ASSIGN
 * (`constituents`, `uiLanguage`, …) needs one owning object either way.
 */

import {
  MERGE,
  MIDLEVEL,
  SUMO_FILE_SETTING,
  WORDNET_ENABLED_KEY,
} from "./constants.ts";

export const state = {
  /** [{ name, text, origin }] — the page's source of truth for what is loaded. */
  constituents: [],

  /** [{ name, origin }] mirrored to localStorage — what the next boot reloads. */
  savedConstituents: JSON.parse(
    localStorage.getItem(SUMO_FILE_SETTING) || "null",
  ) || [
    { name: MERGE, origin: "sumo" },
    { name: MIDLEVEL, origin: "sumo" },
  ],

  /** Whether the WordNet synonym-search lexicon should be loaded, mirrored
   *  to localStorage — see boot.ts's loadWordNetIntoWorker / tabs/kb-tab.ts's
   *  WordNet panel. Defaults on; only the literal string 'false' turns it off. */
  wordnetEnabled: localStorage.getItem(WORDNET_ENABLED_KEY) !== "false",

  /** [{ name, size }] the WordNet mapping files last fetched this session (byte
   *  sizes), for the KB tab's WordNet panel. Empty before the first fetch
   *  resolves, or permanently while wordnetEnabled is false. */
  wordnetFiles: [],

  /** The KB's validation findings, as of the last validate(). */
  diagnostics: [],

  /** Cached list of *.kif paths in the upstream repo, or null before first load. */
  sumoCatalog: null,

  /** Header selector: the language for term/format rendering and NL paraphrases. */
  uiLanguage: "EnglishLanguage",

  /** Settings toggle: render NL paraphrase variables as generic noun phrases
   *  ("an entity" / "the entity") instead of `?VarName`. */
  genericVars: false,

  /** OPFS root, opened once at boot. */
  opfsRoot: null,

  /** True while promote + validate is in flight (the post-processing window). */
  promoting: false,

  /** The Monaco namespace, once the CDN load resolves. */
  monaco: null,

  /** The Edit tab's editor instance. */
  monacoEditor: null,

  /** { name, origin } of the file being edited, or null for an unsaved "new file". */
  editCurrentFile: null,
};
