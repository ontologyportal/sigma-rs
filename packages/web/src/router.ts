/**
 * Tabs + URL routing.
 *
 * Routing is path-based — /edit?file=Merge.kif&l=100 — served by Cloudflare
 * Pages' _redirects (public/_redirects: `/* /index.html 200`), which rewrites
 * any sub-path to index.html so the SPA router below can take over. Vite's dev
 * server mirrors that fallback locally. GitHub Pages has no such rewrite, so a
 * hard refresh on a non-root path 404s there; in-app navigation (pushState) is
 * unaffected either way. `browse` is the default tab and stays at the bare
 * mount path. Legacy `?tab=` bookmarks (the old query-string scheme) still
 * resolve correctly.
 */

import { PROMOTE_TABS } from './constants.ts';
import { state } from './state.ts';
import { call } from './rpc.ts';
import { $, targetEl } from './dom.ts';
import { ensureEditorReady, onEditPickerChange, setEditFullscreen } from './tabs/edit.ts';
import { applyDiagRouteParams } from './tabs/diagnostics.ts';
import { openManPage, runSearch, setBrowseHome, resetBrowseView } from './tabs/browse.ts';
import { loadSumoCatalog } from './tabs/kb-tab.ts';
import { ensureProverEditors } from './tabs/prover.ts';
import { toggleProverSettings } from './prover-config.ts';
import { ensureHistory } from './tabs/history.ts';
import { refreshHomeStats } from './tabs/home-stats.ts';

const TABS = ['browse', 'kb', 'diagnostics', 'prover', 'audit', 'edit', 'history'];

// The path the app is mounted at — "/" locally and on Cloudflare, "/browse/"
// on Pages. Vite substitutes its configured `base` (always absolute, see
// vite.config.ts). Do not derive this from the document or from
// `import.meta.url`: the bundled module lives in the asset directory, and
// under the SPA fallback the document's directory varies per route, either of
// which sends every deep link to the default tab.
export const BASE = import.meta.env.BASE_URL;

export function currentTab() {
  return document.querySelector<HTMLElement>('nav.tabs button[aria-selected="true"]')?.dataset.tab || 'browse';
}

/** The route encoded in the address bar: { tab, params }. The tab is a real
 *  path segment (/edit, /diagnostics, …); a legacy `?tab=` query param (old
 *  bookmarks/links, including the retired `?tab=home`) is honoured as a
 *  fallback when the path itself doesn't name a known tab. */
export function routeFromLocation() {
  const params = new URLSearchParams(location.search);
  const seg = location.pathname.slice(BASE.length).replace(/^\/+/, '').split('/')[0];
  let tab = TABS.includes(seg) ? seg : null;
  if (!tab) {
    const legacy = params.get('tab');
    tab = TABS.includes(legacy) ? legacy : 'browse';
  }
  params.delete('tab');
  return { tab, params };
}

/** Write `tab` + `params` to the address bar without reloading. `browse` is
 *  the default, so it is left off the path to keep the bare URL clean. */
export function syncUrl(tab, params = new URLSearchParams(), { replace = false } = {}) {
  const p = new URLSearchParams(params);
  p.delete('tab');
  const qs = p.toString();
  const path = tab && tab !== 'browse' ? BASE + tab : BASE;
  history[replace ? 'replaceState' : 'pushState'](null, '', path + (qs ? `?${qs}` : ''));
}

/**
 * Show a tab. By default this records a history entry so Back/Forward work;
 * pass `{ push: false }` when reacting to the URL (boot, popstate) so we don't
 * re-push what we just read. `params` is carried into the address bar.
 * `resetBrowse` (default true) controls whether landing on Browse clears its
 * search/man-page state — the header logo's "go home" jump wants that, but a
 * bare tab-bar click back to Browse doesn't: see the nav.tabs listener below.
 */
export function showTab(
  name: string,
  { push = true, params, resetBrowse = true }: { push?: boolean; params?: URLSearchParams; resetBrowse?: boolean } = {},
) {
  if (state.promoting && PROMOTE_TABS.includes(name)) return; // greyed while post-processing
  // Navigating away from Edit — a tab-bar click, a citation's "open man page"
  // link, browser Back, anything that routes through here — always deflates
  // fullscreen first, whether or not `name` is actually 'edit' itself.
  if (name !== 'edit') setEditFullscreen(false);
  for (const btn of document.querySelectorAll<HTMLElement>('nav.tabs button')) {
    btn.setAttribute('aria-selected', String(btn.dataset.tab === name));
  }
  for (const p of document.querySelectorAll<HTMLElement>('.panel')) p.hidden = p.id !== `tab-${name}`;
  // The prover options panel is shared between the Prover and Audit tabs; it
  // should stay open across those two but close on any other tab.
  if (name !== 'prover' && name !== 'audit') toggleProverSettings(false);
  // A bare tab-bar re-entry to Browse (resetBrowse: false, no explicit
  // params) is "come back to what I was looking at," not a fresh navigation
  // — leave the address bar's existing ?sym=/?q= alone instead of
  // overwriting it with an empty query, the same way Edit/Diagnostics don't
  // touch the URL on a bare tab-bar re-entry.
  if (push && !(name === 'browse' && !resetBrowse)) syncUrl(name, params ?? new URLSearchParams());
  // Only reset on a live navigation that asks for it (header logo) — a plain
  // tab-bar click back to Browse restores prior state instead. applyRoute's
  // own `push: false` call still needs the URL's ?q=/?sym= honoured below.
  if (name === 'browse') { if (push && resetBrowse) resetBrowseView(); refreshHomeStats(); }
  if (name === 'kb') loadSumoCatalog();
  if (name === 'edit') ensureEditorReady().catch(() => {}); // surfaced in-panel
  if (name === 'prover') ensureProverEditors().catch(() => {}); // textareas remain the fallback
  // Read the file straight off the URL: syncUrl (above) has already applied a
  // nav click, so this sees ?file=… on a deep link and nothing on a plain
  // click — one code path, and no double fetch from applyRoute.
  if (name === 'history') ensureHistory(new URLSearchParams(location.search).get('file'));
}

/**
 * Apply the current URL: switch to its tab and honour its deep-link params.
 * Runs after boot (constituents must exist) and on every popstate.
 *   /edit?file=Merge.kif&l=100   load that file in the editor, reveal line 100
 *   /kb  /audit  …               open that tab
 *   ?q=Human                     run the search (on the default /browse tab)
 *   ?sym=Human                   open the man page
 */
export async function applyRoute() {
  const { tab, params } = routeFromLocation();
  showTab(tab, { push: false });

  if (tab === 'edit') {
    const file = params.get('file');
    const line = Number(params.get('l') || params.get('line'));
    await ensureEditorReady();
    if (file) {
      // Match on name alone — a deep link shouldn't have to know the origin.
      const c = state.constituents.find((x) => x.name === file);
      if (c) {
        $('editPicker').value = `${c.name}|${c.origin}`;
        onEditPickerChange();
      } else {
        $('editLog').style.color = 'var(--bad)';
        $('editLog').textContent = `${file} is not among the loaded constituents.`;
      }
    }
    if (state.monacoEditor && Number.isFinite(line) && line > 0) {
      state.monacoEditor.revealLineInCenter(line);
      state.monacoEditor.setPosition({ lineNumber: line, column: 1 });
      state.monacoEditor.focus();
    }
    return;
  }

  if (tab === 'diagnostics') { applyDiagRouteParams(); return; }

  // ?sym= / ?q= belong to Browse (the default tab, so bare legacy links with
  // neither ?tab= nor ?tab=home land here too).
  if (tab === 'browse') {
    const sym = params.get('sym');
    const q = params.get('q');
    if (sym) { openManPage(sym); }
    else if (q) { $('q').value = q; runSearch(q); }
    else { setBrowseHome(true); }
  }

}

/** Push a history entry for `tab` with `params` and render it. The three
 *  cross-tab jumps (editor, diagnostics, documentation) all go through here so
 *  they agree on push-vs-replace and on param naming. */
export function navigate(tab, obj) {
  const p = new URLSearchParams();
  for (const [k, v] of Object.entries(obj || {})) if (v != null && v !== '') p.set(k, String(v));
  syncUrl(tab, p);
  return applyRoute();
}

/** Replace the query on the current tab, so the address bar stays shareable
 *  without pushing a history entry for every search/file switch. */
export function updateParams(obj) {
  const p = new URLSearchParams();
  for (const [k, v] of Object.entries(obj)) if (v != null && v !== '') p.set(k, String(v));
  syncUrl(currentTab(), p, { replace: true });
}

document.querySelector('nav.tabs').addEventListener('click', (e) => {
  const btn = targetEl(e).closest('button');
  // resetBrowse: false — see showTab's doc comment. Switching tabs and back
  // should restore what was there, not silently drop it (issue #67).
  if (btn && btn.getAttribute('aria-disabled') !== 'true') showTab(btn.dataset.tab, { resetBrowse: false });
});
document.addEventListener('click', (e) => {
  const jump = targetEl(e).closest<HTMLElement>('.jump');
  if (jump && jump.getAttribute('aria-disabled') !== 'true') { e.preventDefault(); showTab(jump.dataset.tab); }
});
// A symbol inside a rendered formula (man-page refs, proof/audit steps) opens
// its man page from any tab. preventDefault also cancels the enclosing
// <summary>'s expand toggle when the symbol sits inside a citation row.
document.addEventListener('click', async (e) => {
  const link = targetEl(e).closest<HTMLElement>('.sym-link');
  if (!link) return;
  e.preventDefault();
  if (link.classList.contains('sym-dead')) return;
  // Probe before navigating: a symbol with no man page (Skolems that slipped
  // the lexical filter, numerals, ill-formed tokens) must not yank the user
  // away from a proof they are reading just to show an error card.
  try {
    const { page } = await call('manpage', { symbol: link.dataset.sym });
    if (!page) {
      link.classList.add('sym-dead');
      link.title = `no man page for ${link.dataset.sym}`;
      return;
    }
  } catch { return; }
  navigate('browse', { sym: link.dataset.sym });
});
window.addEventListener('popstate', () => { applyRoute(); });
