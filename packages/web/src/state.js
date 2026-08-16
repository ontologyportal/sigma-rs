/**
 * The page's mutable state, for the values more than one module both reads and
 * writes. Anything only one module touches stays a local `let` in that module.
 *
 * A single object rather than exported `let`s: an ES module's live bindings are
 * read-only for importers, so a shared value that any importer has to ASSIGN
 * (`constituents`, `uiLanguage`, …) needs one owning object either way.
 */

import { MERGE, MIDLEVEL, SUMO_FILE_SETTING, GH_TOKEN_KEY } from './constants.js';

export const state = {
  /** [{ name, text, origin }] — the page's source of truth for what is loaded. */
  constituents: [],

  /** [{ name, origin }] mirrored to localStorage — what the next boot reloads. */
  savedConstituents: JSON.parse(localStorage.getItem(SUMO_FILE_SETTING) || 'null') || [
    { name: MERGE, origin: 'sumo' },
    { name: MIDLEVEL, origin: 'sumo' },
  ],

  /** The KB's validation findings, as of the last validate(). */
  diagnostics: [],

  /** Cached list of *.kif paths in the upstream repo, or null before first load. */
  sumoCatalog: null,

  /** Header selector: the language for term/format rendering and NL paraphrases. */
  uiLanguage: 'EnglishLanguage',

  /** OPFS root, opened once at boot. */
  opfsRoot: null,

  /** True while promote + validate is in flight (the post-processing window). */
  promoting: false,

  /**
   * One credential for every GitHub API request: catalog/commit reads as well
   * as contribution writes. It remains session-only unless "remember" is
   * explicitly enabled in the Edit contribution panel.
   */
  ghToken: localStorage.getItem(GH_TOKEN_KEY) || '',

  /** The Monaco namespace, once the CDN load resolves. */
  monaco: null,

  /** The Edit tab's editor instance. */
  monacoEditor: null,

  /** { name, origin } of the file being edited, or null for an unsaved "new file". */
  editCurrentFile: null,
};
