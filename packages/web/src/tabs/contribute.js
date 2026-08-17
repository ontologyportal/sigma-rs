/**
 * Contribute: the list of unpushed changes and the pull request that carries
 * them upstream to `ontologyportal/sumo`.
 *
 * The panel acts on a SELECTION, not on whichever file the editor happens to
 * show: every tracked change (see changes.js) is listed with a checkbox, and
 * the chosen files go up as one commit. What gets pushed is each file's SAVED
 * text, so a buffer with unsaved edits is refused rather than silently
 * contributing a stale version of a file the user is still typing in.
 *
 * One submission is one branch. A selection spanning two open pull requests
 * has no single branch to commit on and is refused by name; a selection that
 * touches exactly one adds its commit to that branch, which updates the open
 * PR in place instead of opening a competing second one.
 *
 * The token is held in memory for the session; it is only persisted
 * (localStorage) if the user ticks "remember", and never leaves the browser
 * except as an Authorization header to api.github.com.
 */

import { SUMO, GH_TOKEN_KEY } from '../constants.js';
import { state } from '../state.js';
import { $, esc, escAttr, isDarkTheme, togglePanel } from '../dom.js';
import { contributeFiles } from '../github.js';
import { currentGithubToken } from '../github-api.js';
import {
  changeRows, isActionable, markProposed, refreshUpstreamShas, refreshProposals,
  fetchUpstreamText,
} from '../changes.js';
import { updateConstituentText } from '../kb.js';
import { isBufferDirty } from './edit.js';
import { loadMonaco } from '../editor/monaco.js';

function ghSetStatus(text, bad = false) {
  const el = $('ghStatus');
  el.textContent = text;
  el.style.color = bad ? 'var(--bad)' : '';
}

const rowKey = (r) => `${r.origin}:${r.name}`;
const textOf = (r) => state.constituents.find((c) => c.name === r.name && c.origin === r.origin)?.text ?? '';

// Which rows go in the next pull request, which rows the user has already been
// shown a conflict dialog for, and the repo path chosen for each local file.
// All keyed by `origin:name` and all reset when the row disappears.
const selected = new Set();
const known = new Set();
const warned = new Set();
const localPaths = new Map();

/**
 * Seed the selection for rows appearing for the first time: actionable changes
 * default to included, everything else (a file already in review, a local
 * scratch file that may never be meant for upstream) defaults to excluded.
 * An explicit tick by the user is never overridden.
 */
function syncSelection(rows) {
  const live = new Set(rows.map(rowKey));
  for (const set of [selected, known, warned]) {
    for (const k of [...set]) if (!live.has(k)) set.delete(k);
  }
  for (const r of rows) {
    const k = rowKey(r);
    if (known.has(k)) continue;
    known.add(k);
    if (isActionable(r)) selected.add(k);
  }
}

// -- Rendering ----------------------------------------------------------------

const STATE_LABEL = {
  modified: 'modified',
  amended: 'changed since PR',
  review: 'in review',
  local: 'local only',
};

/** Repaint the badge, the change list, and the submit button from the current
 *  records. Called after every save, removal, and pull request. */
export function refreshChangeUi() {
  if (!$('ghPropose')) return;
  const rows = changeRows();
  syncSelection(rows);

  const n = rows.filter(isActionable).length;
  const badge = $('ghBadge');
  badge.textContent = n ? String(n) : '';
  badge.hidden = !n;
  badge.classList.toggle('stale', rows.some((r) => r.stale));
  $('ghPropose').title = n
    ? `GitHub — ${n} file${n === 1 ? '' : 's'} changed and not yet pushed`
    : 'Submit changes to GitHub as a pull request';

  $('ghChangesCount').textContent = rows.length ? `(${rows.length})` : '(none)';
  renderChangeList(rows);
  updateSubmitLabel(rows);
}

function renderChangeList(rows) {
  const list = $('ghChangesList');
  if (!list) return;
  if (!rows.length) {
    list.innerHTML = '<div class="hint">No changes yet — files you edit and save appear here until they are pushed.</div>';
    return;
  }
  const groups = [
    ['From GitHub', rows.filter((r) => r.origin === 'sumo')],
    ['Local uploads and new files', rows.filter((r) => r.origin !== 'sumo')],
  ];
  list.innerHTML = groups
    .filter(([, rs]) => rs.length)
    .map(([label, rs]) => `<div class="gh-group">${esc(label)}</div>${rs.map(renderRow).join('')}`)
    .join('');
}

function renderRow(r) {
  const k = rowKey(r);
  const on = selected.has(k);
  const chips = [`<span class="gh-chip st-${esc(r.state)}">${esc(STATE_LABEL[r.state])}</span>`];
  if (r.stale) chips.push('<span class="gh-chip st-stale">upstream changed</span>');
  if (r.proposed) {
    chips.push(`<a class="gh-chip st-pr" href="${escAttr(r.proposed.url)}" target="_blank" rel="noopener">#${esc(r.proposed.number)} ↗</a>`);
  }
  if (r.prClosed) {
    chips.push(`<a class="gh-chip st-closed" href="${escAttr(r.prClosed.url)}" target="_blank" rel="noopener">#${esc(r.prClosed.number)} ${r.prClosed.merged ? 'merged' : 'closed'} ↗</a>`);
  }
  const path = localPaths.get(k) ?? r.path;
  return `
    <div class="gh-row${r.stale ? ' stale' : ''}">
      <input type="checkbox" data-key="${escAttr(k)}"${on ? ' checked' : ''}
             aria-label="Include ${escAttr(r.name)} in the pull request" />
      <span class="gh-row-name mono">${esc(r.name)}</span>
      ${chips.join('')}
      <span class="gh-row-gap"></span>
      ${r.origin === 'sumo'
        ? `<a class="gh-act" data-diff="${escAttr(k)}">${r.stale ? 'resolve' : 'diff'}</a>`
        : ''}
    </div>
    ${r.origin !== 'sumo' && on ? `
    <div class="gh-row-path">
      <label for="ghPath-${escAttr(k)}">Path in repository</label>
      <input type="text" id="ghPath-${escAttr(k)}" class="gh-path" data-key="${escAttr(k)}"
             value="${escAttr(path)}" spellcheck="false" />
    </div>` : ''}`;
}

function updateSubmitLabel(rows) {
  const chosen = rows.filter((r) => selected.has(rowKey(r)));
  const prs = new Set(chosen.filter((r) => r.proposed).map((r) => r.proposed.number));
  const btn = $('ghSubmit');
  btn.textContent = prs.size === 1 ? `Update pull request #${[...prs][0]}` : 'Create pull request';
  btn.disabled = !chosen.length;
}

/** A title that describes the selection, not whatever file is open. */
function defaultTitle(chosen) {
  if (chosen.length === 1) return `Update ${chosen[0].name}`;
  return chosen.length ? `Update ${chosen.length} SUMO files` : 'Update SUMO';
}

// -- The change list's own controls -------------------------------------------

$('ghChangesToggle').onclick = () => {
  const open = togglePanel('ghChangesToggle', 'ghChangesList');
  $('ghChangesToggle').querySelector('.tri').textContent = open ? '▾' : '▸';
};

$('ghChangesList').addEventListener('change', (e) => {
  const box = e.target.closest('input[type=checkbox]');
  if (!box) return;
  if (box.checked) selected.add(box.dataset.key);
  else selected.delete(box.dataset.key);
  refreshChangeUi();
});

$('ghChangesList').addEventListener('input', (e) => {
  const path = e.target.closest('input.gh-path');
  if (path) localPaths.set(path.dataset.key, path.value);
});

$('ghChangesList').addEventListener('click', (e) => {
  const act = e.target.closest('a.gh-act');
  if (!act) return;
  const row = changeRows().find((r) => rowKey(r) === act.dataset.diff);
  if (row) openDiffDialog(row);
});

// -- Diff / conflict dialog ---------------------------------------------------
//
// One dialog for both jobs: reviewing what a saved edit changed, and deciding
// what to do when upstream has moved underneath it. They differ only in
// wording and in what "take upstream" means (a revert versus adopting a newer
// version), so they share a code path rather than drifting apart as two.

let diffEditor = null;
let diffRow = null;

function disposeDiff() {
  if (!diffEditor) return;
  const model = diffEditor.getModel();
  diffEditor.dispose();
  model?.original?.dispose();
  model?.modified?.dispose();
  diffEditor = null;
  $('ghDiffPane').innerHTML = '';
}

export async function openDiffDialog(row) {
  diffRow = row;
  $('ghDiffTitle').textContent = row.stale ? 'Upstream has changed' : 'Local changes';
  $('ghDiffMsg').textContent = row.stale
    ? `${row.name} changed upstream after you started editing it. Keeping yours leaves your copy based on the older version; taking upstream's discards every local change to this file.`
    : `${row.name} — upstream on the left, your saved copy on the right.`;
  $('ghDiffTake').textContent = row.stale ? 'Take upstream (discards your changes)' : 'Revert to upstream';
  $('ghDiffStatus').textContent = 'Loading upstream copy…';
  $('ghDiffDialog').showModal();
  try {
    const [m, upstream] = await Promise.all([loadMonaco(), fetchUpstreamText(row.path)]);
    if (diffRow !== row) return;   // dialog moved on while we were fetching
    disposeDiff();
    diffEditor = m.editor.createDiffEditor($('ghDiffPane'), {
      readOnly: true,
      automaticLayout: true,
      renderSideBySide: true,
      minimap: { enabled: false },
      fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
      fontSize: 12,
      theme: isDarkTheme() ? 'kif-dark' : 'kif-light',
    });
    diffEditor.setModel({
      original: m.editor.createModel(upstream, 'kif'),
      modified: m.editor.createModel(textOf(row), 'kif'),
    });
    $('ghDiffStatus').textContent = '';
  } catch (e) {
    $('ghDiffStatus').textContent = 'Could not load the upstream copy: ' + (e && e.message || e);
  }
}

function closeDiffDialog() {
  diffRow = null;
  disposeDiff();
  $('ghDiffDialog').close();
}

$('ghDiffKeep').onclick = closeDiffDialog;
$('ghDiffDialog').addEventListener('close', () => { diffRow = null; disposeDiff(); });

$('ghDiffTake').onclick = async () => {
  const row = diffRow;
  if (!row) return;
  const btn = $('ghDiffTake');
  btn.disabled = true;
  $('ghDiffStatus').textContent = 'Replacing your copy…';
  try {
    const text = await fetchUpstreamText(row.path);
    // Saving upstream's own content is what clears the tracking: recordSave
    // drops any record whose content matches upstream, so "revert" and "take
    // the newer upstream copy" are the same operation as an ordinary save.
    await updateConstituentText(row.name, text, row.origin);
    if (state.editCurrentFile?.name === row.name && state.editCurrentFile?.origin === row.origin) {
      state.monacoEditor?.setValue(text);
    }
    closeDiffDialog();
    refreshChangeUi();
  } catch (e) {
    $('ghDiffStatus').textContent = String(e && e.message || e);
  } finally {
    btn.disabled = false;
  }
};

/** Warn once per file when the editor opens something upstream has moved on
 *  from. Opening the file is the moment the user can act on it; the badge and
 *  the row's "resolve" link carry the same signal the rest of the time. */
export function checkStaleOnOpen(file) {
  if (!file || file.origin !== 'sumo') return;
  const row = changeRows().find((r) => r.name === file.name && r.origin === file.origin);
  if (!row?.stale) return;
  const k = rowKey(row);
  if (warned.has(k)) return;
  warned.add(k);
  openDiffDialog(row);
}

// -- Token handling -----------------------------------------------------------

$('ghPropose').onclick = async () => {
  if (!togglePanel('ghPropose', 'ghPanel')) return;
  $('ghToken').value = state.ghToken;
  $('ghRemember').checked = Boolean(localStorage.getItem(GH_TOKEN_KEY));
  ghSetStatus('');
  refreshChangeUi();
  const rows = changeRows();
  if (!$('ghTitle').value) $('ghTitle').value = defaultTitle(rows.filter((r) => selected.has(rowKey(r))));
  // Costs at most one tree read plus one read per tracked pull request, and
  // only when something is actually tracked (both are no-ops otherwise).
  await refreshProposals();
  await refreshUpstreamShas({ force: true });
  refreshChangeUi();
};

$('ghForget').onclick = () => {
  localStorage.removeItem(GH_TOKEN_KEY);
  state.ghToken = '';
  $('ghToken').value = '';
  $('ghRemember').checked = false;
  ghSetStatus('Token forgotten.');
};

// Keep a newly entered token available to read-only API calls even before the
// user submits a pull request. Persistence still requires explicit opt-in.
$('ghToken').addEventListener('change', () => {
  state.ghToken = $('ghToken').value.trim();
  if ($('ghRemember').checked && state.ghToken) localStorage.setItem(GH_TOKEN_KEY, state.ghToken);
  else localStorage.removeItem(GH_TOKEN_KEY);
});
$('ghRemember').addEventListener('change', () => {
  if ($('ghRemember').checked && currentGithubToken()) localStorage.setItem(GH_TOKEN_KEY, currentGithubToken());
  else localStorage.removeItem(GH_TOKEN_KEY);
});

// -- Submission ---------------------------------------------------------------

$('ghSubmit').onclick = async () => {
  const btn = $('ghSubmit');
  const label = btn.textContent;
  const token = $('ghToken').value.trim();
  $('ghResult').innerHTML = '';

  // Everything that can be judged without the network first, so an unusable
  // selection is reported as such rather than as "enter a GitHub token".
  let chosen = changeRows().filter((r) => selected.has(rowKey(r)));
  if (!chosen.length) { ghSetStatus('Tick at least one file to include.', true); return; }

  // What goes up is the SAVED text; an unsaved buffer would be pushed as its
  // last-saved version without the user realizing.
  const open = state.editCurrentFile;
  if (open && selected.has(`${open.origin}:${open.name}`) && isBufferDirty()) {
    ghSetStatus(`Save ${open.name} first — the editor has unsaved changes.`, true);
    return;
  }

  // One submission is one branch. Checked before anything touches the network
  // so an impossible selection fails immediately.
  const openPrs = (rows) => new Map(rows.filter((r) => r.proposed).map((r) => [r.proposed.number, r.proposed]));
  const prs = openPrs(chosen);
  if (prs.size > 1) {
    const names = [...prs.keys()].map((n) => `#${n}`).join(' and ');
    ghSetStatus(`The selected files belong to different pull requests (${names}) — a single commit can only go on one branch, so submit them separately.`, true);
    return;
  }

  const missingPath = chosen.find((r) => !(localPaths.get(rowKey(r)) ?? r.path)?.trim());
  if (missingPath) { ghSetStatus(`Give ${missingPath.name} a path in the repository.`, true); return; }

  if (!token) { ghSetStatus('Enter a GitHub token.', true); return; }

  // Remember only on explicit opt-in; otherwise keep it to this session.
  state.ghToken = token;
  if ($('ghRemember').checked) localStorage.setItem(GH_TOKEN_KEY, token);
  else localStorage.removeItem(GH_TOKEN_KEY);

  btn.disabled = true; btn.textContent = 'Submitting…';
  try {
    // Authoritative staleness check: the panel's own check may be minutes old,
    // and this is the last moment before a write.
    ghSetStatus('Checking for upstream changes…');
    await refreshUpstreamShas({ force: true });
    const fresh = changeRows().filter((r) => selected.has(rowKey(r)));
    const landed = chosen.filter((c) => !fresh.some((f) => rowKey(f) === rowKey(c)));
    chosen = fresh;
    if (!chosen.length) {
      ghSetStatus('');
      $('ghResult').textContent = landed.length
        ? `Nothing left to submit — upstream already has ${landed.map((r) => r.name).join(', ')}.`
        : 'Nothing selected to submit.';
      refreshChangeUi();
      return;
    }
    const stale = chosen.find((r) => r.stale);
    if (stale) {
      ghSetStatus(`${stale.name} changed upstream — resolve it before submitting.`, true);
      openDiffDialog(stale);
      return;
    }
    // Recomputed post-refresh: the file that carried the pull request may have
    // landed in the meantime and dropped out of the selection.
    const existing = [...openPrs(chosen).values()][0] ?? null;

    const files = chosen.map((r) => ({
      name: r.name,
      origin: r.origin,
      path: (localPaths.get(rowKey(r)) ?? r.path).trim(),
      content: textOf(r),
    }));
    const title = $('ghTitle').value.trim() || defaultTitle(chosen);
    const pr = await contributeFiles({
      token, owner: SUMO.owner, repo: SUMO.repo,
      files, title, body: $('ghBody').value.trim(), existing,
      onStep: (s) => ghSetStatus(s),
    });
    markProposed(files.map((f, i) => ({ ...f, blobSha: pr.blobShas[i] })), pr);
    refreshChangeUi();
    ghSetStatus('');
    const what = `${files.length} file${files.length === 1 ? '' : 's'}`;
    $('ghResult').innerHTML = pr.amended
      ? `Added ${what} to <a href="${escAttr(pr.url)}" target="_blank" rel="noopener">pull request #${esc(pr.number)} ↗</a>`
        + ` on <code>${esc(pr.branch)}</code>.`
      : `Opened <a href="${escAttr(pr.url)}" target="_blank" rel="noopener">pull request #${esc(pr.number)} ↗</a>`
        + ` with ${what} from <code>${esc(pr.branch)}</code>${pr.forked ? ' (via your fork)' : ''}.`;
  } catch (e) {
    ghSetStatus('');
    $('ghResult').innerHTML = `<span style="color:var(--bad)">${esc(String(e && e.message || e))}</span>`;
  } finally {
    btn.disabled = false; btn.textContent = label;
    updateSubmitLabel(changeRows());
  }
};
