/** Page chrome that belongs to no tab: the theme toggle, the bug-report link,
 *  the settings modal, and the version-change notice. */

import { APP_REPO, THEME_KEY, SEEN_VERSION_KEY } from './constants.js';
import { $ } from './dom.js';

// -- Theme toggle: explicit choice wins over the OS preference ----------------

$('themeToggle')?.addEventListener('click', () => {
  const current = document.documentElement.dataset.theme ||
    (matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light');
  const next = current === 'dark' ? 'light' : 'dark';
  document.documentElement.dataset.theme = next;
  try { localStorage.setItem(THEME_KEY, next); } catch { /* private mode */ }
  window.monaco?.editor.setTheme(next === 'dark' ? 'kif-dark' : 'kif-light');
});

// -- Bug report: no anonymous issue creation via the GitHub API, so this just
// opens a prefilled "new issue" page against this app's own repo. ----------

let appVersion = null;

$('bugReport')?.addEventListener('click', () => {
  const body = [
    '',
    '',
    '---',
    `Version: ${appVersion?.version ?? 'unknown'} (build ${appVersion?.build ?? '?'}, ${appVersion?.commit ?? '?'})`,
    `URL: ${location.href}`,
    `User agent: ${navigator.userAgent}`,
  ].join('\n');
  const url = `https://github.com/${APP_REPO.owner}/${APP_REPO.repo}/issues/new?` +
    new URLSearchParams({ title: '', body, labels: 'bug' });
  window.open(url, '_blank', 'noopener');
});

// -- Settings modal: language selector (moved out of the header) + version --

$('settingsBtn')?.addEventListener('click', () => $('settingsDialog').showModal());
$('settingsClose')?.addEventListener('click', () => $('settingsDialog').close());

fetch('./version.json')
  .then(r => (r.ok ? r.json() : null))
  .then(v => {
    if (!v) return;
    appVersion = v;
    const el = $('settingsVersion');
    if (el) el.textContent = `v${v.version} (build ${v.build}, ${v.commit})`;
    checkVersionChange(v.version);
  })
  .catch(() => { /* local dev / no version.json published yet */ });

// -- Version-change modal: welcome on first visit, notice on upgrade --------

function checkVersionChange(version) {
  let seen;
  try { seen = localStorage.getItem(SEEN_VERSION_KEY); } catch { return; }
  if (seen === version) return;

  const dialog = $('versionDialog');
  const title = $('versionDialogTitle');
  const body = $('versionDialogBody');
  if (dialog && title && body) {
    if (seen === null) {
      title.textContent = 'Welcome to SigmaKEE';
      body.textContent = '';
    } else {
      title.textContent = 'New version available';
      body.textContent = `You are using a new version (v${version}).`;
    }
    dialog.showModal();
  }

  try { localStorage.setItem(SEEN_VERSION_KEY, version); } catch { /* private mode */ }
}

$('versionDialogClose')?.addEventListener('click', () => $('versionDialog').close());
