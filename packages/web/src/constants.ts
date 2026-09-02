/**
 * Values shared by more than one module and owned by none of them: the upstream
 * repository coordinates, the localStorage keys, and the tab groups the router
 * and the post-processing window both reason about.
 *
 * This module imports nothing, so it is always evaluated first and can be
 * imported from anywhere without introducing a cycle.
 */

export const SUMO = { owner: 'ontologyportal', repo: 'sumo', ref: 'HEAD' };
export const MERGE = 'Merge.kif';                     // the foundational ontology, loaded on startup
export const MIDLEVEL = 'Mid-level-ontology.kif';     // also loaded on startup

export const rawUrl = (path) => `https://raw.githubusercontent.com/${SUMO.owner}/${SUMO.repo}/${SUMO.ref}/${path}`;

/** The WordNet-SUMO mapping directory -- same repo/ref as MERGE/MIDLEVEL
 *  above, so `rawUrl(\`${WORDNET_DIR}/<name>\`)` resolves the same way the
 *  main KIF constituents do (see wordnet.ts). */
export const WORDNET_DIR = 'WordNetMappings';

/** This app's own repository — the bug-report link's target. */
export const APP_REPO = { owner: 'ontologyportal', repo: 'sigma-rs' };

export const SUMO_FILE_SETTING = 'sumoFiles';
export const TQ_SETTING = 'sumoTests';
export const EDITS_KEY = 'sumoBrowserEdits';
export const THEME_KEY = 'sumoBrowserTheme';
export const SEEN_VERSION_KEY = 'sumoBrowserSeenVersion';

/** Tabs that need the KB axiomatized; greyed while a promote is in flight. */
export const PROMOTE_TABS = ['diagnostics', 'prover', 'audit'];
