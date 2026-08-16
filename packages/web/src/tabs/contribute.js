/**
 * Contribute: open a pull request against ontologyportal/sumo.
 *
 * The editor buffer is proposed upstream as a branch + PR using a token the
 * user supplies. The token is held in memory for the session; it is only
 * persisted (localStorage) if the user ticks "remember", and never leaves the
 * browser except as an Authorization header to api.github.com.
 */

import { SUMO, GH_TOKEN_KEY } from '../constants.js';
import { state } from '../state.js';
import { $, esc, togglePanel } from '../dom.js';
import { contribute } from '../github.js';
import { currentGithubToken } from '../github-api.js';

function ghSetStatus(text, bad = false) {
  const el = $('ghStatus');
  el.textContent = text;
  el.style.color = bad ? 'var(--bad)' : '';
}

/** The file the Contribute panel acts on: whatever the Edit tab has open.
 *  (Submit-to-GitHub is only ever shown for a `sumo`-origin file — see
 *  updateEditActions — so an unnamed new file, always `file`-origin, never
 *  reaches here in practice; the empty-string fallback is just defensive.) */
function ghCurrentFile() {
  if (state.editCurrentFile) return state.editCurrentFile.name;
  const v = $('editPicker').value;
  return v && v !== '__new__' ? v.slice(0, v.indexOf('|')) : '';
}

$('ghPropose').onclick = () => {
  if (!togglePanel('ghPropose', 'ghPanel')) return;
  $('ghToken').value = state.ghToken;
  $('ghRemember').checked = Boolean(localStorage.getItem(GH_TOKEN_KEY));
  const file = ghCurrentFile();
  if (!$('ghTitle').value) $('ghTitle').value = file ? `Update ${file}` : 'Update SUMO';
  ghSetStatus(file ? '' : 'Open a file in the editor first.', !file);
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

$('ghSubmit').onclick = async () => {
  const btn = $('ghSubmit');
  const file = ghCurrentFile();
  const token = $('ghToken').value.trim();
  $('ghResult').innerHTML = '';
  if (!file)  { ghSetStatus('Open a file in the editor first.', true); return; }
  if (!token) { ghSetStatus('Enter a GitHub token.', true); return; }
  if (!state.monacoEditor) { ghSetStatus('The editor is still loading.', true); return; }

  // Remember only on explicit opt-in; otherwise keep it to this session.
  state.ghToken = token;
  if ($('ghRemember').checked) localStorage.setItem(GH_TOKEN_KEY, token);
  else localStorage.removeItem(GH_TOKEN_KEY);

  btn.disabled = true; btn.textContent = 'Submitting…';
  try {
    const pr = await contribute({
      token, owner: SUMO.owner, repo: SUMO.repo,
      path: file,
      content: state.monacoEditor.getValue(),      // the live buffer, not the last save
      title: $('ghTitle').value.trim() || `Update ${file}`,
      body: $('ghBody').value.trim(),
      onStep: (s) => ghSetStatus(s),
    });
    ghSetStatus('');
    $('ghResult').innerHTML =
      `Opened <a href="${esc(pr.url)}" target="_blank" rel="noopener">pull request #${pr.number} ↗</a>` +
      ` from <code>${esc(pr.branch)}</code>${pr.forked ? ' (via your fork)' : ''}.`;
  } catch (e) {
    ghSetStatus('');
    $('ghResult').innerHTML = `<span style="color:var(--bad)">${esc(String(e && e.message || e))}</span>`;
  } finally {
    btn.disabled = false; btn.textContent = 'Create pull request';
  }
};
