/**
 * Values shared by more than one module and owned by none of them: the upstream
 * repository coordinates, the localStorage keys, and the tab groups the router
 * and the post-processing window both reason about.
 *
 * This module imports nothing, so it is always evaluated first and can be
 * imported from anywhere without introducing a cycle.
 */

export const SUMO = {
  owner: "ontologyportal",
  repo: "sumo",
  branch: "master",
  ref: "HEAD",
};
export const MERGE = "Merge.kif"; // the foundational ontology, loaded on startup
export const MIDLEVEL = "Mid-level-ontology.kif"; // also loaded on startup

export const rawUrl = (path: string) =>
  `https://raw.githubusercontent.com/${SUMO.owner}/${SUMO.repo}/${SUMO.ref}/${path}`;

/** The WordNet-SUMO mapping directory -- same repo/ref as MERGE/MIDLEVEL
 *  above, so `rawUrl(\`${WORDNET_DIR}/<name>\`)` resolves the same way the
 *  main KIF constituents do (see wordnet.ts). */
export const WORDNET_DIR = "WordNetMappings";

/** This app's own repository — the bug-report link's target. */
export const APP_REPO = { owner: "ontologyportal", repo: "sigma-rs" };

export const SUMO_FILE_SETTING = "sumoFiles";
export const TQ_SETTING = "sumoTests";
/** The constituent library: registered repos + local/URL entries. */
export const LIBRARY_KEY = "sumoLibrary";
export const EDITS_KEY = "sumoBrowserEdits";
export const THEME_KEY = "sumoBrowserTheme";
export const LAYOUT_KEY = "sumoBrowserLayout";
export const SEEN_VERSION_KEY = "sumoBrowserSeenVersion";
/** Persisted on/off for the WordNet synonym-search lexicon -- absent or
 *  anything but the literal string `'false'` means enabled (see state.ts). */
export const WORDNET_ENABLED_KEY = "sumoBrowserWordNetEnabled";
/** Per-source update preference (auto-update/auto-check/no-check), keyed by
 *  origin id -- see `useKBStore.updatePrefs`. */
export const UPDATE_PREFS_KEY = "sumoBrowserUpdatePrefs";
/** The upstream commit (git sources) or content hash (URL sources) last
 *  seen for each source, checked against on the next update pass -- see
 *  `useKBStore.updateBaselines`. */
export const UPDATE_BASELINES_KEY = "sumoBrowserUpdateBaselines";

/** Tabs that need the KB axiomatized; greyed while a promote is in flight. */
export const PROMOTE_TABS: readonly TabName[] = [
  "diagnostics",
  "prover",
  "problems",
  "audit",
];

export const AVAILABLE_LAYOUTS = <const>["comfortable", "classic"];
/** Below this width, "classic" mode's multi-column Row/Col grids (Row
 *  defaults to `wrap: "nowrap"`) no longer have room to render usably, so
 *  the shell store forces "comfortable" regardless of the saved preference
 *  (see `useShellStore.effectiveLayout`). Same breakpoint TabNav already
 *  treats as "phone width" for its own tier switch, so the app agrees on
 *  one definition of "narrow". */
export const NARROW_LAYOUT_QUERY = "(max-width: 1000px)";

export const BASE = import.meta.env.BASE_URL;

export const TABS = [
  "browse",
  "kb",
  "problems",
  "diagnostics",
  "prover",
  "audit",
  "edit",
] as const;
export type TabName = (typeof TABS)[number];
