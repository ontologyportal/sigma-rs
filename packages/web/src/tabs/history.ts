/**
 * History tab: a file's commit timeline from GitHub.
 *
 * Plain GitHub REST — the same public API the Knowledge base tab already uses
 * for the file catalog, so no token is required. That caps an unauthenticated
 * visitor at 60 requests/hour per IP, so results are cached per file for the
 * session and only refetched on an explicit Refresh.
 */

import { SUMO } from '../constants.ts';
import { state } from '../state.ts';
import { $, esc } from '../dom.ts';
import { githubApi } from '../github-api.ts';
import { updateParams } from '../router.ts';

const historyCache = new Map();   // file -> commits[]
let historyShown = null;          // file currently rendered, so re-entry is free

/** Only `sumo`-origin constituents exist on GitHub; uploads/URLs have no history. */
function populateHistoryPicker() {
  const sel = $('historyPicker');
  if (!sel) return;
  const current = sel.value;
  const files = state.constituents.filter((c) => c.origin === 'sumo').map((c) => c.name);
  sel.innerHTML = files.length
    ? files.map((f) => `<option value="${esc(f)}">${esc(f)}</option>`).join('')
    : '<option value="">(no SUMO-sourced files loaded)</option>';
  if (files.includes(current)) sel.value = current;
}

async function fetchCommits(file) {
  if (historyCache.has(file)) return historyCache.get(file);
  const commits = await githubApi(`/repos/${SUMO.owner}/${SUMO.repo}/commits`
    + `?path=${encodeURIComponent(file)}&per_page=30`);
  historyCache.set(file, commits);
  return commits;
}

function renderHistory(file, commits) {
  const list = $('historyList');
  if (!commits.length) {
    list.innerHTML = `<div class="card hint">No commits found for <code>${esc(file)}</code>.</div>`;
    return;
  }
  const rows = commits.map((c) => {
    const msg  = (c.commit?.message || '(no message)').split('\n')[0];
    const who  = c.commit?.author?.name || c.author?.login || 'unknown';
    const iso  = c.commit?.author?.date;
    const when = iso ? new Date(iso).toLocaleDateString(undefined,
      { year: 'numeric', month: 'short', day: 'numeric' }) : '';
    return `<li>
      <div class="commit-msg"><a href="${esc(c.html_url || '#')}" target="_blank" rel="noopener">${esc(msg)}</a></div>
      <div class="commit-meta">${esc(who)}${when ? ` · ${esc(when)}` : ''} · <span class="sha">${esc((c.sha || '').slice(0, 7))}</span></div>
    </li>`;
  }).join('');
  // The API view is capped at one page; send people to GitHub for the full log.
  const all = `https://github.com/${SUMO.owner}/${SUMO.repo}/commits/${SUMO.ref}/${encodeURI(file)}`;
  list.innerHTML = `<div class="card">
    <ol class="timeline">${rows}</ol>
    <div class="hint" style="margin-top:12px; padding-top:10px; border-top:1px solid var(--line)">
      Showing the ${commits.length} most recent —
      <a href="${esc(all)}" target="_blank" rel="noopener">full commit history for ${esc(file)} on GitHub ↗</a>
    </div>
  </div>`;
}

async function loadHistory(file, { force = false } = {}) {
  const list = $('historyList');
  if (!file) { list.innerHTML = ''; $('historyStatus').textContent = ''; historyShown = null; return; }
  if (!force && file === historyShown && historyCache.has(file)) return;  // already on screen
  if (force) historyCache.delete(file);
  historyShown = file;
  $('historyStatus').textContent = 'loading…';
  list.innerHTML = '';
  try {
    const commits = await fetchCommits(file);
    if (historyShown !== file) return;   // a newer request won
    $('historyStatus').textContent = `${commits.length} commit${commits.length === 1 ? '' : 's'}`;
    renderHistory(file, commits);
  } catch (e) {
    $('historyStatus').textContent = '';
    historyShown = null;                 // let a retry re-fetch
    list.innerHTML = `<div class="card hint" style="color:var(--bad)">${esc(String(e && e.message || e))}</div>`;
  }
}

/** Open the History tab on `file` (or whatever the picker already has). */
export function ensureHistory(file) {
  populateHistoryPicker();
  const sel = $('historyPicker');
  if (file && [...sel.options].some((o) => o.value === file)) sel.value = file;
  loadHistory(sel.value);
}

$('historyPicker').addEventListener('change', () => {
  const f = $('historyPicker').value;
  updateParams({ file: f });
  loadHistory(f);
});
$('historyRefresh').onclick = () => loadHistory($('historyPicker').value, { force: true });
