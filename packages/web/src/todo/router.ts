/**
 * Tabs + URL routing, on top of `vue-router`'s history/navigation machinery
 * instead of hand-rolled `pushState`/`popstate`.
 *
 * Every tab's content is a Vue component that stays mounted for the app's
 * whole lifetime -- App.vue renders all seven, `v-show`-toggled by the
 * current route -- rather than being swapped in/out by a `<router-view>`.
 * Monaco editors, the prover panels, and the rest all depend on that: their
 * state must survive a tab switch. So `<router-view>` is never used here;
 * `router` only supplies matching, history, and a reactive current route
 * (`router.currentRoute`) that App.vue reads for `v-show`/`aria-selected`,
 * and that `afterEach` below reacts to for the per-tab entry side effects
 * (`applyTabEffects`) the old hand-rolled `showTab`/`applyRoute` used to
 * drive by walking the DOM.
 *
 * Routing is path-based — /edit?file=Merge.kif&l=100 — served by Cloudflare
 * Pages' _redirects (public/_redirects: `/* /index.html 200`), which rewrites
 * any sub-path to index.html so the SPA router below can take over. Vite's dev
 * server mirrors that fallback locally. GitHub Pages has no such rewrite, so a
 * hard refresh on a non-root path 404s there; in-app navigation (pushState,
 * which is all vue-router's default history mode ever does) is unaffected
 * either way. `browse` is the default tab and stays at the bare mount path.
 * Legacy `?tab=` bookmarks (the old query-string scheme) still resolve
 * correctly, via a redirecting `beforeEach` guard below.
 */

import { createRouter, createWebHistory, type RouteLocationNormalized } from 'vue-router';
import { PROMOTE_TABS } from './constants.ts';
import { state } from './state.ts';
import { call } from './rpc.ts';
import { $ } from './dom.ts';
import { ensureEditorReady, onEditPickerChange, setEditFullscreen } from './tabs/edit.ts';
import { applyDiagRouteParams } from './tabs/diagnostics.ts';
import { openManPage, runSearch, resetBrowseView } from './tabs/browse.ts';
import { loadSumoCatalog } from './tabs/kb-tab.ts';
import { ensureProverEditors } from './tabs/prover.ts';
import { toggleProverSettings } from './prover-config.ts';
import { ensureHistory } from './tabs/history.ts';
import { refreshHomeStats } from './tabs/home-stats.ts';

// The path the app is mounted at — "/" locally and on Cloudflare, "/browse/"
// on Pages. Vite substitutes its configured `base` (always absolute, see
// vite.config.ts). Do not derive this from the document or from
// `import.meta.url`: the bundled module lives in the asset directory, and
// under the SPA fallback the document's directory varies per route, either of
// which sends every deep link to the default tab.



// A route record needs a `component` (or `children`); since none of these is
// ever rendered by a `<router-view>` (see the file doc comment), a single
// shared no-op component satisfies that without meaning anything.
const NoRender = { render: () => null };

export const router = createRouter({
  history: createWebHistory(BASE),
  routes: TABS.map((name) => ({ path: name === 'browse' ? '/' : `/${name}`, name, component: NoRender })),
});

// Legacy `?tab=` bookmarks (the old query-string scheme, including the
// retired `?tab=home`, which never matched a real tab and so already just
// falls through to Browse via the bare `/` route below) still resolve
// correctly: redirect them onto the real path the first time they're seen,
// dropping the query param. `browse`'s own path IS `/`, the same fallback
// every OTHER unrecognized path also resolves to -- so this only skips a
// path that's already unambiguously a DIFFERENT real tab; a bare `/` still
// needs checking; that's what actually resolves e.g. `/?tab=prover`.
router.beforeEach((to) => {
  if (to.name && to.name !== 'browse') return true;
  const legacy = to.query.tab;
  if (typeof legacy !== 'string' || !(TABS as readonly string[]).includes(legacy) || legacy === 'browse') return true;
  const query = { ...to.query };
  delete query.tab;
  return { path: `/${legacy}`, query, replace: true };
});

router.beforeEach((to, from) => {
  // Greyed while post-processing: bounce a cold or clicked deep link into a
  // promote-gated tab back to Browse instead of landing somewhere unusable.
  // This only catches an actual navigation attempt; `kb.ts`'s
  // `setPromoteTabsEnabled` is what evicts a tab already showing when
  // promotion starts under it, and remembers where to send the user back
  // once it finishes.
  if (state.promoting && PROMOTE_TABS.includes(String(to.name))) return { name: 'browse' };
  // Navigating away from Edit — a tab-bar click, a citation's "open man page"
  // link, browser Back, anything that routes through here — always deflates
  // fullscreen first, whether or not the destination is Edit itself.
  if (from.name === 'edit' && to.name !== 'edit') setEditFullscreen(false);
  // The prover options panel is shared between the Prover and Audit tabs; it
  // should stay open across those two but close on any other tab.
  if (to.name !== 'prover' && to.name !== 'audit') toggleProverSettings(false);
  return true;
});

/**
 * Per-tab entry side effects + deep-link params, run once a navigation has
 * actually landed (see the `afterEach` below and `applyRoute`).
 *   /edit?file=Merge.kif&l=100   load that file in the editor, reveal line 100
 *   /kb  /audit  …               ready that tab
 *   ?q=Human                     run the search (on the default /browse tab)
 *   ?sym=Human                   open the man page
 */
function applyTabEffects(to: RouteLocationNormalized) {
  const name = to.name as string;
  if (name === 'kb') loadSumoCatalog();
  if (name === 'prover') ensureProverEditors().catch(() => {}); // textareas remain the fallback
  if (name === 'history') ensureHistory(typeof to.query.file === 'string' ? to.query.file : null);

  if (name === 'edit') {
    const file = typeof to.query.file === 'string' ? to.query.file : '';
    const line = Number(to.query.l ?? to.query.line);
    ensureEditorReady().then(() => {
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
    }).catch(() => {}); // surfaced in-panel
    return;
  }

  if (name === 'diagnostics') { applyDiagRouteParams(to.query); return; }

  // ?sym= / ?q= belong to Browse (the default tab, so bare legacy links with
  // neither ?tab= nor ?tab=home land here too).
  if (name === 'browse') {
    refreshHomeStats();
    const sym = typeof to.query.sym === 'string' ? to.query.sym : '';
    const q = typeof to.query.q === 'string' ? to.query.q : '';
    if (sym) openManPage(sym);
    else if (q) { $('q').value = q; runSearch(q); }
    else resetBrowseView(); // also clears any stale text left in the search box
  }
}

// Suppressed for exactly two kinds of navigation that must not re-derive
// content from the query they just wrote: `updateParams` (bookkeeping only —
// the caller already rendered whatever the new query describes) and a bare
// nav-tab-bar re-entry to Browse, which restores whatever was already
// showing rather than re-deriving from the URL or resetting to the welcome
// screen (issue #67) — see `navigateQuiet`.
let suppressEffects = false;

// Gated on `applyRoute` (called once by boot.ts, after the KB exists) so a
// popstate/programmatic navigation before that point — there is none in
// practice, but the router's own initial navigation always fires this guard
// once as soon as it's installed, long before boot() runs — can't try to
// open a man page or ready an editor against a KB that isn't loaded yet.
let bootDone = false;

router.afterEach((to) => {
  if (!bootDone || suppressEffects) { suppressEffects = false; return; }
  applyTabEffects(to);
});

/** Run once, after boot (constituents must exist), to honour whatever route
 *  is already in the address bar — a cold load or a page refresh on a deep
 *  link. Every navigation from here on (nav click, `navigate()`, browser
 *  Back/Forward) re-triggers `applyTabEffects` on its own, via `afterEach`. */
export async function applyRoute() {
  await router.isReady();
  bootDone = true;
  applyTabEffects(router.currentRoute.value);
}

/** `true` on the given tab. */
export function currentTab(): TabName {
  return (router.currentRoute.value.name as TabName) || 'browse';
}

function toQuery(obj: Record<string, unknown> | undefined) {
  const query: Record<string, string> = {};
  for (const [k, v] of Object.entries(obj || {})) if (v != null && v !== '') query[k] = String(v);
  return query;
}

/** Push a history entry for `tab` with `params` and navigate there. The
 *  cross-tab jumps (editor, diagnostics, documentation) all go through here
 *  so they agree on param naming; browser-visible navigation, unlike
 *  `updateParams`. */
export function navigate(tab: TabName, obj?: Record<string, unknown>) {
  return router.push({ name: tab, query: toQuery(obj) });
}

/** Switch to `tab` without touching whatever content is already rendered
 *  there — used by the nav tab bar's bare re-entry to Browse (see
 *  `suppressEffects` above); every other destination behaves exactly like
 *  `navigate` with no params. */
export function navigateQuiet(tab: TabName) {
  suppressEffects = true;
  return router.push({ name: tab });
}

/** Replace the query on the current tab, so the address bar stays shareable
 *  without pushing a history entry for every search/file switch, and without
 *  re-triggering that tab's entry side effects (the caller already rendered
 *  whatever this query now describes). */
export function updateParams(obj: Record<string, unknown>) {
  suppressEffects = true;
  return router.replace({ name: currentTab(), query: toQuery(obj) });
}

// A cross-tab jump link (the header logo, or one rendered inline in a hint
// -- "see the Knowledge base tab") -- one delegated listener for both, since
// most `.jump` targets are rendered as HTML strings via `v-html`, not as Vue
// template elements a per-element `@click` could bind to; the header logo
// (real template markup) just goes through the same listener for consistency.
document.addEventListener('click', (e) => {
  const jump = (e.target as HTMLElement).closest<HTMLElement>('.jump');
  if (jump && jump.getAttribute('aria-disabled') !== 'true' && jump.dataset.tab) {
    e.preventDefault();
    navigate(jump.dataset.tab as TabName);
  }
});
// A symbol inside a rendered formula (man-page refs, proof/audit steps) opens
// its man page from any tab. preventDefault also cancels the enclosing
// <summary>'s expand toggle when the symbol sits inside a citation row.
document.addEventListener('click', async (e) => {
  const link = (e.target as HTMLElement).closest<HTMLElement>('.sym-link');
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
