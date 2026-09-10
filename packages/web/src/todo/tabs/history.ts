/**
 * History tab state: a file's commit timeline from GitHub.
 *
 * Plain GitHub REST — the same public API the Knowledge base tab already uses
 * for the file catalog, so no token is required. That caps an unauthenticated
 * visitor at 60 requests/hour per IP, so results are cached per file for the
 * session and only refetched on an explicit Refresh.
 *
 * Only what `ensureHistory` (the entry point `router.ts` calls on tab-enter)
 * itself needs lives here; the UI-triggered bits (Refresh, picking a file --
 * see `refreshHistory`/`selectHistoryFile` in the old history) live in
 * `HistoryTab.vue` directly, since nothing outside that component calls them.
 */

import { ref } from 'vue';
import { SUMO } from '../constants.ts';
import { state } from '../state.ts';
import { githubApi } from '../github-api.ts';

const historyCache = new Map();   // file -> commits[]
let historyShown = null;          // file currently rendered, so re-entry is free

export const historyFile = ref('');
export const historyStatus = ref('');
export const historyCommits = ref([]);
export const historyError = ref('');

/** Only `sumo`-origin constituents exist on GitHub; uploads/URLs have no
 *  history. `state` isn't a reactive object (see state.ts) -- a `computed`
 *  here would read `state.constituents` once and cache it forever, so this
 *  stays a plain function, re-read fresh by the template on every render the
 *  component's own reactive state (historyFile et al.) triggers. */
export function historyPickerOptions() {
  return state.constituents.filter((c) => c.origin === 'sumo').map((c) => c.name);
}

async function fetchCommits(file) {
  if (historyCache.has(file)) return historyCache.get(file);
  const commits = await githubApi(`/repos/${SUMO.owner}/${SUMO.repo}/commits`
    + `?path=${encodeURIComponent(file)}&per_page=30`);
  historyCache.set(file, commits);
  return commits;
}

export async function loadHistory(file, { force = false } = {}) {
  historyFile.value = file || '';
  if (!file) { historyCommits.value = []; historyError.value = ''; historyStatus.value = ''; historyShown = null; return; }
  if (!force && file === historyShown && historyCache.has(file)) return;  // already on screen
  if (force) historyCache.delete(file);
  historyShown = file;
  historyStatus.value = 'loading…';
  historyCommits.value = [];
  historyError.value = '';
  try {
    const commits = await fetchCommits(file);
    if (historyShown !== file) return;   // a newer request won
    historyStatus.value = `${commits.length} commit${commits.length === 1 ? '' : 's'}`;
    historyCommits.value = commits;
  } catch (e) {
    historyStatus.value = '';
    historyShown = null;                 // let a retry re-fetch
    historyError.value = String(e && e.message || e);
  }
}

/** Open the History tab on `file` (or whatever the picker already has). */
export function ensureHistory(file) {
  const files = historyPickerOptions();
  const target = file && files.includes(file) ? file : (files.includes(historyFile.value) ? historyFile.value : files[0]);
  loadHistory(target || '');
}
