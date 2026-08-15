/**
 * SUMO browser — a demo over the SDK-shaped facade, running the wasm engine in
 * a Web Worker (sigma.worker.js) so the prover never blocks the UI thread.
 *
 * Loading only INGESTS each constituent; axiomatization (promote) runs in the
 * background afterwards, during which the promote-dependent tabs (Diagnostics /
 * Ask-Tell / Audit) are greyed behind a "post-processing" toast. Search /
 * Knowledge base / Edit stay live throughout.
 *
 * Tabs:
 *   Home          — symbol search → results → man page
 *   Knowledge base — manage loaded constituents (add from SUMO / URL / upload)
 *   Diagnostics   — the KB's validation findings
 *   Ask/Tell      — tell assertions + ask a query, in-browser (Cytoscape proof)
 *   Audit         — whole-KB consistency check
 *   Edit          — in-browser Monaco IDE for KIF constituents
 *
 * Must be served over HTTP — browsers block ES modules + wasm fetch on file://.
 *   npm run web   # → http://localhost:8080/
 *
 * The page owns the constituent list, OPFS, localStorage, and the editor; the
 * worker owns the Session. The engine comes from the `sigmakee` workspace
 * package, which Vite bundles alongside this file, so the whole demo can be
 * dropped at any path (locally, or /browse/ on GitHub Pages).
 */

import { formatKif, formatTest } from 'sigmakee/sdk';

const worker = new Worker(new URL('./sigma.worker.js', import.meta.url), { type: 'module' });

// -- tiny id-keyed RPC over postMessage ---------------------------------------
let seq = 0;
const pending = new Map();
worker.onmessage = (e) => {
  const { id, result, error } = e.data;
  const p = pending.get(id);
  if (!p) return;
  pending.delete(id);
  error ? p.reject(new Error(error)) : p.resolve(result);
};
const call = (cmd, args) => new Promise((resolve, reject) => {
  const id = ++seq; pending.set(id, { resolve, reject });
  worker.postMessage({ id, cmd, args });
});
worker.onerror = (e) => {
  const m = e.message || `${e.filename || ''}:${e.lineno || ''}`;
  const ov = document.getElementById('overlayErr'); if (ov) ov.textContent = 'worker: ' + m;
  console.error('worker error', e);
};

const SUMO = { owner: 'ontologyportal', repo: 'sumo', ref: 'HEAD' };
const MERGE = 'Merge.kif';                     // the foundational ontology, loaded on startup
const MIDLEVEL = 'Mid-level-ontology.kif';     // also loaded on startup
const rawUrl = (path) => `https://raw.githubusercontent.com/${SUMO.owner}/${SUMO.repo}/${SUMO.ref}/${path}`;
const SUMO_FILE_SETTING = 'sumoFiles';
let savedConstituents = JSON.parse(localStorage.getItem(SUMO_FILE_SETTING) || 'null') || [
  { name: MERGE, origin: 'sumo' },
  { name: MIDLEVEL, origin: 'sumo' },
];
let opfsRoot = null;

// One credential for every GitHub API request: catalog/commit reads as well
// as contribution writes. It remains session-only unless "remember" is
// explicitly enabled in the Edit contribution panel.
const GH_TOKEN_KEY = 'sumoBrowserGhToken';
let ghToken = localStorage.getItem(GH_TOKEN_KEY) || '';

function currentGithubToken() {
  return $('ghToken')?.value.trim() || ghToken;
}

const $ = (id) => document.getElementById(id);
const esc = (s) => String(s).replace(/[&<>]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' }[c]));
const escAttr = (s) => esc(s).replace(/"/g, '&quot;');
const fmtNum = (n) => Number(n).toLocaleString();
const fmtDate = (d) => d.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });

/** GitHub REST, via the same client the contribution flow uses. The user's
 *  optional token authenticates catalog and commit reads as well as writes,
 *  with one place for headers and rate-limit wording. */
async function githubApi(path) {
  const { api } = await import('./github.js');
  return api(currentGithubToken(), path);
}

let diagnostics = [];
let diagFilter = { file: '', severity: '', kind: '', code: '', page: 0 };
// A full SUMO load can produce thousands of Hint-severity completeness
// findings alone; pagination keeps `diagList` from rendering them all as DOM
// nodes at once.
const DIAG_PAGE_SIZE = 50;
let constituents = [];   // [{ name, text, origin }] — the page's source of truth
let sumoCatalog = null;  // cached list of *.kif paths in the repo
let uiLanguage = 'EnglishLanguage';  // header selector; the language for term/format rendering

async function fromOrigin(origin, file) {
  if (origin === 'sumo') return await fetchText(rawUrl(file));
  if (origin === 'url') return await fetchText(file);
  if (origin === 'file') {
    if (opfsRoot === null) throw new Error('File system not initialized yet');
    const handle = await opfsRoot.getFileHandle(file);
    const vFile = await handle.getFile();
    return await vFile.text();
  }
}

async function fetchText(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${url}: HTTP ${r.status}`);
  return r.text();
}

// -- KB state mutations -------------------------------------------------------
// Mutations INGEST (fast) but do not promote; the caller runs reprocess() once
// to promote + validate under the post-processing toast.

/**
 * Ingest one constituent's text into the worker session and track it. The
 * constituent is tracked once ingested — ingest still accepts content that
 * carries non-fatal notices (e.g. "duplicate formula ignored").
 * @returns {Promise<{ added: boolean, notices: string[] }>}
 */
async function ingestConstituent(name, text, origin = 'sumo') {
  if (constituents.some((c) => c.name === name)) return { added: false, notices: [`${name}: already loaded`] };
  const { notices } = await call('ingest', { name, text });
  constituents.push({ name, text, origin });
  if (savedConstituents.find((c) => c.name == name && c.origin == origin) === undefined) {
    savedConstituents.push({ name, origin });
    localStorage.setItem(SUMO_FILE_SETTING, JSON.stringify(savedConstituents));
  }
  return { added: true, notices };
}

/** Rebuild the worker session from the current (cached) constituents — used by remove/reset/edit. */
async function rebuildSession() {
  await call('newSession');
  for (const c of constituents) await call('ingest', { name: c.name, text: c.text });
}

/**
 * Save `text` as constituent `name`/`origin` — updates it in place if already
 * loaded, else adds it. Used by the Edit tab's Save button. For `file`-origin
 * constituents, persists to OPFS FIRST (awaited), mirroring the KB tab's upload
 * flow — a `file` entry with no OPFS handle would throw on next boot and abort
 * loading every OTHER constituent too.
 * @returns {Promise<{ added: boolean, notices: string[] }>}
 */
async function updateConstituentText(name, text, origin = 'file') {
  if (origin === 'file') {
    if (!opfsRoot) throw new Error('File system not initialized yet');
    const handle = await opfsRoot.getFileHandle(name, { create: true });
    const stream = await handle.createWritable();
    await stream.write(text);
    await stream.close();
  }
  const idx = constituents.findIndex((c) => c.name === name && c.origin === origin);
  if (idx === -1) {
    const r = await ingestConstituent(name, text, origin);
    await reprocess();
    return r;
  }
  constituents[idx] = { ...constituents[idx], text };
  // In-place diff-commit instead of rebuildSession(): validateBuffer stages
  // the buffer under the file's own name and commits it, so only the changed
  // sentences are processed — a one-formula edit costs a diff, not a full
  // re-ingest of every constituent. reprocess() then re-promotes (no-op for
  // untouched files) and re-validates for correct whole-KB diagnostics.
  // Remove/reset below genuinely retract whole files, so they keep the rebuild.
  await call('validateBuffer', { file: name, text });
  await reprocess();
  return { added: false, notices: [] };
}

async function removeConstituent(name, origin = 'sumo') {
  constituents = constituents.filter((c) => c.name !== name || c.origin !== origin);
  savedConstituents = savedConstituents.filter((c) => c.name !== name || c.origin !== origin);
  localStorage.setItem(SUMO_FILE_SETTING, JSON.stringify(savedConstituents));
  if (origin === 'file') {
    try { const h = await opfsRoot.getFileHandle(name); await h.remove(); } catch { /* already gone */ }
  }
  await rebuildSession();
  await reprocess();
}

async function resetToMerge() {
  const merge = constituents.find((c) => c.name === MERGE);
  constituents = merge ? [merge] : [];
  await rebuildSession();
  await reprocess();
}

// -- Deferred promote + post-processing UI ------------------------------------

// Keep the toast up at least this long so the post-processing state is
// perceptible even when promote+validate finish in well under one paint frame.
const MIN_TOAST_MS = 650;
// Tabs that need the KB axiomatized; greyed while a promote is in flight.
const PROMOTE_TABS = ['diagnostics', 'prover', 'audit'];
let promoting = false;

// Run `fn` (promote → validate → render) under the "post-processing" UI: grey
// the promote-dependent tabs and show the toast until it finishes. Ingest
// happens BEFORE this (under the loading screen on boot / the busy button on
// adds). Re-entrant: a nested call runs inside the outer window.
async function withPostProcessing(fn) {
  const outer = !promoting;
  if (outer) {
    promoting = true;
    setPromoteTabsEnabled(false);
    showToast(true);
  }
  const shownAt = performance.now();
  try {
    await fn();
  } finally {
    if (outer) {
      const held = performance.now() - shownAt;
      if (held < MIN_TOAST_MS) await new Promise((r) => setTimeout(r, MIN_TOAST_MS - held));
      promoting = false;
      setPromoteTabsEnabled(true);
      showToast(false);
      // A cold-boot deep link into a promote tab (e.g. /prover) got deflected
      // to Browse while promoting — the URL still names the real tab, so
      // return to it now that it's usable (no history push: the URL never
      // changed out from under it).
      const { tab: urlTab } = routeFromLocation();
      if (currentTab() === 'browse' && PROMOTE_TABS.includes(urlTab)) showTab(urlTab, { push: false });
      // Anything that renders the `promoting` flag has to be redrawn HERE.
      // Views refreshed inside the window (renderAll → refreshHomeStats) ran
      // while the flag was still set, so their "post-processing" wording is
      // stale the moment it clears.
      if (currentTab() === 'browse') updateHomeNote();
    }
  }
}

// Promote every ingested constituent into the axiom base, THEN validate once,
// THEN refresh every view. Promote and validate are the KB-size-bound steps —
// validation runs exactly once here, not per constituent.
async function promoteAndValidate() {
  await call('promoteAll', { names: constituents.map((c) => c.name) });
  diagnostics = (await call('validate')).diagnostics;
  renderAll();
  // The route was applied before any of this existed; re-honour ?file/?sev/?l
  // now that there is something to filter and scroll to.
  applyDiagRouteParams();
  // Fire-and-forget: refresh the boot cache for next time. Every mutation
  // path (ingest/edit/remove/reset) funnels through here, so the cache is
  // always kept as fresh as the last successful promote — no separate
  // invalidation step. Not awaited: the app is already usable, and a failed
  // or slow cache write must never hold up the UI (see saveKbCache).
  saveKbCache();
}

const reprocess = () => withPostProcessing(promoteAndValidate);

/** Refresh every view that reflects KB contents (after promote+validate). */
function renderAll() {
  renderDiagnostics();
  renderConstituents();
  renderTests();
  refreshLangSelect();
  if (sumoCatalog) renderPicker();
  populateEditPicker();
  if (currentTab() === 'browse') refreshHomeStats();   // counts moved
}

function setPromoteTabsEnabled(on) {
  for (const t of PROMOTE_TABS) {
    const btn = document.querySelector(`nav.tabs [data-tab=${t}]`);
    if (btn) { btn.classList.toggle('disabled', !on); btn.setAttribute('aria-disabled', String(!on)); }
  }
  // push:false — this is an automatic deflection (the tab isn't usable yet),
  // not a navigation, so it must not clobber a deep-linked URL (e.g. a
  // /prover link opened cold, before the boot promote finishes).
  if (!on && PROMOTE_TABS.includes(currentTab())) showTab('browse', { push: false });
}

function showToast(on) { const t = $('toast'); if (t) t.hidden = !on; }

// Populate the header language selector from the KB's `NaturalLanguage`
// instances, preserving the current choice. Fire-and-forget: the selected
// symbol lands in the `uiLanguage` global, consumed by search, man-page
// rendering, and the NL paraphrases. Only call where the language list can
// actually have changed (boot, promote) — the worker round-trip is KB-bound.
async function refreshLangSelect() {
  const sel = $('langSelect');
  if (!sel) return;
  let languages;
  try { languages = (await call('naturalLanguages')).languages; }
  catch { return; }
  if (!languages || !languages.length) return;
  const has = (v) => languages.some((l) => l.symbol === v);
  const options = languages
    .map((l) => `<option value="${esc(l.symbol)}">${esc(l.label)}</option>`)
    .join('');
  // Unchanged list → leave the DOM alone (a rebuild would collapse the
  // dropdown under the user's pointer and reset the selection).
  if (sel.dataset.options !== options) {
    sel.dataset.options = options;
    sel.innerHTML = options;
  }
  sel.value = has(uiLanguage) ? uiLanguage
    : has('EnglishLanguage') ? 'EnglishLanguage' : languages[0].symbol;
  uiLanguage = sel.value;
}

// One-time: react to a language change by re-rendering whatever the Browse
// tab is showing so its documentation follows the selection.
$('langSelect')?.addEventListener('change', () => {
  const sel = $('langSelect');
  uiLanguage = sel.value;
  if (currentTab() !== 'browse') return;
  const params = new URLSearchParams(location.search);
  const sym = params.get('sym');
  const q = params.get('q') || $('q').value.trim();
  if (sym) openManPage(sym);
  else if (q) runSearch(q);
});

// -- KB snapshot cache (OPFS) --------------------------------------------------
//
// Boot's expensive work is re-fetching every 'sumo'-origin file over HTTP and
// re-running ingest+promote+validate from scratch. The core KB has its own
// freeze/thaw seam (session.snapshot()/restore()) built exactly for this; this
// cache pairs a frozen snapshot with a cached copy of each 'sumo' file's text
// (needed for the Edit tab / file-size display, which read `constituents[i]
// .text` regardless of how the KB itself got built) so a matching boot can
// skip BOTH the network fetch and the ingest+promote — not just one of them.
//
// Validity: the cached upstream commit SHA must match the CURRENT SHA
// (`sumo`-origin content is pinned to a commit, not versioned per-file), and
// a fingerprint of the exact (name, origin) set loaded must match (a
// different file set obviously needs a different snapshot). `file`-origin
// text has no commit to pin to, so its cache freshness instead relies on
// every mutation path (ingest/update/remove/reset) funnelling through
// promoteAndValidate, which rewrites the cache after every successful
// promote — there is no separate "invalidate" step, just "always keep the
// cache as fresh as the last successful promote".
//
// `url`-origin constituents have no version signal at all (an arbitrary
// link's content can change with nothing to detect it against), so their
// presence disables caching for that boot entirely rather than risk serving
// stale content silently.

const SUMO_CACHE_DIR      = 'sumo-cache';
const SUMO_CACHE_META     = 'meta.json';
const SUMO_CACHE_SNAPSHOT = 'snapshot.bin';

let sumoCacheDirHandle = null;   // lazily opened, separate from the top-level
                                  // OPFS dir 'file'-origin uploads already use

async function getSumoCacheDir() {
  if (sumoCacheDirHandle) return sumoCacheDirHandle;
  if (!opfsRoot) throw new Error('File system not initialized yet');
  sumoCacheDirHandle = await opfsRoot.getDirectoryHandle(SUMO_CACHE_DIR, { create: true });
  return sumoCacheDirHandle;
}

async function writeOpfsFile(dir, name, contents) {
  const handle = await dir.getFileHandle(name, { create: true });
  const w = await handle.createWritable();
  await w.write(contents);
  await w.close();
}

/** Encode a constituent name into a single OPFS-safe path segment. Some SUMO
 *  constituents live in a subdirectory of the repo (e.g.
 *  "development/Muscles.kif"), and OPFS's `getFileHandle` only accepts one
 *  path component — a `/` in the name throws `TypeError: Invalid filename`.
 *  That failure happens mid-loop in `saveKbCache`, *before* snapshot.bin/
 *  meta.json are written, so it silently aborted the entire cache save on
 *  every KB containing such a file, forcing a normal fetch+rebuild on every
 *  boot even though the cache directory looked partially populated. */
function opfsSafeName(name) {
  return encodeURIComponent(name);
}

/** Stable fingerprint of the current constituent SET (name+origin pairs) —
 *  changes on add/remove, independent of any file's content. */
function constituentsFingerprint() {
  return savedConstituents.map((c) => `${c.origin}:${c.name}`).sort().join('|');
}

/** `false` when any loaded constituent has no stable version signal to cache
 *  against (`url` origin) — caching is skipped entirely for that boot. */
function kbCacheEligible() {
  return savedConstituents.length > 0 && savedConstituents.every((c) => c.origin === 'sumo' || c.origin === 'file');
}

/**
 * Attempt a cache-hit boot: restore the KB from a cached snapshot and
 * populate `constituents` from cached ('sumo') / OPFS ('file') text,
 * skipping the fetch+ingest+promote loop entirely.
 *
 * Returns `true` on success — the caller still renders/routes, just skips
 * straight past the fetch loop and `reprocess()`. Returns `false` for any
 * reason at all (no cache yet, a stale one, a corrupt read, an unsupported
 * browser) and touches no state the normal boot path wouldn't also set, so
 * the caller can unconditionally fall through to it.
 */
async function tryRestoreFromCache() {
  if (!kbCacheEligible()) return false;
  // Offline / rate-limited: trust whatever's cached rather than fail the
  // whole boot — the normal path needs this same network access anyway, so a
  // cache miss here doesn't cost anything a fresh boot wasn't already risking.
  let info;
  try { info = await fetchLastCommitInfo(); } catch { info = null; }
  try {
    const dir = await getSumoCacheDir();
    const meta = JSON.parse(await (await (await dir.getFileHandle(SUMO_CACHE_META)).getFile()).text());
    if (info && meta.commitSha !== info.sha) return false;
    if (meta.fingerprint !== constituentsFingerprint()) return false;

    // Confirmed hit — only now touch the shared boot-progress counters. Doing
    // this any earlier corrupts them for the normal fetch loop on a MISS
    // (the common case): bootTotal got left at a tiny fixed number while
    // bootStep kept climbing once per file, so the bar rushed to 100% almost
    // immediately and then sat pinned there while the per-file status label
    // kept changing underneath it.
    bootStep = 0;
    bootTotal = 2;   // short, fixed sequence — unlike the fetch loop, not sized per constituent
    bootProgress('Restoring from cache…');
    const bytes = new Uint8Array(await (await (await dir.getFileHandle(SUMO_CACHE_SNAPSHOT)).getFile()).arrayBuffer());
    await call('restore', { bytes });

    const built = [];
    for (const { name, origin } of savedConstituents) {
      const text = origin === 'sumo'
        ? await (await (await dir.getFileHandle(opfsSafeName(name))).getFile()).text()
        : await fromOrigin(origin, name);
      built.push({ name, origin, text });
    }
    constituents = built;
    // The restored KB is already promoted — this is the read-only structural
    // pass reprocess() would otherwise run, not a rebuild, so it's cheap.
    diagnostics = (await call('validate')).diagnostics;
    bootProgress('Cache restored');
    return true;
  } catch {
    return false;   // no cache dir yet, a missing/corrupt entry, restore() rejected, …
  }
}

/**
 * Persist the current, just-promoted KB as the cache for next boot. Best
 * effort and fire-and-forget from the caller's perspective: any failure
 * (OPFS quota, an unsupported browser, offline) just means the next boot
 * does a normal fetch+ingest — never surfaced to the user.
 */
async function saveKbCache() {
  if (!kbCacheEligible()) return;
  try {
    const info = await fetchLastCommitInfo();
    if (!info?.sha) return;
    const bytes = (await call('snapshot')).bytes;
    const dir = await getSumoCacheDir();
    for (const { name, origin, text } of constituents) {
      if (origin === 'sumo') await writeOpfsFile(dir, opfsSafeName(name), text);
    }
    await writeOpfsFile(dir, SUMO_CACHE_SNAPSHOT, bytes);
    await writeOpfsFile(dir, SUMO_CACHE_META, JSON.stringify({
      commitSha: info.sha, fingerprint: constituentsFingerprint(),
    }));
  } catch (e) {
    console.warn('KB snapshot cache: failed to save', e);
  }
}

// Settings-modal maintenance action — a manual escape hatch for a stale/
// corrupt cache, not a feature to promote day-to-day. Only clears the
// persisted OPFS cache; the live in-memory KB is untouched, so a fresh boot
// (a manual reload) is what actually exercises the change.
$('clearCacheLink')?.addEventListener('click', async (e) => {
  e.preventDefault();
  const link = $('clearCacheLink');
  const original = link.textContent;
  try {
    if (opfsRoot) await opfsRoot.removeEntry(SUMO_CACHE_DIR, { recursive: true });
  } catch {
    // Nothing cached yet is not a failure — either way the cache is now clear.
  }
  sumoCacheDirHandle = null;   // the lazily-opened handle would otherwise point at a removed directory
  link.textContent = 'cache cleared';
  setTimeout(() => { link.textContent = original; }, 2000);
});

// -- Boot ---------------------------------------------------------------------

// Boot progress. Each constituent contributes two steps — the fetch and the
// ingest — so the bar keeps moving across a slow download instead of sitting at
// one value for the whole of a multi-MB file. Step 1 is the engine itself.
let bootStep = 0;
let bootTotal = 1;

/** Advance the bar one step and show `label` as the quiet line beneath it. */
function bootProgress(label) {
  bootStep += 1;
  const pct = Math.min(100, Math.round((bootStep / bootTotal) * 100));
  const fill = $('bootBarFill');
  if (fill) fill.style.width = `${pct}%`;
  $('bootBar')?.setAttribute('aria-valuenow', String(pct));
  const msg = $('overlayMsg');
  if (msg) msg.textContent = label;
}

async function boot() {
  try {
    bootTotal = 1 + savedConstituents.length * 2;
    $('overlayMsg').textContent = 'Starting the engine…';
    // Fetching + compiling the wasm is the longest single phase on a cold load
    // and reports no intermediate progress, so seed a visible sliver rather
    // than leaving the bar at a dead 0% for all of it.
    $('bootBarFill').style.width = '8%';
    // The worker resolves the optional Vampire runner against this; its own
    // URL sits in the bundle's asset directory, so it cannot derive the base.
    await call('boot', { baseUrl: new URL(BASE, location.href).href });
    bootProgress('Engine ready');
    opfsRoot = await navigator.storage.getDirectory();

    // Cache hit: the KB and every constituent's text are already restored —
    // skip the fetch+ingest+promote loop below entirely.
    if (await tryRestoreFromCache()) {
      $('overlay').remove();
      renderAll();
      applyRoute();
      restoreTests();
      return;
    }

    let i = 1;
    const total = savedConstituents.length;
    for (const { name, origin } of savedConstituents) {
      bootProgress(`Fetching ${name} (${i}/${total})`);
      const text = await fromOrigin(origin, name);
      bootProgress(`Reading ${name} (${i}/${total})`);
      await ingestConstituent(name, text, origin);   // ingest only — promote runs after
      i += 1;
    }
    $('overlay').remove();
    renderConstituents();
    refreshLangSelect();
    // Honour the URL now that the constituents exist — /edit?file=…&l=…
    // needs them loaded before it can select a file in the editor.
    applyRoute();
    restoreTests();
    reprocess();   // toast → promote all → validate → untoast (off the critical path)
  } catch (e) {
    $('overlayTitle').textContent = 'Failed to load SUMO';
    $('overlayMsg').textContent = '';
    $('overlayErr').textContent = String(e && e.message || e) + '  (Try checking your network connection.)';
    $('bootBar')?.remove();   // a stalled bar reads as "still working"
  }
}

// -- Tabs + URL routing -------------------------------------------------------
//
// Routing is path-based — /edit?file=Merge.kif&l=100 — served by Cloudflare
// Pages' _redirects (web/_redirects: `/* /index.html 200`), which rewrites any
// sub-path to index.html so the SPA router below can take over. `serve.sh`
// mirrors that fallback locally. GitHub Pages has no such rewrite, so a hard
// refresh on a non-root path 404s there; in-app navigation (pushState) is
// unaffected either way. `browse` is the default tab and stays at the bare
// mount path. Legacy `?tab=` bookmarks (the old query-string scheme) still
// resolve correctly.

const TABS = ['browse', 'kb', 'diagnostics', 'prover', 'audit', 'edit', 'history'];

// The path the app is mounted at — "/" locally and on Cloudflare, "/browse/"
// on Pages. Vite substitutes its configured `base` (always absolute, see
// vite.config.js). Do not derive this from the document or from
// `import.meta.url`: the bundled module lives in the asset directory, and
// under the SPA fallback the document's directory varies per route, either of
// which sends every deep link to the default tab.
const BASE = import.meta.env.BASE_URL;

function currentTab() {
  return document.querySelector('nav.tabs button[aria-selected="true"]')?.dataset.tab || 'browse';
}

/** The route encoded in the address bar: { tab, params }. The tab is a real
 *  path segment (/edit, /diagnostics, …); a legacy `?tab=` query param (old
 *  bookmarks/links, including the retired `?tab=home`) is honoured as a
 *  fallback when the path itself doesn't name a known tab. */
function routeFromLocation() {
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
function syncUrl(tab, params = new URLSearchParams(), { replace = false } = {}) {
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
 */
function showTab(name, { push = true, params } = {}) {
  if (promoting && PROMOTE_TABS.includes(name)) return; // greyed while post-processing
  // Navigating away from Edit — a tab-bar click, a citation's "open man page"
  // link, browser Back, anything that routes through here — always deflates
  // fullscreen first, whether or not `name` is actually 'edit' itself.
  if (name !== 'edit') setEditFullscreen(false);
  for (const btn of document.querySelectorAll('nav.tabs button')) {
    btn.setAttribute('aria-selected', String(btn.dataset.tab === name));
  }
  for (const p of document.querySelectorAll('.panel')) p.hidden = p.id !== `tab-${name}`;
  if (push) syncUrl(name, params ?? new URLSearchParams());
  if (name === 'browse') refreshHomeStats();
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
async function applyRoute() {
  const { tab, params } = routeFromLocation();
  showTab(tab, { push: false });

  if (tab === 'edit') {
    const file = params.get('file');
    const line = Number(params.get('l') || params.get('line'));
    await ensureEditorReady();
    if (file) {
      // Match on name alone — a deep link shouldn't have to know the origin.
      const c = constituents.find((x) => x.name === file);
      if (c) {
        $('editPicker').value = `${c.name}|${c.origin}`;
        onEditPickerChange();
      } else {
        $('editLog').style.color = 'var(--bad)';
        $('editLog').textContent = `${file} is not among the loaded constituents.`;
      }
    }
    if (monacoEditor && Number.isFinite(line) && line > 0) {
      monacoEditor.revealLineInCenter(line);
      monacoEditor.setPosition({ lineNumber: line, column: 1 });
      monacoEditor.focus();
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

document.querySelector('nav.tabs').addEventListener('click', (e) => {
  const btn = e.target.closest('button');
  if (btn && btn.getAttribute('aria-disabled') !== 'true') showTab(btn.dataset.tab);
});
document.addEventListener('click', (e) => {
  const jump = e.target.closest('.jump');
  if (jump && jump.getAttribute('aria-disabled') !== 'true') { e.preventDefault(); showTab(jump.dataset.tab); }
});
// A symbol inside a rendered formula (man-page refs, proof/audit steps) opens
// its man page from any tab. preventDefault also cancels the enclosing
// <summary>'s expand toggle when the symbol sits inside a citation row.
document.addEventListener('click', async (e) => {
  const link = e.target.closest('.sym-link');
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

// -- Keyboard shortcuts: `/` focuses search, Esc backs out of a man page ------

document.addEventListener('keydown', (e) => {
  const t = e.target;
  const typing = t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement ||
    t instanceof HTMLSelectElement || (t instanceof HTMLElement && t.isContentEditable);
  if (e.key === '/' && !e.ctrlKey && !e.metaKey && !e.altKey && !typing) {
    e.preventDefault();
    showTab('browse');
    $('q').focus();
    $('q').select();
  } else if (e.key === 'Escape' && currentTab() === 'browse' && (!typing || t === $('q'))) {
    const params = new URLSearchParams(location.search);
    const q = params.get('q') || $('q').value.trim();
    if (params.get('sym')) {
      // Man page open → back to the search results (or the welcome state).
      updateParams({ q });
      if (q) runSearch(q); else setBrowseHome(true);
    } else if (t === $('q') && $('q').value) {
      $('q').value = '';
      updateParams({});
      setBrowseHome(true);
    }
  }
});

// -- Theme toggle: explicit choice wins over the OS preference ----------------

const THEME_KEY = 'sumoBrowserTheme';
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

const APP_REPO = { owner: 'ontologyportal', repo: 'sigma-rs' };

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

let appVersion = null;

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

const SEEN_VERSION_KEY = 'sumoBrowserSeenVersion';

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

/** Push a history entry for `tab` with `params` and render it. The three
 *  cross-tab jumps (editor, diagnostics, documentation) all go through here so
 *  they agree on push-vs-replace and on param naming. */
function navigate(tab, obj) {
  const p = new URLSearchParams();
  for (const [k, v] of Object.entries(obj || {})) if (v != null && v !== '') p.set(k, String(v));
  syncUrl(tab, p);
  return applyRoute();
}

/** Replace the query on the current tab, so the address bar stays shareable
 *  without pushing a history entry for every search/file switch. */
function updateParams(obj) {
  const p = new URLSearchParams();
  for (const [k, v] of Object.entries(obj)) if (v != null && v !== '') p.set(k, String(v));
  syncUrl(currentTab(), p, { replace: true });
}

async function withBusy(button, fn) {
  const prev = button.textContent;
  button.disabled = true; button.textContent = 'Working…';
  try { await fn(); }
  catch (e) { $('kbLog').textContent = String(e && e.message || e); $('kbLog').style.color = 'var(--bad)'; }
  finally { button.disabled = false; button.textContent = prev; }
}

// -- Knowledge base: constituent management -----------------------------------

function renderConstituents() {
  const kb = $('kbTotals');
  if (kb) kb.innerHTML = `<b>${constituents.length}</b> constituent(s) loaded · ${diagnostics.length} diagnostic(s)`;
  const list = $('loadedList');
  if (!list) return;
  const ORIGIN_LABELS = { sumo: 'GitHub', file: 'Local File', url: 'Remote URL' };
  list.innerHTML = constituents.map((c) => `
    <li class="loaded-row">
      <span><a class="file-open" data-name="${escAttr(c.name)}" title="Open in the editor">${esc(c.name)}</a>
        <span class="hint">${(c.text.length / 1000).toFixed(0)} KB · ${ORIGIN_LABELS[c.origin] || esc(c.origin)}</span></span>
      ${c.name === MERGE ? '<span class="hint">core</span>' : `<a class="rm" data-name="${esc(c.name)}" data-source="${c.origin}">remove</a>`}
    </li>`).join('');
}

$('loadedList').addEventListener('click', (e) => {
  const rm = e.target.closest('.rm');
  if (rm) { $('kbLog').textContent = ''; removeConstituent(rm.dataset.name, rm.dataset.source); return; }
  const open = e.target.closest('.file-open');
  if (open) navigate('edit', { file: open.dataset.name });
});

// -- .kif.tq tests -------------------------------------------------------------
//
// Tests share the constituent import channels (GitHub picker / URL / upload)
// but are a separate collection: a test's (query …) must never be ingested as
// an axiom. Running one reuses the prove pipeline with the test's own axioms,
// query, and (time N) budget.

const TQ_SETTING = 'sumoTests';
let savedTests = JSON.parse(localStorage.getItem(TQ_SETTING) || '[]');   // [{ name, origin }]
let tqTests = [];   // [{ name, origin, text, parsed, outcome }]
const isTestFile = (name) => /\.tq$/i.test(name);

async function addTest(name, text, origin) {
  if (tqTests.some((t) => t.name === name)) return { added: false, notices: [`${name}: already imported`] };
  const { test } = await call('parseTest', { name, text });
  tqTests.push({ name, origin, text, parsed: test, outcome: null });
  if (!savedTests.some((t) => t.name === name && t.origin === origin)) {
    savedTests.push({ name, origin });
    localStorage.setItem(TQ_SETTING, JSON.stringify(savedTests));
  }
  renderTests();
  return { added: true, notices: [] };
}

async function removeTest(name, origin) {
  tqTests = tqTests.filter((t) => t.name !== name || t.origin !== origin);
  savedTests = savedTests.filter((t) => t.name !== name || t.origin !== origin);
  localStorage.setItem(TQ_SETTING, JSON.stringify(savedTests));
  if (origin === 'file') {
    try { const h = await opfsRoot.getFileHandle(name); await h.remove(); } catch { /* already gone */ }
  }
  renderTests();
}

async function restoreTests() {
  for (const { name, origin } of savedTests) {
    try { await addTest(name, await fromOrigin(origin, name), origin); }
    catch (e) { console.warn(`test ${name}: ${e.message || e}`); }
  }
}

function gradeTest(parsed, result) {
  const exp = parsed.expectedProof;
  const conclusiveNo = ['Disproved', 'CounterSatisfiable', 'Consistent'].includes(result.status);
  if (exp === true) {
    return result.proved ? { cls: 'ok', label: 'pass' } : { cls: 'bad', label: `no proof (${result.status})` };
  }
  if (exp === false) {
    if (result.proved) return { cls: 'bad', label: 'proved — expected no' };
    return conclusiveNo ? { cls: 'ok', label: 'pass' } : { cls: 'mut', label: result.status };
  }
  return { cls: 'mut', label: result.status };   // no yes/no expectation: informational
}

async function runTest(t) {
  const cfg = proverConfig(t.parsed.timeout ? { timeLimitSecs: t.parsed.timeout } : {});
  const { result } = await call('prove', {
    assertions: t.parsed.axiomKif,
    query: t.parsed.queryKif,
    config: cfg,
    session: '__tq_test__',
  });
  t.outcome = { ...gradeTest(t.parsed, result), status: result.status };
}

function renderTests() {
  const list = $('testsList');
  if (!list) return;
  $('testsEmpty').hidden = tqTests.length > 0;
  $('runAllTests').hidden = tqTests.length === 0;
  list.innerHTML = tqTests.map((t, i) => {
    const p = t.parsed;
    const missing = (p.extraFiles || []).filter((f) => !constituents.some((c) => c.name.endsWith(f)));
    const o = t.outcome;
    return `
    <li class="loaded-row">
      <span>
        <span class="sym">${esc(t.name)}</span>
        ${p.note ? `<span class="hint">${esc(p.note)}</span>` : ''}
        ${p.queryKif ? `<code class="hint">${esc(p.queryKif.length > 60 ? p.queryKif.slice(0, 60) + '…' : p.queryKif)}</code>` : '<span class="hint">no (query)</span>'}
        ${p.expectedProof != null ? `<span class="hint">expects ${p.expectedProof ? 'yes' : 'no'}</span>` : ''}
        ${p.expectedAnswer ? `<span class="hint">answer: ${esc(p.expectedAnswer.join(' '))}</span>` : ''}
        ${missing.length ? `<span class="hint" style="color:var(--warn)">needs ${esc(missing.join(', '))}</span>` : ''}
        ${o ? `<span class="tq-badge tq-${o.cls}">${esc(o.label)}</span>` : ''}
      </span>
      <span>
        ${p.queryKif ? `<a class="tq-run" data-i="${i}">run</a> · ` : ''}
        <a class="tq-open" data-i="${i}">open</a> ·
        <a class="rm tq-rm" data-i="${i}">remove</a>
      </span>
    </li>`;
  }).join('');
}

$('testsList')?.addEventListener('click', async (e) => {
  const a = e.target.closest('a[data-i]');
  if (!a) return;
  const t = tqTests[Number(a.dataset.i)];
  if (!t) return;
  if (a.classList.contains('tq-rm')) return removeTest(t.name, t.origin);
  if (a.classList.contains('tq-open')) {
    await ensureProverEditors().catch(() => {});
    if (assertionsEditor) {
      assertionsEditor.setValue(t.parsed.axiomKif || '');
      queryEditor.setValue(t.parsed.queryKif || '');
    } else {
      $('assertions').value = t.parsed.axiomKif || '';
      $('pquery').value = t.parsed.queryKif || '';
    }
    showTab('prover');
    return;
  }
  if (a.classList.contains('tq-run')) {
    a.textContent = 'running…';
    try { await runTest(t); } catch (err) { t.outcome = { cls: 'bad', label: String(err.message || err).slice(0, 60) }; }
    renderTests();
  }
});

$('runAllTests')?.addEventListener('click', (e) => withBusy(e.target, async () => {
  let pass = 0, ran = 0;
  for (const t of tqTests) {
    if (!t.parsed.queryKif) continue;
    $('testsLog').textContent = `Running ${t.name}…`;
    try { await runTest(t); } catch (err) { t.outcome = { cls: 'bad', label: String(err.message || err).slice(0, 60) }; }
    ran += 1;
    if (t.outcome.cls === 'ok') pass += 1;
    renderTests();
  }
  $('testsLog').textContent = `${pass}/${ran} passed.`;
}));

// -- Standard constituent sets ------------------------------------------------
//
// File lists mirror the Sigma XML configuration. Order is preserved from it:
// ingest is order-independent here (everything is promoted together at the
// end), but keeping it makes the two lists diffable against the source.

const PRESETS = {
  minimal: {
    label: 'Minimal SUMO',
    files: ['Merge.kif', 'Mid-level-ontology.kif', 'english_format.kif', 'domainEnglishFormat.kif'],
  },
  full: {
    label: 'Full SUMO',
    files: [
      'english_format.kif', 'domainEnglishFormat.kif', 'ArabicCulture.kif', 'Anatomy.kif',
      'arteries.kif', 'Biography.kif', 'Cars.kif', 'Catalog.kif', 'Communications.kif',
      'ComputerInput.kif', 'ComputingBrands.kif', 'CountriesAndRegions.kif', 'Dining.kif',
      'Economy.kif', 'emotion.kif', 'engineering.kif', 'Facebook.kif', 'FinancialOntology.kif',
      'Food.kif', 'Geography.kif', 'Government.kif', 'Hotel.kif', 'Justice.kif', 'Languages.kif',
      'Law.kif', 'Media.kif', 'Medicine.kif', 'Merge.kif', 'Mid-level-ontology.kif',
      'MilitaryDevices.kif', 'Military.kif', 'MilitaryPersons.kif', 'MilitaryProcesses.kif',
      'Music.kif', 'development/Muscles.kif', 'naics.kif', 'People.kif', 'pictureList.kif',
      'pictureList-ImageNet.kif', 'QoSontology.kif', 'Sports.kif', 'TransnationalIssues.kif',
      'Transportation.kif', 'TransportDetail.kif', 'UXExperimentalTerms.kif',
      'VirusProteinAndCellPart.kif', 'Weather.kif', 'WMD.kif', 'capabilities.kif',
    ],
  },
};

/** Fetch every file, up to `limit` at once, returning texts in list order.
 *  A per-file failure is captured rather than thrown so one bad file cannot
 *  abandon the other forty-eight. Sequential fetching would make Full SUMO
 *  a minutes-long wait. */
async function fetchAllTexts(files, limit, onDone) {
  const out = new Array(files.length);
  let next = 0, done = 0;
  const worker = async () => {
    for (let i = next++; i < files.length; i = next++) {
      try { out[i] = await fetchText(rawUrl(files[i])); }
      catch (e) { out[i] = e instanceof Error ? e : new Error(String(e)); }
      onDone(++done);
    }
  };
  await Promise.all(Array.from({ length: Math.min(limit, files.length) }, worker));
  return out;
}

async function loadPreset(key) {
  const preset = PRESETS[key];
  const buttons = [$('loadMinimal'), $('loadFull')];
  buttons.forEach((b) => { b.disabled = true; });
  const note = $('presetNote');
  try {
    // A preset describes a whole KB, so it replaces rather than merges.
    constituents = [];
    savedConstituents = [];
    localStorage.setItem(SUMO_FILE_SETTING, JSON.stringify(savedConstituents));
    await call('newSession');
    renderConstituents();

    const total = preset.files.length;
    note.style.color = '';
    note.textContent = `Fetching ${preset.label} — 0/${total}…`;
    const texts = await fetchAllTexts(preset.files, 6,
      (n) => { note.textContent = `Fetching ${preset.label} — ${n}/${total}…`; });

    const failed = [];
    for (let i = 0; i < preset.files.length; i++) {
      const name = preset.files[i], text = texts[i];
      if (text instanceof Error) { failed.push(`${name}: ${text.message}`); continue; }
      note.textContent = `Reading ${name} (${i + 1}/${total})…`;
      try { await ingestConstituent(name, text, 'sumo'); }
      catch (e) { failed.push(`${name}: ${e.message || e}`); }
    }
    renderConstituents();
    note.textContent = `Axiomatizing ${constituents.length} constituent(s)…`;
    await reprocess();

    note.style.color = failed.length ? 'var(--bad)' : '';
    note.textContent = failed.length
      ? `${preset.label}: loaded ${constituents.length}/${total}, ${failed.length} failed — ${failed[0]}`
      : `${preset.label} loaded — ${constituents.length} constituents.`;
  } catch (e) {
    note.style.color = 'var(--bad)';
    note.textContent = String(e && e.message || e);
  } finally {
    buttons.forEach((b) => { b.disabled = false; });
  }
}

$('loadMinimal').onclick = () => loadPreset('minimal');
$('loadFull').onclick = () => loadPreset('full');

async function loadSumoCatalog() {
  if (sumoCatalog) return;
  $('pickerStatus').textContent = 'loading file list…';
  try {
    // Via the shared client so a rate-limited response raises rather than
    // silently yielding `undefined.tree`.
    const tree = await githubApi(`/repos/${SUMO.owner}/${SUMO.repo}/git/trees/${SUMO.ref}?recursive=1`);
    sumoCatalog = (tree.tree || []).filter((e) => e.type === 'blob' && /\.kif(\.tq)?$/i.test(e.path)).map((e) => e.path).sort();
    renderPicker();
  } catch (e) {
    $('pickerStatus').textContent = 'could not load file list: ' + (e.message || e);
  }
}

function renderPicker() {
  const filter = $('fileFilter').value.toLowerCase();
  const loaded = new Set(constituents.filter((c) => c.origin === 'sumo').map((c) => c.name));
  for (const t of tqTests) if (t.origin === 'sumo') loaded.add(t.name);
  const avail = sumoCatalog.filter((p) => !loaded.has(p) && p.toLowerCase().includes(filter));
  $('sumoPicker').innerHTML = avail.map((p) => `<option value="${esc(p)}">${esc(p)}</option>`).join('');
  $('pickerStatus').textContent = `${avail.length} file(s) available`;
}

$('fileFilter').addEventListener('input', () => { if (sumoCatalog) renderPicker(); });

$('addSumo').onclick = (e) => withBusy(e.target, async () => {
  const paths = [...$('sumoPicker').selectedOptions].map((o) => o.value);
  if (!paths.length) { $('kbLog').textContent = 'Select one or more files first.'; return; }
  // Ingest (fetch + parse) under the busy button — no toast yet. Fetches run
  // batched, like the presets: a multi-select of a dozen files is otherwise a
  // dozen serial round-trips.
  let added = 0, notices = 0; const failed = [];
  $('kbLog').style.color = '';
  const texts = await fetchAllTexts(paths, 6,
    (n) => { $('kbLog').textContent = `Fetching — ${n}/${paths.length}…`; });
  for (let i = 0; i < paths.length; i++) {
    const path = paths[i], text = texts[i];
    if (text instanceof Error) { failed.push(`${path}: ${text.message}`); continue; }
    try {
      const r = isTestFile(path) ? await addTest(path, text, 'sumo') : await ingestConstituent(path, text);
      if (r.added) added += 1; notices += r.notices.length;
    }
    catch (err) { failed.push(`${path}: ${err.message || err}`); }
  }
  renderConstituents();
  $('kbLog').textContent = `Ingested ${added}/${paths.length} constituent(s); axiomatizing…`;
  await reprocess();   // toast → promote → validate → untoast
  if (failed.length) { $('kbLog').style.color = 'var(--bad)'; $('kbLog').textContent = `Added ${added}/${paths.length}; ${failed.length} failed — ${failed[0]}`; }
  else $('kbLog').textContent = `Added ${added}/${paths.length} constituent(s)` + (notices ? ` (${notices} load notice(s))` : '') + '.';
});

$('addUrl').onclick = (e) => withBusy(e.target, async () => {
  const url = $('kbUrl').value.trim();
  if (!url) { $('kbLog').textContent = 'Enter a URL first.'; return; }
  const text = await fetchText(url);
  $('kbLog').style.color = '';
  if (isTestFile(url)) {
    const r = await addTest(url, text, 'url');
    $('kbLog').textContent = r.added ? `Imported test ${url}.` : r.notices.join(' | ');
    return;
  }
  const r = await ingestConstituent(url, text, 'url');
  renderConstituents();
  $('kbLog').textContent = r.added ? `Ingested ${url}; axiomatizing…` : r.notices.join(' | ');
  if (r.added) await reprocess();
});

$('kbFile').onchange = (e) => withBusy($('addUrl'), async () => {
  const file = e.target.files[0];
  if (!file) return;
  const text = await file.text();
  if (opfsRoot === null) throw new Error('File system not yet initialized');
  const handle = await opfsRoot.getFileHandle(file.name, { create: true });
  const stream = await handle.createWritable();
  await stream.write(text);
  await stream.close();
  $('kbLog').style.color = '';
  if (isTestFile(file.name)) {
    const r = await addTest(file.name, text, 'file');
    $('kbLog').textContent = r.added ? `Imported test ${file.name}.` : r.notices.join(' | ');
    return;
  }
  const r = await ingestConstituent(file.name, text, 'file');
  renderConstituents();
  $('kbLog').textContent = r.added ? `Ingested ${file.name}; axiomatizing…` : r.notices.join(' | ');
  if (r.added) await reprocess();
});

// -- Home: search → results → man page ----------------------------------------

$('searchForm').addEventListener('submit', (e) => {
  e.preventDefault();
  const q = $('q').value.trim();
  updateParams({ q });
  runSearch(q);
});

// Search-as-you-type: results render live off a short debounce; Enter still
// works via the submit handler above (same runSearch, seq-guarded below).
let searchDebounce = 0;
$('q').addEventListener('input', () => {
  clearTimeout(searchDebounce);
  searchDebounce = setTimeout(() => {
    const q = $('q').value.trim();
    updateParams({ q });
    runSearch(q);
  }, 150);
});

/** Show/hide the Browse tab's empty state (welcome card + KB stats). */
function setBrowseHome(show) {
  const el = $('browseHome');
  if (el) el.hidden = !show;
  if (show) $('browseView').innerHTML = '';
}

// The welcome card's example queries.
document.addEventListener('click', (e) => {
  const a = e.target.closest('a.try-q');
  if (!a) return;
  e.preventDefault();
  $('q').value = a.textContent;
  updateParams({ q: a.textContent });
  runSearch(a.textContent);
});

// Only the newest in-flight search may render: typing "proc" fires four
// requests and the slower ones must not clobber the freshest results.
let searchSeq = 0;

async function runSearch(query) {
  if (!query) { setBrowseHome(true); return; }
  const seq = ++searchSeq;
  setBrowseHome(false);
  let { hits } = await call('search', { query, limit: 100, language: uiLanguage });
  if (seq !== searchSeq) return;
  // The language filter is a preference, not a wall: a KB documented in
  // another language must stay searchable on the default (English) setting.
  let langNote = '';
  if (hits.length === 0) {
    ({ hits } = await call('search', { query, limit: 100 }));
    if (seq !== searchSeq) return;
    if (hits.length) langNote = ` (no ${esc(langLabel(uiLanguage))} matches — showing all languages)`;
  }
  if (hits.length === 0) {
    $('browseView').innerHTML = `<div class="card hint">No matches for <code>${esc(query)}</code>.</div>`;
    return;
  }
  const items = hits.map((h) => `
    <li>
      <a class="sym open" data-sym="${esc(h.symbol)}">${esc(h.symbol)}</a>
      <span class="kinds">${h.kinds.join(' · ') || h.source} · rank ${h.rank.toFixed(0)}</span>
      ${h.text ? `<div class="snippet">${boldifyDoc(h.text)}</div>` : ''}
    </li>`).join('');
  $('browseView').innerHTML =
    `<div class="card">
       <div class="hint" style="margin-bottom:6px">${hits.length} result${hits.length === 1 ? '' : 's'} for <code>${esc(query)}</code>${langNote}</div>
       <ul class="results">${items}</ul>
     </div>`;
}

/** Human label for a language symbol from the header selector, falling back
 *  to the raw symbol name. */
function langLabel(symbol) {
  const opt = [...($('langSelect')?.options ?? [])].find((o) => o.value === symbol);
  return opt ? opt.textContent : symbol;
}

$('browseView').addEventListener('click', (e) => {
  const link = e.target.closest('.open');
  if (link) { e.preventDefault(); updateParams({ sym: link.dataset.sym }); openManPage(link.dataset.sym); }
});

/** Render `&%Symbol` cross-reference markers as bold plain text (no link) —
 *  used in search-result snippets, where each row already links to the symbol. */
function boldifyDoc(text) {
  return String(text).split(/(&%[A-Za-z0-9_-]+)/).map((part) => {
    const m = part.match(/^&%([A-Za-z0-9_-]+)$/);
    return m ? `<b>${esc(m[1])}</b>` : esc(part);
  }).join('');
}

/** Turn `&%Symbol` cross-reference markers in documentation text into man-page links. */
function linkifyDoc(text) {
  return String(text).split(/(&%[A-Za-z0-9_-]+)/).map((part) => {
    const m = part.match(/^&%([A-Za-z0-9_-]+)$/);
    return m ? `<a class="open xref" data-sym="${esc(m[1])}">${esc(m[1])}</a>` : esc(part);
  }).join('');
}

/** Doc entries in the selected language, falling back to English then to all,
 *  so a symbol never renders blank just because it lacks the chosen language. */
function docsForLanguage(entries) {
  const pick = (lang) => entries.filter((d) => d.language === lang);
  return pick(uiLanguage).length ? pick(uiLanguage)
    : pick('EnglishLanguage').length ? pick('EnglishLanguage')
    : entries;
}

async function openManPage(symbol) {
  const { page: p } = await call('manpage', { symbol });
  setBrowseHome(false);
  if (!p) { $('browseView').innerHTML = `<div class="card hint">No man page for <code>${esc(symbol)}</code>.</div>`; return; }

  const docs = (v) => docsForLanguage(v).map((d) => `<div>${linkifyDoc(d.text)} <span class="hint">(${esc(d.language)})</span></div>`).join('');
  const sig = () => {
    const parts = [];
    if (p.arity != null) parts.push(`arity ${p.arity < 0 ? 'variable' : p.arity}`);
    for (const d of p.domains) parts.push(`arg ${d.position}: ${esc(d.sort.class)}${d.sort.subclass ? ' (class)' : ''}`);
    if (p.range) parts.push(`range: ${esc(p.range.class)}`);
    return parts.length ? parts.join('<br>') : '<span class="hint">none declared</span>';
  };
  const field = (title, html) => `<div class="field"><h3>${title}</h3><div class="val">${html}</div></div>`;

  const refsNote = (p) => {
    const shown = p.references.length;
    const omitted = Math.max(0, p.appears_in_count - shown);
    if (!shown) {
      return omitted
        ? `appears only in ${omitted} documentation/taxonomy/format entr${omitted === 1 ? 'y' : 'ies'} (not shown)`
        : 'appears in no formulas';
    }
    const excl = omitted
      ? ` (${omitted} documentation/taxonomy/format entr${omitted === 1 ? 'y' : 'ies'} omitted)`
      : '';
    return `appears in ${shown} formula${shown === 1 ? '' : 's'}${excl}, listed below`;
  };
  const refsBlock = p.references.length
    ? `<div class="hint" style="margin-bottom:4px">${refsNote(p)}</div>
       ${refFilterControl(p.references)}
       <div id="refListWrap">${renderRefList(p.references, '', p.name)}</div>`
    : `<div class="hint">${refsNote(p)}</div>`;

  $('browseView').innerHTML = `
    <div class="card man">
      <div class="man-head">
        <a class="hint back" style="cursor:pointer">← back to results</a>
        <h2>${esc(p.name)}</h2>
        <div class="kinds">${p.kinds.join(' · ') || 'symbol'}</div>
      </div>
      ${p.documentation.length ? field('Documentation', docs(p.documentation)) : ''}
      ${field('Taxonomy', taxonomyWidget(p))}
      ${(p.arity != null || p.domains.length || p.range) ? field('Signature', sig()) : ''}
      ${p.term_format.length ? field('Term format', docs(p.term_format)) : ''}
      ${p.format.length ? field('Format', docs(p.format)) : ''}
      ${field('References', refsBlock)}
    </div>`;
  $('browseView').querySelector('.back').onclick = () => runSearch($('q').value.trim());
  const refSel = $('refFilter');
  if (refSel) refSel.onchange = () => { $('refListWrap').innerHTML = renderRefList(p.references, refSel.value, p.name); };
  fillAncestors(p);   // fire-and-forget: the chain above the symbol streams in
}

// -- Taxonomy tree ------------------------------------------------------------
//
// Replaces the flat Parents/Children link lists: the ancestor chain renders
// above the current symbol (walked lazily upward after first paint), and the
// descendants below expand on demand via the lightweight `taxonomy` call.

/** The edge kinds the tree walks, in legend order. */
const TAX_RELATIONS = ['subclass', 'instance', 'subrelation', 'subAttribute'];

/** Color-coded pill for an edge's relation kind (colors keyed off data-rel). */
function relChip(relation) {
  return `<span class="tax-rel" data-rel="${escAttr(relation)}">${esc(relation)}</span>`;
}

/** Legend row naming each edge kind present in the rendered tree. */
function taxLegend(rels) {
  const present = [...TAX_RELATIONS.filter((r) => rels.has(r)),
                   ...[...rels].filter((r) => !TAX_RELATIONS.includes(r))];
  return present.length
    ? `<div class="tax-legend"><span class="hint">edges:</span> ${present.map(relChip).join(' ')}</div>`
    : '';
}

/** How many direct children the diagram shows before eliding the rest. */
const TAX_MAX_CHILDREN = 60;

/** The initial widget: legend + an empty graph container that
 *  `fillAncestors` populates with the Cytoscape diagram. */
function taxonomyWidget(p) {
  const rels = new Set([...p.parents, ...p.children].map((e) => e.relation));
  return `<div class="taxtree">
    ${taxLegend(rels)}
    <div id="taxGraph" class="graph-container tax-graph"><span class="hint">tracing taxonomy…</span></div>
    <div id="taxTip" class="hint graph-tip">tap a node to open its man page · scroll to zoom</div>
  </div>`;
}

/** Walk the ENTIRE ancestor graph upward from `p` (all parents, transitively,
 *  to the roots — Entity, ultimately), add the direct children, and render it
 *  all as a Cytoscape diagram: roots at the top, the current symbol
 *  highlighted, edges color-coded by relation kind. Tapping any other node
 *  opens its man page. Bounded and cycle-safe. */
async function fillAncestors(p) {
  const container = document.getElementById('taxGraph');
  if (!container) return;

  // BFS upward, collecting every symbol's parent edges.
  const parentEdges = new Map([[p.name, p.parents]]);       // sym → [{relation, parent}]
  const seen = new Set([p.name]);
  const queue = [];
  for (const e of p.parents) if (!seen.has(e.parent)) { seen.add(e.parent); queue.push(e.parent); }
  for (let n = 0; queue.length && n < 80; n++) {
    const sym = queue.shift();
    let tax;
    try { ({ tax } = await call('taxonomy', { symbol: sym })); } catch { continue; }
    const ps = tax?.parents ?? [];
    parentEdges.set(sym, ps);
    for (const e of ps) if (!seen.has(e.parent)) { seen.add(e.parent); queue.push(e.parent); }
  }
  if (!container.isConnected) return;  // view re-rendered while walking — stale target

  // Elements: ancestor nodes + current + (capped) direct children; one edge
  // per taxonomy assertion, tagged with its relation for the color styling.
  const rels = new Set();
  const nodes = new Map();                                  // sym → node element
  const addNode = (sym, kind) => {
    if (!nodes.has(sym)) nodes.set(sym, { data: { id: sym, label: sym, kind } });
  };
  const edges = [];
  const addEdge = (child, relation, parent) => {
    rels.add(relation);
    edges.push({ data: { id: `${child}→${parent}#${relation}`, source: child, target: parent, rel: relation } });
  };
  addNode(p.name, 'current');
  for (const [child, ps] of parentEdges) {
    if (child !== p.name) addNode(child, 'ancestor');
    for (const e of ps) { addNode(e.parent, 'ancestor'); addEdge(child, e.relation, e.parent); }
  }
  const shownKids = p.children.slice(0, TAX_MAX_CHILDREN);
  for (const e of shownKids) { addNode(e.parent, 'child'); addEdge(e.parent, e.relation, p.name); }
  const elided = p.children.length - shownKids.length;

  try {
    const cytoscape = await loadCytoscape();
    if (!container.isConnected) return;
    container.textContent = '';
    const cy = cytoscape({
      container,
      elements: [...nodes.values(), ...edges],
      style: taxonomyGraphStyle(isDarkTheme()),
      // Edges point child → parent, so rank bottom-to-top puts Entity on top.
      layout: { name: 'dagre', rankDir: 'BT', nodeSep: 14, rankSep: 46 },
      wheelSensitivity: 0.2,
    });
    const tip = document.getElementById('taxTip');
    cy.on('tap', 'node', (e) => {
      const sym = e.target.id();
      if (sym !== p.name) navigate('browse', { sym });
    });
    if (tip) {
      cy.on('mouseover', 'edge', (e) => { tip.textContent = `(${e.target.data('rel')} ${e.target.source().id()} ${e.target.target().id()})`; });
      if (elided) tip.textContent += ` · showing ${shownKids.length} of ${p.children.length} children`;
    }
  } catch (err) {
    container.textContent = 'Failed to load taxonomy graph: ' + (err && err.message || err);
    return;
  }

  const legend = container.parentElement.querySelector('.tax-legend');
  const fresh = taxLegend(rels);
  if (legend) legend.outerHTML = fresh; else container.insertAdjacentHTML('beforebegin', fresh);
}

/** Cytoscape style for the taxonomy diagram: node emphasis by role (current /
 *  ancestor / child), edge color by relation kind — matching the legend pills. */
function taxonomyGraphStyle(dark) {
  const relColor = {
    subclass:     dark ? '#6ea8ff' : '#2d6cdf',   // --accent
    instance:     dark ? '#4ac26b' : '#1a7f37',   // --ok
    subrelation:  dark ? '#d2a8ff' : '#8250df',   // --op
    subAttribute: dark ? '#e3b341' : '#9a6700',   // --warn
  };
  const base = cytoscapeStyle(dark);
  return [
    ...base,
    { selector: 'node', style: { 'font-size': 11, 'text-max-width': '140px' } },
    { selector: 'node[kind="current"]', style: {
        'border-width': 3,
        'font-weight': 'bold',
      } },
    { selector: 'node[kind="child"]', style: {
        'border-style': 'dashed',
      } },
    ...Object.entries(relColor).map(([rel, color]) => ({
      selector: `edge[rel="${rel}"]`,
      style: { 'line-color': color, 'target-arrow-color': color },
    })),
  ];
}

/** `true` when the page currently renders dark — the explicit header choice
 *  wins, the OS preference is the fallback. */
function isDarkTheme() {
  const t = document.documentElement.dataset.theme;
  if (t) return t === 'dark';
  return window.matchMedia?.('(prefers-color-scheme: dark)').matches ?? false;
}

/** Reference-filter <select>, offering only the categories present among `refs`.
 *  Value encoding: '' = all; 'fact' = any plain fact (a relation atom, possibly
 *  under `not`); 'fact:<n>' = plain fact with the symbol at argument n (0 = the
 *  relation itself); '=>' '<=>' 'and' 'or' = a top-level logical operator.
 *  `kind`/`arg_pos` come from the core classification in `manpage_to_js`. */
function refFilterControl(refs) {
  const facts = refs.filter((r) => r.kind === 'fact');
  const positions = [...new Set(facts.map((r) => r.arg_pos).filter((n) => n != null))].sort((a, b) => a - b);
  const ops = [['=>', 'Implications (⇒)'], ['<=>', 'Biconditionals (⇔)'], ['and', 'Conjunctions (and)'], ['or', 'Disjunctions (or)']]
    .filter(([k]) => refs.some((r) => r.kind === k));
  const opt = (v, label) => `<option value="${esc(v)}">${esc(label)}</option>`;
  const posLabel = (n) => n === 0 ? 'symbol as the relation (arg 0)' : `symbol as argument ${n}`;
  const count = (pred) => refs.filter(pred).length;
  return `<label class="ref-filter"><span class="hint">Filter</span>
    <select id="refFilter">
      ${opt('', `All (${refs.length})`)}
      ${facts.length ? opt('fact', `Plain facts (${facts.length})`) : ''}
      ${positions.map((n) => opt(`fact:${n}`, `  ${posLabel(n)} (${count((r) => r.kind === 'fact' && r.arg_pos === n)})`)).join('')}
      ${ops.map(([k, label]) => opt(k, `${label} (${count((r) => r.kind === k)})`)).join('')}
    </select></label>`;
}

/** Subset of `refs` matching an encoded filter value (see refFilterControl). */
function filterRefs(refs, filter) {
  if (!filter) return refs;
  if (filter === 'fact') return refs.filter((r) => r.kind === 'fact');
  if (filter.startsWith('fact:')) {
    const n = Number(filter.slice(5));
    return refs.filter((r) => r.kind === 'fact' && r.arg_pos === n);
  }
  return refs.filter((r) => r.kind === filter);
}

/** The filtered <ol> of reference rows for man-page subject `name`. */
function renderRefList(refs, filter, name) {
  const shown = filterRefs(refs, filter);
  if (!shown.length) return '<span class="hint">no formulas match this filter</span>';
  const rows = shown.map((r) => kifCiteRow({ kif: r.kif, file: r.file, line: r.line, focusSymbol: name })).join('');
  return `<ol class="refs">${rows}</ol>`;
}

// -- Diagnostics --------------------------------------------------------------

const DIAG_SEV_ORDER = ['error', 'warning', 'info', 'hint'];

/** `true` if `d` matches every active diagFilter dimension except `exceptDim`
 *  (pass `null` to apply all four). The single predicate both
 *  `filteredDiagnostics` (what's actually shown) and `diagnosticsExcept`
 *  (each dropdown's faceted counts) build on, so the two never drift apart. */
function matchesDiagFilter(d, exceptDim) {
  return (exceptDim === 'file'     || !diagFilter.file     || d.file === diagFilter.file) &&
    (exceptDim === 'severity' || !diagFilter.severity || d.severity === diagFilter.severity) &&
    (exceptDim === 'kind'     || !diagFilter.kind     || d.kind === diagFilter.kind) &&
    (exceptDim === 'code'     || !diagFilter.code     || d.code === diagFilter.code);
}

/** `{d, i}` pairs (`i` = index into the full `diagnostics` array) matching
 *  every active filter — shared by rendering and deep-link scrolling so both
 *  agree on what's "shown" (and therefore which page it's on). */
function filteredDiagnostics() {
  return diagnostics
    .map((d, i) => ({ d, i }))
    .filter(({ d }) => matchesDiagFilter(d, null));
}

/** Diagnostics matching every active filter EXCEPT `dim` — the pool `dim`'s
 *  own dropdown counts its options against, so a count reflects what the
 *  OTHER active filters already narrowed to (faceted: e.g. once Severity is
 *  set to Error, the File dropdown's counts are error counts per file, not
 *  raw totals) rather than freezing at the unfiltered totals. */
function diagnosticsExcept(dim) {
  return diagnostics.filter((d) => matchesDiagFilter(d, dim));
}

/**
 * Populate `sel` from `field`'s distinct values within `pool`, each option
 * labelled with its occurrence count — `"All <label> (N)"` plus one row per
 * value, `"value (n)"`. `opts.order` fixes a leading sort order (severity's
 * error/warning/info/hint); unlisted values fall back to alphabetical after
 * it. `opts.labelFn` formats a value for display (raw value if omitted).
 *
 * Resets `diagFilter[stateKey]` to '' when its current value has no match in
 * `pool` — a sibling filter changing (or a fresh validate()) can invalidate
 * an existing selection; this is the one place that's discovered and healed,
 * mirroring how a stale `?file=`/`?kind=` from the URL is handled the same
 * way on first load.
 */
function populateDiagFilter(sel, stateKey, field, pool, allLabel, opts = {}) {
  if (!sel) return;
  const { order, labelFn } = opts;
  const counts = new Map();
  for (const d of pool) {
    const v = d[field];
    if (!v) continue;
    counts.set(v, (counts.get(v) || 0) + 1);
  }
  const values = [...counts.keys()].sort((a, b) => {
    const ia = order ? order.indexOf(a) : -1, ib = order ? order.indexOf(b) : -1;
    if (ia !== -1 || ib !== -1) return (ia === -1 ? 999 : ia) - (ib === -1 ? 999 : ib);
    return a < b ? -1 : a > b ? 1 : 0;
  });
  sel.innerHTML = `<option value="">${allLabel} (${pool.length})</option>` +
    values.map((v) => `<option value="${esc(v)}">${esc(labelFn ? labelFn(v) : v)} (${counts.get(v)})</option>`).join('');
  if (diagnostics.length && diagFilter[stateKey] && !counts.has(diagFilter[stateKey])) {
    diagFilter[stateKey] = '';
  }
  sel.value = diagFilter[stateKey];
}

function renderDiagnostics() {
  populateDiagFilter($('diagFileFilter'), 'file',     'file',     diagnosticsExcept('file'),     'All files');
  populateDiagFilter($('diagSevFilter'),  'severity',  'severity', diagnosticsExcept('severity'), 'All severities',
    { order: DIAG_SEV_ORDER, labelFn: (s) => s[0].toUpperCase() + s.slice(1) });
  populateDiagFilter($('diagKindFilter'), 'kind',     'kind',     diagnosticsExcept('kind'),      'All types');
  populateDiagFilter($('diagCodeFilter'), 'code',     'code',     diagnosticsExcept('code'),      'All codes');

  // Active-filter count on the Filter button, so it's meaningful collapsed.
  const activeCount = ['file', 'severity', 'kind', 'code'].filter((k) => diagFilter[k]).length;
  const badge = $('diagFilterBadge');
  if (badge) badge.textContent = activeCount || '';

  const filtered = filteredDiagnostics();

  const errs = diagnostics.filter((d) => d.severity === 'error').length;

  // Count chip on the Diagnostics tab button — glanceable KB health from any
  // tab. Hidden at zero; colored by worst severity present.
  const tabBtn = document.querySelector('nav.tabs [data-tab="diagnostics"]');
  if (tabBtn) {
    tabBtn.querySelector('.tab-badge')?.remove();
    if (diagnostics.length) {
      tabBtn.insertAdjacentHTML('beforeend',
        `<span class="tab-badge${errs ? ' err' : ''}">${diagnostics.length}</span>`);
    }
  }

  const filterActive = diagFilter.file || diagFilter.severity || diagFilter.kind || diagFilter.code;
  const sum = $('diagSummary');
  if (sum) sum.innerHTML = diagnostics.length
    ? (filterActive
        ? `<b>${filtered.length}</b> of <b>${diagnostics.length}</b> diagnostic${diagnostics.length === 1 ? '' : 's'} shown`
        : `<b>${diagnostics.length}</b> diagnostic${diagnostics.length === 1 ? '' : 's'}`) +
      (errs ? ` (${errs} error${errs === 1 ? '' : 's'} total)` : '') +
      ` — click a <span class="loc">file:line</span> to open it in the editor`
    : 'No diagnostics — the loaded KB is clean.';

  // Clamp the page to the current (possibly just-filtered/shrunk) result set
  // so a filter change or a smaller re-validation never strands the view
  // past the end.
  const pageCount = Math.max(1, Math.ceil(filtered.length / DIAG_PAGE_SIZE));
  diagFilter.page = Math.min(Math.max(0, diagFilter.page), pageCount - 1);
  const pageStart = diagFilter.page * DIAG_PAGE_SIZE;
  const pageItems = filtered.slice(pageStart, pageStart + DIAG_PAGE_SIZE);

  const list = $('diagList');
  if (list) {
    list.innerHTML = pageItems.length ? pageItems.map(({ d, i }) => {
      const loc = d.file ? locLink(d.file, d.line, 'loc') : '<span class="loc">(no location)</span>';
      return `<div class="diag" data-i="${i}" data-sev="${esc(d.severity)}">
        <div class="diag-head">
          <span class="sev ${esc(d.severity)}">${esc(d.severity)}</span>
          ${loc}
          <span class="code">[${esc(d.kind)}/${esc(d.code)}]</span>
          ${ghAnchor(d.file, d.line)}
          <span class="msg">${esc(d.message)}</span>
        </div>
      </div>`;
    }).join('') : `<div class="hint">${diagnostics.length ? 'No diagnostics match the current filters.' : ''}</div>`;
  }
  renderDiagPager(filtered.length, pageCount);
}

/** Prev/Next pager beneath the diagnostics list — hidden entirely when
 *  everything fits on one page. */
function renderDiagPager(total, pageCount) {
  const el = $('diagPager');
  if (!el) return;
  if (pageCount <= 1) { el.innerHTML = ''; return; }
  const page = diagFilter.page;
  const from = total ? page * DIAG_PAGE_SIZE + 1 : 0;
  const to = Math.min(total, from + DIAG_PAGE_SIZE - 1);
  el.innerHTML = `
    <button class="btn ghost" id="diagPrev" type="button" ${page === 0 ? 'disabled' : ''}>‹ Prev</button>
    <span class="hint">${from}–${to} of ${total} · page ${page + 1} of ${pageCount}</span>
    <button class="btn ghost" id="diagNext" type="button" ${page >= pageCount - 1 ? 'disabled' : ''}>Next ›</button>
  `;
}

$('diagPager')?.addEventListener('click', (e) => {
  if (e.target.id === 'diagPrev') diagFilter.page -= 1;
  else if (e.target.id === 'diagNext') diagFilter.page += 1;
  else return;
  renderDiagnostics();
  syncDiagUrl();
  $('diagList').scrollIntoView({ block: 'nearest' });
});

/**
 * GitHub *blame* deep-link for a SUMO-sourced constituent, else null.
 *
 * Blame rather than blob: it lands on the same line but with per-line author,
 * date and commit attribution — "who last changed this axiom" — for free. The
 * API route to that data is GraphQL `Blob.blame`, which requires a token even
 * for public repos and so is unusable from a static, unauthenticated page.
 */
function ghLink(file, line) {
  const c = constituents.find((x) => x.name === file);
  if (!c || c.origin !== 'sumo') return null;
  return `https://github.com/${SUMO.owner}/${SUMO.repo}/blame/${SUMO.ref}/${file}#L${line}`;
}

/** The blame anchor for a citation, or '' when the source is not on GitHub. */
function ghAnchor(file, line) {
  const url = ghLink(file, line);
  return url
    ? `<a class="hint gh" href="${url}" target="_blank" rel="noopener"
         title="Who last changed this line (GitHub blame)">blame ↗</a>`
    : '';
}

/** Open `file` in the Edit tab with the caret on `line`. Routed through the URL
 *  so the jump is a real history entry and the resulting view is shareable. */
function openInEditor(file, line) {
  return navigate('edit', { file, l: line > 0 ? line : null });   // Back returns here
}

/**
 * A `file:line` citation. Rendered as a link that opens the editor there when
 * the file is a loaded constituent, and as plain text otherwise — a proof can
 * cite a synthetic/CNF source, or an axiom from a file the user has since
 * removed, and neither is openable. `extraClass` carries the caller's styling.
 */
function locLink(file, line, extraClass = 'hint ref-loc') {
  if (!file) return '';
  const label = `${esc(file)}:${line}`;
  if (!constituents.some((c) => c.name === file)) return `<span class="${extraClass}">${label}</span>`;
  return `<a class="${extraClass} jump-src" data-file="${esc(file)}" data-line="${line}">${label}</a>`;
}

// One delegated handler for every file:line citation — diagnostics, proof
// steps, audit contradictions, and man-page references all route through here.
document.addEventListener('click', (e) => {
  if (e.target.closest('a.gh')) return;   // let the GitHub link open normally
  const a = e.target.closest('a.jump-src');
  if (!a) return;
  e.preventDefault();
  openInEditor(a.dataset.file, Number(a.dataset.line));
});

$('revalidate').onclick = () => withBusy($('revalidate'), async () => {
  diagnostics = (await call('validate')).diagnostics;
  renderDiagnostics();
});

/** Mirror the active filters (and page, when not the first) into the address
 *  bar so a filtered/paginated view is shareable. `p` is 1-based in the URL —
 *  friendlier to read/type than the internal 0-based index. */
function syncDiagUrl() {
  updateParams({
    file: diagFilter.file,
    sev:  diagFilter.severity,
    kind: diagFilter.kind,
    code: diagFilter.code,
    p:    diagFilter.page ? diagFilter.page + 1 : null,
  });
}

/**
 * Apply ?file / ?sev / ?kind / ?code / ?p / ?l to the Diagnostics tab. Called
 * both when the route is applied and again once validation finishes — on a
 * cold load the route runs before any diagnostics exist, so the first pass
 * has nothing to filter or scroll to. `?l` (deep-link to one diagnostic)
 * takes precedence over `?p` — it jumps to whatever page that diagnostic
 * actually falls on.
 */
function applyDiagRouteParams() {
  const params = new URLSearchParams(location.search);
  if ((params.get('tab') || 'browse') !== 'diagnostics') return;
  diagFilter.file     = params.get('file') || '';
  diagFilter.severity = params.get('sev')  || '';
  diagFilter.kind     = params.get('kind') || '';
  diagFilter.code     = params.get('code') || '';
  diagFilter.page     = Math.max(0, (Number(params.get('p')) || 1) - 1);
  // A shared/deep-linked URL that already carries a filter should show it
  // expanded, not hide the active filter behind a collapsed button.
  if (diagFilter.file || diagFilter.severity || diagFilter.kind || diagFilter.code) {
    togglePanel('diagFilterBtn', 'diagFilterPanel', true);
  }
  renderDiagnostics();
  const line = Number(params.get('l'));
  if (Number.isFinite(line) && line > 0) scrollToDiagnostic(line);
}

$('diagFilterBtn').addEventListener('click', () => togglePanel('diagFilterBtn', 'diagFilterPanel'));

$('diagFileFilter').addEventListener('change', () => {
  diagFilter.file = $('diagFileFilter').value; diagFilter.page = 0; renderDiagnostics(); syncDiagUrl();
});
$('diagSevFilter').addEventListener('change', () => {
  diagFilter.severity = $('diagSevFilter').value; diagFilter.page = 0; renderDiagnostics(); syncDiagUrl();
});
$('diagKindFilter').addEventListener('change', () => {
  // Picking a type invalidates a code chosen under a different type — clear
  // it rather than leave a stale, now-impossible combination active.
  diagFilter.kind = $('diagKindFilter').value; diagFilter.code = ''; diagFilter.page = 0;
  renderDiagnostics(); syncDiagUrl();
});
$('diagCodeFilter').addEventListener('change', () => {
  diagFilter.code = $('diagCodeFilter').value; diagFilter.page = 0; renderDiagnostics(); syncDiagUrl();
});

/**
 * Jump to whichever page contains the diagnostic nearest `line` (within the
 * active file/severity filter) and flash it. Nearest rather than exact: the
 * caller's line comes from an edited buffer, whose line numbers drift from
 * the KB's as soon as anything above is inserted.
 */
function scrollToDiagnostic(line) {
  const filtered = filteredDiagnostics();
  let bestPos = -1, bestDist = Infinity;
  filtered.forEach(({ d }, pos) => {
    const dist = Math.abs((d.line || 0) - line);
    if (dist < bestDist) { bestDist = dist; bestPos = pos; }
  });
  if (bestPos < 0) return;
  diagFilter.page = Math.floor(bestPos / DIAG_PAGE_SIZE);
  renderDiagnostics();

  const el = $('diagList').querySelector(`.diag[data-i="${filtered[bestPos].i}"]`);
  if (!el) return;
  el.scrollIntoView({ block: 'center', behavior: 'smooth' });
  el.classList.add('diag-target');
  setTimeout(() => el.classList.remove('diag-target'), 2200);
}

// The IDE's diagnostic count links here, filtered to the file being edited.
document.addEventListener('click', (e) => {
  const a = e.target.closest('a.jump-diag');
  if (!a) return;
  e.preventDefault();
  navigate('diagnostics', { file: a.dataset.file, l: a.dataset.line });
});

// -- KIF syntax highlighting (textarea + mirrored <pre> overlay) --------------

const KIF_KEYWORDS = new Set(['and', 'or', 'not', 'forall', 'exists', 'equal']);

/** Prover-internal vocabulary that has no man page: Skolem constants
 *  (`sK1`, `esk2_0`, …) and scope-qualified variable interning keys
 *  (`Human__15551`) that can leak into prover-emitted KIF. Linking them
 *  would only offer dead ends. */
function isInternalSymbol(word) {
  return /^(sK|esk|epred)\d/.test(word) || /__\d+$/.test(word);
}
const KIF_TOKEN_RE = /(;[^\n]*)|("(?:[^"\\]|\\.)*")|([()])|([?@][A-Za-z0-9_-]+)|(-?\d+(?:\.\d+)?)|(<=>|=>|=)|([A-Za-z_][A-Za-z0-9_-]*)/g;

function highlightKif(src, { focusSymbol, linkSymbols } = {}) {
  let out = '', last = 0, m, afterOpenParen = false;
  KIF_TOKEN_RE.lastIndex = 0;
  while ((m = KIF_TOKEN_RE.exec(src))) {
    out += esc(src.slice(last, m.index));
    const [, comment, str, paren, variable, num, op, word] = m;
    if (comment) { out += `<span class="tok-com">${esc(comment)}</span>`; afterOpenParen = false; }
    else if (str) { out += `<span class="tok-str">${esc(str)}</span>`; afterOpenParen = false; }
    else if (paren) { out += `<span class="tok-paren">${esc(paren)}</span>`; afterOpenParen = paren === '('; }
    else if (variable) { out += `<span class="tok-var">${esc(variable)}</span>`; afterOpenParen = false; }
    else if (num) { out += `<span class="tok-num">${esc(num)}</span>`; afterOpenParen = false; }
    else if (op) { out += `<span class="tok-kw">${esc(op)}</span>`; afterOpenParen = false; }
    else if (word) {
      const isKw = KIF_KEYWORDS.has(word);
      let tok;
      if (isKw) tok = `<span class="tok-kw">${esc(word)}</span>`;
      else if (afterOpenParen) tok = `<span class="tok-fn">${esc(word)}</span>`; // relation/function symbol
      else tok = esc(word);
      if (focusSymbol && word === focusSymbol) {
        tok = `<span class="sym-focus">${tok}</span>`;      // the viewed symbol: highlight, don't self-link
      } else if (linkSymbols && !isKw && !isInternalSymbol(word)) {
        tok = `<a class="sym-link" data-sym="${escAttr(word)}">${tok}</a>`;  // symbol → its man page
      }
      out += tok;
      afterOpenParen = false;
    }
    last = KIF_TOKEN_RE.lastIndex;
  }
  out += esc(src.slice(last));
  return out + '\n'; // trailing line so a source ending in \n doesn't collapse height vs. the textarea
}

// -- Ask/Tell Monaco panes -----------------------------------------------------
//
// The two prover inputs are compact Monaco editors sharing the Edit tab's kif
// language, theme, and marker pipeline. The textareas in the markup remain the
// working fallback if the Monaco CDN is unreachable; paneValue() reads from
// whichever is active.

let assertionsEditor = null;
let queryEditor = null;
let proverEditorsPromise = null;

function paneValue(name) {
  if (name === 'assertions') return assertionsEditor ? assertionsEditor.getValue() : $('assertions').value;
  return queryEditor ? queryEditor.getValue() : $('pquery').value;
}

function ensureProverEditors() {
  if (!proverEditorsPromise) {
    proverEditorsPromise = createProverEditors().catch((e) => {
      proverEditorsPromise = null;
      throw e;
    });
  }
  return proverEditorsPromise;
}

function darkThemeActive() {
  const t = document.documentElement.dataset.theme;
  return t ? t === 'dark' : !!window.matchMedia?.('(prefers-color-scheme: dark)').matches;
}

async function createProverEditors() {
  if (assertionsEditor) return;
  monaco = await loadMonaco();
  const m = monaco;
  const dark = darkThemeActive();
  const opts = {
    language: 'kif',
    theme: dark ? 'kif-dark' : 'kif-light',
    automaticLayout: true,
    minimap: { enabled: false },
    lineNumbers: 'off',
    folding: false,
    glyphMargin: false,
    lineDecorationsWidth: 6,
    scrollBeyondLastLine: false,
    overviewRulerLanes: 0,
    hideCursorInOverviewRuler: true,
    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
    fontSize: 13,
    // Only the KB's own symbols (via kifCompletionProvider) should be
    // suggested, not arbitrary strings already typed in the buffer.
    wordBasedSuggestions: 'off',
  };
  const mount = (mountId, taId) => {
    const ta = $(taId);
    const box = $(mountId);
    const ed = m.editor.create(box, { ...opts, value: ta.value });
    ta.hidden = true;
    box.hidden = false;
    ed.onDidChangeModelContent(scheduleScratchValidate);
    return ed;
  };
  assertionsEditor = mount('assertionsEd', 'assertions');
  queryEditor = mount('pqueryEd', 'pquery');
  runScratchValidate();
}

let scratchValidateTimer = 0;
let scratchValidateBusy = false;
let scratchValidateQueued = false;

function scheduleScratchValidate() {
  clearTimeout(scratchValidateTimer);
  scratchValidateTimer = setTimeout(runScratchValidate, 400);
}

async function runScratchValidate() {
  if (!assertionsEditor) return;
  if (scratchValidateBusy) { scratchValidateQueued = true; return; }
  scratchValidateBusy = true;
  try {
    const r = await call('validateScratch', {
      assertions: assertionsEditor.getValue(),
      query: queryEditor.getValue(),
    });
    if (!assertionsEditor) return;
    monaco.editor.setModelMarkers(assertionsEditor.getModel(), 'sigma', diagsToMarkers(r.assertions));
    monaco.editor.setModelMarkers(queryEditor.getModel(), 'sigma', diagsToMarkers(r.query));
  } catch (e) { console.warn('validateScratch:', e.message || e); }
  finally {
    scratchValidateBusy = false;
    if (scratchValidateQueued) { scratchValidateQueued = false; runScratchValidate(); }
  }
}

// -- Proof graph (Cytoscape.js, lazy CDN load) ---------------------------------
//
// Interactive rendering of a proof/contradiction's `{index, rule, premises,
// kif}[]` steps, used by both Ask/Tell's proof and each Audit contradiction.

const CYTOSCAPE_VERSION = '3.34.0';
const CYTOSCAPE_DAGRE_VERSION = '4.0.0';

let cytoscapeLoadPromise = null;

function loadScript(src) {
  return new Promise((resolve, reject) => {
    const s = document.createElement('script');
    s.src = src;
    s.onload = resolve;
    s.onerror = () => reject(new Error(`failed to load ${src}`));
    document.head.appendChild(s);
  });
}

/**
 * Load a UMD bundle so it lands on `window`, not in a module registry.
 *
 * Monaco's loader installs a global `define` with `.amd`, and a UMD wrapper
 * that sees one registers itself as an anonymous AMD module and never sets its
 * browser global — so `window.cytoscape` stays undefined and instantiating it
 * throws "cytoscape is not a function". It only bites when the Edit tab loaded
 * Monaco before the first proof graph, which is what made it look intermittent.
 *
 * Hiding `define`/`exports`/`module` for the duration forces the wrapper down
 * its browser-global branch. They are restored in a `finally`, so an aborted
 * load cannot leave Monaco's loader detached.
 */
async function loadUmdGlobal(src) {
  const saved = { define: window.define, exports: window.exports, module: window.module };
  window.define = undefined; window.exports = undefined; window.module = undefined;
  try {
    await loadScript(src);
  } finally {
    window.define = saved.define; window.exports = saved.exports; window.module = saved.module;
  }
}

/** Loads cytoscape.js then cytoscape-dagre (which self-registers onto `window.cytoscape`). */
function loadCytoscape() {
  if (cytoscapeLoadPromise) return cytoscapeLoadPromise;
  cytoscapeLoadPromise = (async () => {
    // Never pull `define` out from under an in-flight Monaco load.
    if (monacoLoadPromise) { try { await monacoLoadPromise; } catch { /* its own problem */ } }
    await loadUmdGlobal(`https://cdn.jsdelivr.net/npm/cytoscape@${CYTOSCAPE_VERSION}/dist/cytoscape.min.js`);
    await loadUmdGlobal(`https://cdn.jsdelivr.net/npm/cytoscape-dagre@${CYTOSCAPE_DAGRE_VERSION}/dist/cytoscape-dagre.min.js`);
    if (typeof window.cytoscape !== 'function') {
      throw new Error('cytoscape did not register as a browser global');
    }
    return window.cytoscape;
  })().catch((e) => {
    cytoscapeLoadPromise = null;   // a cached rejection would break the graph for the session
    throw e;
  });
  return cytoscapeLoadPromise;
}

/** Proof/contradiction steps → Cytoscape elements: one node per step, one edge per premise. */
function stepsToElements(steps) {
  const nodes = steps.map((s) => ({
    data: { id: `n${s.index}`, label: `${s.index + 1}. ${s.rule}`, kif: s.kif },
  }));
  const edges = steps.flatMap((s) => s.premises.map((p) => ({
    data: { id: `n${p}-n${s.index}`, source: `n${p}`, target: `n${s.index}` },
  })));
  return [...nodes, ...edges];
}

function cytoscapeStyle(dark) {
  return [
    { selector: 'node', style: {
        'background-color': dark ? '#1e2024' : '#f7f7f8',
        'border-color':     dark ? '#6ea8ff' : '#2d6cdf',
        'border-width': 1.5,
        shape: 'round-rectangle',
        label: 'data(label)',
        color: dark ? '#e6e6e6' : '#1a1a1a',
        'font-family': 'ui-monospace, SFMono-Regular, Menlo, monospace',
        'font-size': 10,
        'text-valign': 'center', 'text-halign': 'center',
        'text-wrap': 'wrap', 'text-max-width': '160px',
        padding: '8px', width: 'label', height: 'label',
      } },
    { selector: 'edge', style: {
        width: 1.5,
        'line-color':          dark ? '#9aa0a6' : '#666666',
        'target-arrow-color':  dark ? '#9aa0a6' : '#666666',
        'target-arrow-shape': 'triangle',
        'curve-style': 'bezier',
      } },
    { selector: 'node:selected', style: {
        'border-color': dark ? '#d2a8ff' : '#8250df',
        'border-width': 3,
      } },
  ];
}

/** Create a Cytoscape instance inside `container` from `steps`, top-down (dagre) layout. */
async function renderProofGraph(container, steps, tipEl) {
  const cytoscape = await loadCytoscape();
  container.textContent = '';
  const dark = isDarkTheme();
  const cy = cytoscape({
    container,
    elements: stepsToElements(steps),
    style: cytoscapeStyle(dark),
    layout: { name: 'dagre', rankDir: 'TB', nodeSep: 20, rankSep: 40 },
    wheelSensitivity: 0.2,
  });
  if (tipEl) {
    cy.on('tap mouseover', 'node', (e) => { tipEl.textContent = e.target.data('kif'); });
  }
  return cy;
}

/** Wire a `<details>` element to lazily render its proof graph the first time it's opened, and re-fit on later opens. */
function wireProofGraph(details, container, tipEl, getSteps) {
  const render = async () => {
    if (details._cy) { details._cy.destroy(); details._cy = null; }
    container.textContent = 'Loading graph…';
    try {
      details._cy = await renderProofGraph(container, getSteps(), tipEl);
    } catch (err) {
      container.textContent = 'Failed to load graph: ' + (err && err.message || err);
    }
  };
  details.addEventListener('toggle', () => {
    if (!details.open) return;
    if (details._cy) { details._cy.resize(); details._cy.fit(); }
    else render();
  });
  return () => { if (details.open) render(); else if (details._cy) { details._cy.destroy(); details._cy = null; } };
}

// -- Prover settings (the wasm `Config`) ---------------------------------------
//
// The cog next to Prove toggles a panel over the same knobs `Config` exposes.
// Values are read fresh on each run and sent to the worker, which builds the
// Config there — the page never holds a wasm object.

// One descriptor per Config knob, driving the form, the summary and the object
// sent to the worker. Adding a knob is one row here plus the markup, rather
// than four coordinated edits where a typo'd id fails silently.
const CFG_KNOBS = [
  { key: 'timeLimitSecs', id: 'cfgTimeLimit',    dflt: 30   },
  { key: 'maxSteps',      id: 'cfgMaxSteps',     dflt: 4000 },
  { key: 'maxLits',       id: 'cfgMaxLits',      dflt: 8    },
  { key: 'forwardClose',  id: 'cfgForwardClose', dflt: true },
  { key: 'wantProof',     id: 'cfgWantProof',    dflt: true },
  { key: 'profile',       id: 'cfgProfile',      dflt: false },
  // 0 means "engine default" (sent to the worker as `null` — see makeConfig
  // in sigma.worker.js — never as a literal 0% budget). 100 searches the
  // whole KB, same as the old standalone "disable axiom selection" toggle.
  { key: 'selectionTolerancePct', id: 'cfgSelectionPct', dflt: 0 },
];
const CFG_DEFAULTS = Object.fromEntries(CFG_KNOBS.map((k) => [k.key, k.dflt]));

/** Current settings as a plain object for the worker. Numeric fields coerce to
 *  u32-safe ints; `overrides` wins, so callers with their own input (Audit's
 *  time limit) get the same coercion instead of redoing it. */
function proverConfig(overrides = {}) {
  const cfg = {};
  for (const { key, id, dflt } of CFG_KNOBS) {
    if (typeof dflt === 'boolean') { cfg[key] = $(id).checked; continue; }
    const raw = key in overrides ? overrides[key] : $(id).value;
    const v = Math.floor(Number(raw));
    cfg[key] = Number.isFinite(v) && v >= 0 ? v : dflt;
  }
  return cfg;
}

function applyProverConfig(c) {
  for (const { key, id, dflt } of CFG_KNOBS) {
    const el = $(id);
    if (typeof dflt === 'boolean') el.checked = c[key]; else el.value = c[key];
  }
  renderCfgSummary();
}

/** One-line summary next to the cog, so non-default settings are visible without opening the panel. */
function renderCfgSummary() {
  const c = proverConfig();
  const diffs = Object.keys(CFG_DEFAULTS).filter((k) => c[k] !== CFG_DEFAULTS[k]);
  $('proverCfgSummary').textContent = diffs.length
    ? `${c.timeLimitSecs}s · ${c.maxSteps} steps · ${diffs.length} non-default`
    : `${c.timeLimitSecs}s · ${c.maxSteps} steps · defaults`;
}

/** Disclosure panel: flip (or force) visibility and keep aria-expanded paired
 *  with it, so the two never drift apart. */
function togglePanel(btnId, panelId, force) {
  const panel = $(panelId);
  const open = force !== undefined ? force : panel.hidden;
  panel.hidden = !open;
  $(btnId).setAttribute('aria-expanded', String(open));
  return open;
}

// The panel is one shared instance toggled by either tab's cog — keep BOTH
// buttons' aria-expanded in sync with it (only the one actually clicked would
// otherwise update, leaving the other stale after a tab switch).
function toggleProverSettings(force) {
  const open = togglePanel('proverSettingsBtn', 'proverSettings', force);
  $('auditSettingsBtn').setAttribute('aria-expanded', String(open));
  return open;
}
$('proverSettingsBtn').onclick = () => toggleProverSettings();
$('auditSettingsBtn').onclick = () => toggleProverSettings();
$('cfgReset').onclick = () => applyProverConfig(CFG_DEFAULTS);
for (const { id } of CFG_KNOBS) $(id).addEventListener('input', renderCfgSummary);

// -- Backend-specific settings visibility -------------------------------------
//
// Vampire is a fixed-strategy refutation search, not a tunable given-clause
// loop — most of the native backend's knobs (given-clause budget, literal
// cap, forward closure, profiling) are silently ignored if sent to it. Grey
// them out of the shared settings panel (and Audit's native-only "max found",
// which Vampire can't honor — it's a single-shot run, not an enumerator) when
// Vampire is selected, and show Vampire's own knob (raw CLI args) instead —
// rather than leave editable controls that quietly do nothing.
function updateBackendVisibility() {
  const vampire = $('proverBackend').value === 'vampire';
  document.querySelectorAll('[data-native-only]').forEach((el) => { el.hidden = vampire; });
  document.querySelectorAll('[data-vampire-only]').forEach((el) => { el.hidden = !vampire; });
  $('proverBackendHint').textContent = vampire
    ? 'Vampire is a fixed refutation search — only time limit, selection budget, and extra CLI args apply.'
    : 'given-clause knobs below apply to the native backend only';
  // A downloadable TPTP input only exists for a completed Vampire run, and a
  // prior one (if any) was for whichever backend was selected at the time —
  // switching away invalidates it rather than leaving a stale download.
  $('downloadVampireTptp').hidden = true;
}
$('proverBackend').addEventListener('change', updateBackendVisibility);
updateBackendVisibility();

/** Live "N% of axioms" / "engine default" label under the selection-budget slider. */
function renderSelectionPctLabel() {
  const pct = Number($('cfgSelectionPct').value);
  $('cfgSelectionPctVal').textContent = pct === 0
    ? 'engine default — % of axioms a query-relevant selection may admit (also applies to Vampire; 100% searches the whole KB)'
    : `${pct}% of axioms admitted into a query-relevant selection (also applies to Vampire; 100% searches the whole KB)`;
}
$('cfgSelectionPct').addEventListener('input', renderSelectionPctLabel);
renderSelectionPctLabel();
renderCfgSummary();

// -- Prover: tell + ask -------------------------------------------------------

$('saveTq').onclick = () => {
  const query = paneValue('query').trim();
  if (!query) { $('proverCfgSummary').textContent = 'Enter a query first.'; return; }
  const text = formatTest({
    timeout: proverConfig().timeLimitSecs,
    assertions: paneValue('assertions'),
    query,
    expectedProof: true,
  });
  const blob = new Blob([text], { type: 'text/plain' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url; a.download = 'test.kif.tq';
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
};

// The exact TPTP problem text handed to Vampire for the most recent Ask/Tell
// run — `proveVampire` returns it alongside the result (computed anyway to
// run the query, previously discarded); `downloadVampireTptp` below just
// hands back what's already in memory, no extra worker round-trip.
let lastVampireTptp = null;

$('prove').onclick = async () => {
  const btn = $('prove');
  const vampire = $('proverBackend').value === 'vampire';
  btn.disabled = true; btn.textContent = 'Proving…';
  lastVampireTptp = null;
  $('downloadVampireTptp').hidden = true;
  try {
    let result;
    if (vampire) {
      const res = await call('proveVampire', {
        assertions: paneValue('assertions').trim(),
        query: paneValue('query'),
        timeLimitSecs: proverConfig().timeLimitSecs,
        selectionTolerancePct: proverConfig().selectionTolerancePct,
        extraArgs: $('cfgVampireArgs').value.trim(),
      });
      result = res.result;
      lastVampireTptp = res.tptp;
      $('downloadVampireTptp').hidden = false;
    } else {
      ({ result } = await call('prove', {
        assertions: paneValue('assertions').trim(),
        query: paneValue('query'),
        config: proverConfig(),
        session: 'user-assertions',
      }));
    }
    renderProof(result, vampire ? 'Vampire' : 'SUPr');
  } catch (e) {
    $('proverResult').hidden = false;
    $('pStatus').textContent = 'Error'; $('pStatus').className = 'status InputError';
    $('pBackendBadge').textContent = '';
    $('pSteps').textContent = String(e && e.message || e);
    $('pProof').innerHTML = ''; $('pRaw').textContent = ''; $('pGraphDot').textContent = '';
    $('pProseSlot').innerHTML = '';
    lastAskProof = [];
    invalidateAskGraph();
  } finally {
    btn.disabled = false; btn.textContent = 'Prove';
  }
};

$('downloadVampireTptp').onclick = () => {
  if (!lastVampireTptp) return;
  downloadText('vampire-input.tptp', lastVampireTptp);
};

let lastAskProof = [];
const invalidateAskGraph = wireProofGraph(
  $('pGraphDetails'), $('pGraphContainer'), $('pGraphTip'), () => lastAskProof);

// -- Shared proof rendering (Ask/Tell and Audit) ------------------------------
//
// Both a refutation proof and an audit contradiction are the same thing — a
// `{index, rule, premises, kif, file, line}[]` transcript — so they render
// through one code path: rule + derivation, highlighted KIF, source citation.

/** "(from steps 1, 3)" — the premise back-references the graph draws as edges.
 *  Step indices are 0-based on the wire; the <ol> and the graph both label from
 *  1, so shift for display. */
function premiseRefs(s) {
  if (!s.premises || !s.premises.length) return '';
  const label = s.premises.length === 1 ? 'step' : 'steps';
  return ` <span class="hint">(from ${label} ${s.premises.map((p) => p + 1).join(', ')})</span>`;
}

/** One KIF-citation row (an <li> for an `ol.refs` list), shared by the man-page
 *  reference list and the proof/contradiction step list: an optional `header`
 *  line, the highlighted KIF (with `focusSymbol` subtly highlighted), and a
 *  `file:line` + blame footer shown only when a source location is known.
 *  Clicking the row expands a natural-language paraphrase of the formula,
 *  rendered lazily in the currently selected language. */
function kifCiteRow({ kif, file, line, header, focusSymbol }) {
  const loc = locLink(file, line);
  const gh = ghAnchor(file, line);
  const meta = loc || gh ? `<div class="ref-meta">${loc}${gh}</div>` : '';
  return `<li>
    <details class="cite">
      <summary>
        ${header ? `<div class="hint">${header}</div>` : ''}
        <pre class="ref-kif">${highlightKif(kif, { focusSymbol, linkSymbols: true }).replace(/\n$/, '')}</pre>
      </summary>
      <div class="nl" data-kif="${escAttr(kif)}"></div>
    </details>
    ${meta}
  </li>`;
}

// Lazily render a citation row's natural-language paraphrase when it is
// expanded, re-rendering if the language changed since the last time it opened.
// `toggle` doesn't bubble, so listen in the capture phase.
document.addEventListener('toggle', (e) => {
  const d = e.target;
  if (!(d instanceof HTMLDetailsElement) || !d.classList.contains('cite') || !d.open) return;
  const nl = d.querySelector('.nl');
  if (!nl || nl.dataset.lang === uiLanguage) return;
  nl.dataset.lang = uiLanguage;
  nl.textContent = 'rendering…';
  call('renderNl', { kif: nl.dataset.kif, language: uiLanguage })
    .then(({ text }) => { nl.textContent = text && text.trim() ? text : 'no paraphrase available'; })
    // A failed call is not a verdict — clear the language stamp so the next
    // expand retries instead of treating the failure as a cached answer.
    .catch(() => { nl.dataset.lang = ''; nl.textContent = 'paraphrase failed — reopen to retry'; });
}, true);

/** One proof/contradiction step as an <li>. `pos` is the 0-based fallback when a
 *  step carries no explicit `index` — the number shown must match `premiseRefs`
 *  and the graph node labels, which both count from 1. */
function proofStepRow(s, pos) {
  const n = (s.index != null ? s.index : pos) + 1;
  const header = `<span class="step-num">${n}.</span> ${esc(s.rule)}${premiseRefs(s)}`;
  return kifCiteRow({ kif: s.kif, file: s.file, line: s.line, header });
}

const renderProofSteps = (steps) => steps.map((s, i) => proofStepRow(s, i)).join('');

/** The "shown by bare name" note under a prose block, or '' when nothing is missing. */
function proseMissingNote(missing) {
  return missing && missing.length
    ? `${missing.length} symbol(s) shown by bare name (no format/termFormat in EnglishLanguage): ${missing.join(', ')}`
    : '';
}

/** A collapsible plain-English rendering of a transcript (used inline by Audit;
 *  Both Ask/Tell and Audit render through this. */
function proseDetails(prose, missing) {
  return `<details class="prose-details" style="margin-top:10px">
    <summary class="hint">proof in plain English</summary>
    <div class="prose">${esc(prose || '')}</div>
    ${missing && missing.length ? `<div class="hint" style="margin-top:6px">${esc(proseMissingNote(missing))}</div>` : ''}
  </details>`;
}

function renderProof(r, backendLabel) {
  $('proverResult').hidden = false;
  $('pStatus').textContent = r.status; $('pStatus').className = 'status ' + r.status;
  $('pBackendBadge').textContent = backendLabel ? `via ${backendLabel}` : '';
  $('pSteps').textContent = r.given_steps != null ? `${r.given_steps} given-clause steps` : '';
  $('pProof').innerHTML = renderProofSteps(r.proof);
  $('pRaw').textContent = r.raw_output || '(none)';
  $('pGraphDot').textContent = r.graphviz || '(none)';
  $('pProseSlot').innerHTML = proseDetails(r.prose, r.prose_missing);
  lastAskProof = r.proof;
  invalidateAskGraph();
}

// -- Audit: whole-KB consistency check -----------------------------------------

$('runAudit').onclick = async () => {
  const btn = $('runAudit');
  const vampire = $('proverBackend').value === 'vampire';
  btn.disabled = true; btn.textContent = 'Auditing…';
  try {
    // Audit inherits the Ask/Tell prover settings (including backend, via
    // the shared #proverSettings panel both tabs toggle), but keeps its own
    // time limit.
    const { result } = vampire
      ? await call('auditVampire', {
          timeLimitSecs: $('auditTime').value,
          extraArgs: $('cfgVampireArgs').value.trim(),
        })
      : await call('audit', {
          config: proverConfig({ timeLimitSecs: $('auditTime').value }),
          limit: Math.max(1, Number($('auditLimit').value) || 5),
        });
    renderAudit(result, vampire ? 'Vampire' : 'SUPr');
  } catch (e) {
    $('auditResult').innerHTML = `<div class="card hint" style="color:var(--bad)">${esc(String(e && e.message || e))}</div>`;
  } finally {
    btn.disabled = false; btn.textContent = 'Run audit';
  }
};

function renderAudit(r, backendLabel) {
  const badge = `<span class="audit-status ${esc(r.status)}">${esc(r.status)}</span>`;
  const backend = backendLabel ? `<span class="hint">via ${esc(backendLabel)}</span>` : '';
  const steps = r.given_steps != null ? `<span class="hint">${r.given_steps} given-clause steps</span>` : '';

  let verdict;
  if (r.status === 'Consistent') {
    verdict = 'No contradiction found — the loaded KB saturated cleanly.';
  } else if (r.inconsistent && !r.contradictions.length) {
    // Vampire's one-shot run yields at most a single contradiction (no
    // enumerator, unlike the native audit's driver) — this covers the case
    // where even that single witness failed to parse; say so without
    // implying "zero".
    verdict = 'Contradiction found — see raw engine output for the derivation.';
  } else if (r.inconsistent) {
    verdict = `${r.contradictions.length} distinct contradiction${r.contradictions.length === 1 ? '' : 's'} found.`;
  } else {
    verdict = 'No contradiction found within budget — inconclusive (raise the time limit and try again).';
  }

  let html = `
    <div class="card">
      <div class="inline" style="gap:10px">${badge}${backend}${steps}</div>
      <div class="hint" style="margin-top:8px">${esc(verdict)}</div>
      <details style="margin-top:10px"><summary class="hint">raw engine output</summary><pre>${esc(r.raw_output || '(none)')}</pre></details>
    </div>`;

  html += r.contradictions.map((c, i) => `
    <div class="card">
      <div class="contradiction-hd">Contradiction #${i + 1} — ${c.steps.length} step${c.steps.length === 1 ? '' : 's'}</div>
      <ol class="refs">${renderProofSteps(c.steps)}</ol>
      ${proseDetails(c.prose, c.prose_missing)}
      <details class="proof-graph-details" style="margin-top:10px">
        <summary class="hint">proof graph</summary>
        <div class="graph-container"></div>
        <div class="hint graph-tip"></div>
        <details class="graph-dot-toggle"><summary>graphviz (DOT) source</summary><pre>${esc(c.graphviz || '(none)')}</pre></details>
      </details>
    </div>`).join('');

  $('auditResult').innerHTML = html;

  document.querySelectorAll('#auditResult .proof-graph-details').forEach((details, i) => {
    wireProofGraph(
      details,
      details.querySelector('.graph-container'),
      details.querySelector('.graph-tip'),
      () => r.contradictions[i].steps,
    );
  });
}

// -- Edit: in-browser IDE (Monaco) for KIF constituents ------------------------

const MONACO_VERSION = '0.55.1';
const MONACO_CDN = `https://cdn.jsdelivr.net/npm/monaco-editor@${MONACO_VERSION}/min/vs`;

let monaco = null;
let monacoEditor = null;
let monacoLoadPromise = null;
let editCurrentFile = null; // { name, origin } of the file being edited, or null for an unsaved "new file"
let editValidateTimer = null;
let editDiagnostics = [];

function loadMonaco() {
  if (monacoLoadPromise) return monacoLoadPromise;
  monacoLoadPromise = new Promise((resolve, reject) => {
    const script = document.createElement('script');
    script.src = `${MONACO_CDN}/loader.js`;
    script.onload = () => {
      window.require.config({ paths: { vs: MONACO_CDN } });
      window.require(['vs/editor/editor.main'], () => {
        defineKifLanguage(window.monaco);
        defineTptpLanguage(window.monaco);
        resolve(window.monaco);
      }, reject);
    };
    script.onerror = () => reject(new Error(`failed to load Monaco from ${MONACO_CDN}`));
    document.head.appendChild(script);
  });
  return monacoLoadPromise;
}

/** Monarch tokenizer mirroring `highlightKif`. */
const KIF_MONARCH = {
  defaultToken: '',
  tokenizer: {
    root: [
      { include: '@whitespace' },
      [/;.*$/, 'comment'],
      [/"(?:[^"\\]|\\.)*"/, 'string'],
      [/[?@][A-Za-z0-9_-]+/, 'variable'],
      [/-?\d+(?:\.\d+)?/, 'number'],
      [/<=>|=>/, 'keyword'],
      [/\(/, { token: 'delimiter.parenthesis', next: '@afterOpen' }],
      [/\)/, 'delimiter.parenthesis'],
      [/[A-Za-z_][A-Za-z0-9_-]*/, 'identifier'],
    ],
    afterOpen: [
      { include: '@whitespace' },
      [/\b(?:and|or|not|forall|exists|equal)\b/, { token: 'keyword', next: '@pop' }],
      [/[A-Za-z_][A-Za-z0-9_-]*/, { token: 'kif-function', next: '@pop' }],
      [/./, { token: '@rematch', next: '@pop' }],
    ],
    whitespace: [
      [/[ \t\r\n]+/, 'white'],
    ],
  },
};

/** Monaco `CompletionItemKind` for a man-paged symbol's `ManKind` labels (see core's `ManKind::as_str`). */
function kindToCompletionKind(m, kinds) {
  const K = m.languages.CompletionItemKind;
  if (kinds.includes('class')) return K.Class;
  if (kinds.includes('relation')) return K.Interface;
  if (kinds.includes('function')) return K.Function;
  if (kinds.includes('predicate')) return K.Method;
  if (kinds.includes('instance')) return K.Value;
  if (kinds.includes('individual')) return K.Constant;
  return K.Text;
}

// Only the newest in-flight completion request may resolve into suggestions —
// typing several characters in a row fires one `search` per keystroke, and a
// slow early one must not clobber the list for what's on screen now.
let completionSeq = 0;

function isRelationPosition(model, position, wordStartColumn) {
  let line = position.lineNumber;
  while (line >= 1) {
    const text = model.getLineContent(line);
    let i = line === position.lineNumber ? wordStartColumn - 2 : text.length - 1;
    while (i >= 0 && /\s/.test(text[i])) i--;
    if (i >= 0) return text[i] === '(';
    line--;
  }
  return false;
}

/**
 * Real KB symbols only, via the same `search` the Home tab uses — this
 * replaces Monaco's default word-based suggestions (any string already
 * typed in the buffer), which has no notion of what's actually a SUMO term.
 */
function kifCompletionProvider(m) {
  return {
    provideCompletionItems(model, position) {
      const word = model.getWordUntilPosition(position);
      const prefix = word.word;
      if (!prefix) return { suggestions: [] };
      const range = {
        startLineNumber: position.lineNumber, startColumn: word.startColumn,
        endLineNumber: position.lineNumber, endColumn: word.endColumn,
      };
      const seq = ++completionSeq;
      const relationOnly = isRelationPosition(model, position, word.startColumn);
        return call('search', {
          query: prefix,
          limit: 50,
          kind: relationOnly ? 'relation' : undefined,
        })
        .then((r) => {
          if (seq !== completionSeq) return { suggestions: [] };
          const prefixLc = prefix.toLowerCase();
          const suggestions = r.hits
            .filter((h) => h.symbol.toLowerCase().startsWith(prefixLc))
            .map((h) => ({
              label: h.symbol,
              kind: kindToCompletionKind(m, h.kinds),
              detail: h.kinds.join(' · ') || h.source,
              insertText: h.symbol,
              range,
            }));
          return { suggestions };
        })
        .catch(() => ({ suggestions: [] }));
    },
  };
}

function defineKifLanguage(m) {
  if (m.languages.getLanguages().some((l) => l.id === 'kif')) return;
  m.languages.register({ id: 'kif' });
  m.languages.setMonarchTokensProvider('kif', KIF_MONARCH);
  m.languages.setLanguageConfiguration('kif', {
    brackets: [['(', ')']],
    autoClosingPairs: [{ open: '(', close: ')' }, { open: '"', close: '"' }],
    // Match the tokenizer's notion of a symbol so word selection and
    // getWordAtPosition return whole SUMO terms, hyphens included — the Monaco
    // default stops at "-" and would hand back a fragment.
    wordPattern: /[A-Za-z_][A-Za-z0-9_-]*/g,
  });
  // Registering this is what makes Monaco's OWN "Format Document" command
  // (right-click menu, Shift+Alt+F) work, not just the toolbar button below —
  // both end up calling this same provider, so there's exactly one
  // implementation of "what formatting means" (formatKif, from the SDK).
  m.languages.registerDocumentFormattingEditProvider('kif', {
    provideDocumentFormattingEdits(model) {
      return [{ range: model.getFullModelRange(), text: formatKif(model.getValue()) }];
    },
  });
  m.languages.registerCompletionItemProvider('kif', kifCompletionProvider(m));
  m.editor.defineTheme('kif-light', {
    base: 'vs', inherit: true,
    rules: [
      { token: 'comment', foreground: '666666', fontStyle: 'italic' },
      { token: 'string', foreground: '1a7f37' },
      { token: 'number', foreground: '1a7f37' },
      { token: 'variable', foreground: '9a6700' },
      { token: 'keyword', foreground: '8250df', fontStyle: 'bold italic' },
      { token: 'kif-function', foreground: '2d6cdf' },
      { token: 'delimiter.parenthesis', foreground: '666666', fontStyle: 'bold' },
    ],
    colors: {},
  });
  m.editor.defineTheme('kif-dark', {
    base: 'vs-dark', inherit: true,
    rules: [
      { token: 'comment', foreground: '9aa0a6', fontStyle: 'italic' },
      { token: 'string', foreground: '4ac26b' },
      { token: 'number', foreground: '4ac26b' },
      { token: 'variable', foreground: 'e3b341' },
      { token: 'keyword', foreground: 'd2a8ff', fontStyle: 'bold italic' },
      { token: 'kif-function', foreground: '6ea8ff' },
      { token: 'delimiter.parenthesis', foreground: '9aa0a6', fontStyle: 'bold' },
    ],
    colors: {},
  });
}

/** Monarch tokenizer for the read-only TPTP preview pane. Reuses the `kif-*`
 * themes' token names (comment/string/number/keyword/kif-function/delimiter)
 * so the light/dark toggle at `setTheme` applies here too, with no separate
 * theme to keep in sync. */
const TPTP_MONARCH = {
  defaultToken: '',
  tokenizer: {
    root: [
      [/%.*$/, 'comment'],
      [/"(?:[^"\\]|\\.)*"/, 'string'],
      [/-?\d+(?:\.\d+)?/, 'number'],
      [/\b(?:fof|tff|cnf|thf)\b/, 'keyword'],
      [/\b(?:axiom|hypothesis|conjecture|negated_conjecture|type|plain)\b/, 'keyword'],
      [/[!?]\[/, 'keyword'],
      [/\b[A-Z][A-Za-z0-9_]*\b/, 'variable'],
      [/\b[a-z][A-Za-z0-9_]*\b/, 'kif-function'],
      [/[()[\],.]/, 'delimiter.parenthesis'],
    ],
  },
};

function defineTptpLanguage(m) {
  if (m.languages.getLanguages().some((l) => l.id === 'tptp')) return;
  m.languages.register({ id: 'tptp' });
  m.languages.setMonarchTokensProvider('tptp', TPTP_MONARCH);
}

const SEVERITY_TO_MONACO = { error: 'Error', warning: 'Warning', info: 'Info', hint: 'Hint' };

/** Diagnostics (from `validateFormula`, buffer-relative line/col) → Monaco markers. */
function diagsToMarkers(diags) {
  return diags.map((d) => ({
    startLineNumber: Math.max(1, d.line || 1),
    startColumn:     Math.max(1, d.col || 1),
    endLineNumber:   Math.max(1, d.end_line || d.line || 1),
    endColumn:       Math.max(1, (d.end_col || d.col || 1) + 1),
    message:         `[${d.kind}/${d.code}] ${d.message}`,
    severity:        monaco.MarkerSeverity[SEVERITY_TO_MONACO[d.severity]] || monaco.MarkerSeverity.Info,
  }));
}

/** Render only actionable errors/warnings for the buffer currently in Edit.
 * Monaco still receives all severities as inline markers. */
function renderEditDiagnostics(diags) {
  editDiagnostics = diags.filter((d) => d.severity === 'error' || d.severity === 'warning');
  const errors = editDiagnostics.filter((d) => d.severity === 'error').length;
  const warnings = editDiagnostics.length - errors;
  const parts = [];
  if (errors) parts.push(`${errors} error${errors === 1 ? '' : 's'}`);
  if (warnings) parts.push(`${warnings} warning${warnings === 1 ? '' : 's'}`);
  $('editDiagSummary').textContent = parts.length ? parts.join(', ') : 'No errors or warnings';
  $('editDiagList').innerHTML = editDiagnostics.length
    ? editDiagnostics.map((d, i) => `<button class="edit-diag" type="button" data-i="${i}" data-sev="${esc(d.severity)}">
        <span class="sev ${esc(d.severity)}">${esc(d.severity)}</span>
        <span class="edit-diag-loc">${esc(editCurrentFile?.name || 'untitled')}:${Math.max(1, d.line || 1)}:${Math.max(1, d.col || 1)}</span>
        <span class="edit-diag-msg">${esc(d.message)} <span class="edit-diag-code">[${esc(d.kind)}/${esc(d.code)}]</span></span>
      </button>`).join('')
    : '<div id="editDiagEmpty" class="hint">This file has no errors or warnings.</div>';
}

$('editDiagList').addEventListener('click', (e) => {
  const row = e.target.closest('.edit-diag');
  if (!row || !monacoEditor) return;
  const d = editDiagnostics[Number(row.dataset.i)];
  if (!d) return;
  const lineNumber = Math.max(1, d.line || 1);
  const column = Math.max(1, d.col || 1);
  monacoEditor.setPosition({ lineNumber, column });
  monacoEditor.revealPositionInCenter({ lineNumber, column });
  monacoEditor.focus();
});

function scheduleEditValidate() {
  clearTimeout(editValidateTimer);
  editValidateTimer = setTimeout(runEditValidate, 400);
}

// Coalesce validate requests: whole-file validation of a large constituent
// takes a couple of seconds in the worker, so at most one runs at a time and
// at most one rerun is queued behind it — typing never piles up a backlog.
let editValidateBusy = false;
let editValidateQueued = false;

async function runEditValidate() {
  if (!monacoEditor) return;
  if (editValidateBusy) { editValidateQueued = true; return; }
  editValidateBusy = true;
  try {
    await runEditValidateNow();
  } finally {
    editValidateBusy = false;
    if (editValidateQueued) { editValidateQueued = false; runEditValidate(); }
  }
}

async function runEditValidateNow() {
  if (!monacoEditor) return;
  const text = monacoEditor.getValue();
  const model = monacoEditor.getModel();
  const version = model?.getVersionId();
  const validatingFile = editCurrentFile
    ? `${editCurrentFile.name}|${editCurrentFile.origin}` : '';
  // A buffer belonging to a loaded constituent is diffed into the live KB and
  // validated against it, so semantic diagnostics resolve. A scratch buffer has
  // no backing file, so it falls back to parse-only checking in a throwaway KB.
  const known = editCurrentFile
    && constituents.find((c) => c.name === editCurrentFile.name && c.origin === editCurrentFile.origin);
  let diags = [];
  try {
    diags = known
      ? (await call('validateBuffer', { file: known.name, text })).diagnostics
      : (await call('validateFormula', { kif: text })).diagnostics;
  } catch (e) { $('editStatus').textContent = 'parse error: ' + (e && e.message || e); return; }
  if (!monacoEditor || monacoEditor.getModel() !== model
      || model?.getVersionId() !== version
      || validatingFile !== (editCurrentFile ? `${editCurrentFile.name}|${editCurrentFile.origin}` : '')) return;
  monaco.editor.setModelMarkers(model, 'sigma', diagsToMarkers(diags));
  renderEditDiagnostics(diags);
  const errs = diags.filter((d) => d.severity === 'error').length;
  if (!diags.length) { $('editStatus').textContent = 'no diagnostics'; return; }
  // Link the count into the Diagnostics tab, filtered to this file and landing
  // on the diagnostic nearest the first problem in the buffer.
  const label = `${diags.length} diagnostic${diags.length === 1 ? '' : 's'}` +
    (errs ? ` (${errs} error${errs === 1 ? '' : 's'})` : '');
  const file = editCurrentFile ? editCurrentFile.name : '';
  const line = diags[0]?.line || 0;
  $('editStatus').innerHTML = file
    ? `<a class="jump-diag" data-file="${esc(file)}" data-line="${line}"
         title="Show these in the Diagnostics tab">${esc(label)}</a>`
    : esc(label);
  await refreshTptpPane();
}

function setEditorContent(text) {
  if (!monacoEditor) return;
  monacoEditor.setValue(text);
  runEditValidate();
}

/** Populate the file picker from the currently loaded constituents, preserving the selection when possible. */
function populateEditPicker() {
  const sel = $('editPicker');
  if (!sel) return;
  const current = sel.value;
  sel.innerHTML = '<option value="__new__">+ New file…</option>' +
    constituents.map((c) => `<option value="${esc(c.name)}|${esc(c.origin)}">${esc(c.name)}</option>`).join('');
  sel.value = [...sel.options].some((o) => o.value === current) ? current : '__new__';
}

/**
 * The Edit tab offers exactly one write action, chosen by where the buffer came
 * from — there is no single action that means the same thing for all three:
 *   file (upload/new) → Save     — persists to OPFS + the in-memory KB
 *   sumo (GitHub)     → Submit change — opens a PR upstream; a local save would
 *                                   be silently discarded on reload anyway
 *   url (remote)      → neither  — nowhere to save it and nowhere to submit it
 */
function updateEditActions() {
  const origin = editCurrentFile ? editCurrentFile.origin : 'file';  // unsaved new file is local
  const isLocal = origin === 'file';
  const isGitHub = origin === 'sumo';
  $('editSave').hidden = !isLocal;
  $('ghPropose').hidden = !isGitHub;
  // Collapse the PR panel when it no longer applies.
  if (!isGitHub) togglePanel('ghPropose', 'ghPanel', false);
  $('editLog').style.color = '';   // clear any prior error styling
  $('editLog').textContent = origin === 'url'
    ? 'Loaded from a URL — it can be edited and downloaded here, but not saved or submitted.'
    : '';
  // A save result from whatever file was open before must not linger once
  // the user has switched to a different one.
  $('editSaveStatus').style.color = '';
  $('editSaveStatus').textContent = '';

  // The TPTP pane resolves cursor positions against a real loaded file — an
  // unsaved new buffer has nothing in the KB to resolve against yet.
  $('editTptpToggle').title = 'Split-screen views…';
  if (tptpPaneOpen) refreshTptpPane();
}

function onEditPickerChange() {
  const val = $('editPicker').value;
  if (val === '__new__') {
    editCurrentFile = null;
    setEditorContent('; New KIF file\n');
    updateEditActions();
    updateEditFileLabel();
    return;
  }
  const sep = val.indexOf('|');
  const name = val.slice(0, sep), origin = val.slice(sep + 1);
  const c = constituents.find((x) => x.name === name && x.origin === origin);
  editCurrentFile = c ? { name: c.name, origin: c.origin } : null;
  setEditorContent(c ? c.text : '');
  updateEditActions();
  updateEditFileLabel();
}

/** The filename shown beside the toolbar button group. */
function updateEditFileLabel() {
  const el = $('editFileName');
  if (!el) return;
  el.textContent = editCurrentFile ? editCurrentFile.name : 'new file (unsaved)';
}

// -- Open-file dialog: pick a loaded constituent, or start a new file ---------

$('editOpen').onclick = () => {
  $('openList').innerHTML = constituents.length
    ? constituents.map((c) =>
        `<li><a class="open-file" data-val="${escAttr(`${c.name}|${c.origin}`)}">${esc(c.name)}</a>
             <span class="hint origin">${esc(c.origin)}</span></li>`).join('')
    : '<li class="hint">no files loaded yet — create one below</li>';
  $('openDialog').showModal();
};

$('openList').addEventListener('click', (e) => {
  const a = e.target.closest('a.open-file');
  if (!a) return;
  $('openDialog').close();
  $('editPicker').value = a.dataset.val;
  onEditPickerChange();
  updateParams(editCurrentFile ? { file: editCurrentFile.name } : {});
});

$('editCreate').onclick = () => {
  $('openDialog').close();
  $('editPicker').value = '__new__';
  onEditPickerChange();
  updateParams({});
};

$('openCancel').onclick = () => $('openDialog').close();

// Memoized: `showTab('edit')` and `applyRoute()` both ask for the editor, and
// without this the two concurrent calls each get past the `monacoEditor` guard
// (it is only set at the very end, after an await) and build a SECOND editor.
// The loser's `onEditPickerChange()` then resets the buffer, clobbering any
// cursor position a deep link had just set.
let editorReadyPromise = null;
function ensureEditorReady() {
  if (!editorReadyPromise) {
    editorReadyPromise = createEditor().catch((e) => {
      editorReadyPromise = null;   // let a later visit retry
      throw e;
    });
  }
  return editorReadyPromise;
}

async function createEditor() {
  if (monacoEditor) return;
  const container = $('editorContainer');
  try {
    monaco = await loadMonaco();
  } catch (e) {
    container.dataset.placeholder = 'Failed to load the editor: ' + (e && e.message || e);
    return;
  }
  const dark = darkThemeActive();
  monacoEditor = monaco.editor.create(container, {
    value: '',
    language: 'kif',
    theme: dark ? 'kif-dark' : 'kif-light',
    automaticLayout: true,
    minimap: { enabled: false },
    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
    fontSize: 13,
    // Only the KB's own symbols (via kifCompletionProvider) should be
    // suggested, not arbitrary strings already typed in the buffer.
    wordBasedSuggestions: 'off',
  });
  monacoEditor.onDidChangeModelContent(scheduleEditValidate);
  monacoEditor.onDidChangeCursorPosition(scheduleTptpFollow);

  // Right-click a symbol → its man page. Monaco does not reliably move the
  // caret on right-click, so the click's own position is captured and used in
  // preference to the cursor.
  let ctxPos = null;
  monacoEditor.onContextMenu((e) => { ctxPos = e.target?.position ?? null; });
  monacoEditor.addAction({
    id: 'sumo.open-documentation',
    label: 'Open SUMO documentation',
    contextMenuGroupId: 'navigation',
    contextMenuOrder: 0,
    run: (ed) => {
      const model = ed.getModel();
      const pos = ctxPos || ed.getPosition();
      ctxPos = null;
      const word = model && pos && model.getWordAtPosition(pos);
      if (!word) return;
      // `?x` / `@row` are KIF variables, not terms — nothing to document.
      const prev = word.startColumn > 1
        ? model.getValueInRange({
            startLineNumber: pos.lineNumber, startColumn: word.startColumn - 1,
            endLineNumber: pos.lineNumber,   endColumn: word.startColumn })
        : '';
      if (prev === '?' || prev === '@') return;
      navigate('browse', { sym: word.word });
    },
  });

  populateEditPicker();
  onEditPickerChange();
}

$('editPicker').addEventListener('change', () => {
  onEditPickerChange();
  updateParams(editCurrentFile ? { file: editCurrentFile.name } : {});
});

// Delegates to Monaco's own format-document command rather than calling
// formatKif + applying the edit by hand — Monaco's command already runs it
// through the SAME registered provider (defineKifLanguage, above) and
// applies the result as one undoable edit, preserving cursor/scroll/undo
// history the way a hand-rolled setValue() would not.
$('editFormat').onclick = () => monacoEditor?.getAction('editor.action.formatDocument')?.run();

/** Trigger a real browser download of `text` as `name`. */
function downloadText(name, text) {
  const blob = new Blob([text], { type: 'text/plain' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url; a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

// -- Edit: download menu -------------------------------------------------------
//
// The download button opens a two-entry menu: the current buffer as KIF (a
// real download, independent of the in-browser OPFS/KB state), or the WHOLE
// knowledge base's TPTP translation (same `toTptpIndexed` dump the TPTP pane
// shows — an individual file's translation is meaningless without the rest
// of the KB's declarations, so there is no per-file TPTP option).

function setDownloadMenuOpen(on) {
  const menu = $('editDownloadMenu');
  const btn  = $('editDownload');
  if (on) {
    // Fixed-position under the button (the menu can't live inside
    // .btn-group — overflow:hidden would clip it; see index.html).
    const r = btn.getBoundingClientRect();
    menu.style.left = `${Math.round(r.left)}px`;
    menu.style.top  = `${Math.round(r.bottom + 4)}px`;
  }
  menu.hidden = !on;
  btn.setAttribute('aria-expanded', String(on));
}

$('editDownload').onclick = () => setDownloadMenuOpen($('editDownloadMenu').hidden);
// Close on any click OUTSIDE the button/menu (matching on the event target
// rather than relying on stopPropagation, which breaks under synthesized
// event sequences). The menu-item handlers below close it themselves.
document.addEventListener('click', (e) => {
  if (e.target instanceof Element && e.target.closest('#editDownload, #editDownloadMenu')) return;
  setDownloadMenuOpen(false);
});
document.addEventListener('keydown', (e) => { if (e.key === 'Escape') setDownloadMenuOpen(false); });

$('dlKifBtn').onclick = () => {
  setDownloadMenuOpen(false);
  if (!monacoEditor) return;
  downloadText(editCurrentFile ? editCurrentFile.name : 'untitled.kif', monacoEditor.getValue());
};

$('dlTptpBtn').onclick = async () => {
  setDownloadMenuOpen(false);
  const status = $('editStatus');
  const prior = status.textContent;
  status.textContent = 'generating TPTP…';
  try {
    const { text } = await call('toTptpIndexed', {});
    downloadText('knowledge-base.tptp', text);
    status.textContent = prior;
  } catch (e) {
    status.textContent = 'TPTP translation failed: ' + (e && e.message || e);
  }
};

// -- Edit: fullscreen toggle ---------------------------------------------------
//
// Lifts #tab-edit (toolbar + editor + editLog) to cover the viewport via a
// body-level class (body.edit-fullscreen), rather than moving/re-parenting
// any DOM — the CSS alone describes the two end states. The View Transitions
// API (where supported) morphs between them automatically; browsers without
// it just get an instant toggle, still fully functional.

let editFullscreen = false;

// Two icon variants for the one button: outward corner-brackets to enter,
// inward ones to exit — the same visual language most fullscreen toggles use.
const EDIT_FULLSCREEN_ENTER_PATH =
  'M2 5.5V2.75A.75.75 0 0 1 2.75 2h2.75M2 10.5v2.75c0 .414.336.75.75.75h2.75' +
  'M14 5.5V2.75a.75.75 0 0 0-.75-.75h-2.75M14 10.5v2.75a.75.75 0 0 1-.75.75h-2.75';
const EDIT_FULLSCREEN_EXIT_PATH =
  'M5.5 2v2.75a.75.75 0 0 1-.75.75H2M10.5 2v2.75c0 .414.336.75.75.75H14' +
  'M5.5 14v-2.75a.75.75 0 0 0-.75-.75H2M10.5 14v-2.75c0-.414.336-.75.75-.75H14';

function setEditFullscreen(on) {
  if (on === editFullscreen) return;
  const apply = () => {
    editFullscreen = on;
    document.body.classList.toggle('edit-fullscreen', on);
    const btn = $('editFullscreen');
    btn.setAttribute('aria-pressed', String(on));
    btn.title = on ? 'Exit fullscreen editor' : 'Toggle fullscreen editor';
    btn.querySelector('path').setAttribute('d', on ? EDIT_FULLSCREEN_EXIT_PATH : EDIT_FULLSCREEN_ENTER_PATH);
    // The container's size just changed outside of any window resize, which
    // is the one case automaticLayout's own ResizeObserver can lag behind —
    // an explicit layout() is cheap insurance against a stale-sized canvas.
    monacoEditor?.layout();
  };
  // Snapshot-based morph between the two states; falls back to an instant
  // toggle wherever unsupported (Firefox, Safari as of this writing) — still
  // fully correct, just not animated.
  if (document.startViewTransition) document.startViewTransition(apply);
  else apply();
}

$('editFullscreen').onclick = () => setEditFullscreen(!editFullscreen);

document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && editFullscreen) setEditFullscreen(false);
});

// -- Edit: split TPTP preview --------------------------------------------------
//
// A second, read-only Monaco pane showing the WHOLE knowledge base's TPTP
// translation (not just this file — an individual file's translation is
// meaningless without the rest of the KB's declarations/context), scrolled
// to follow this file's content as you edit and move the cursor. The pane
// itself works for any buffer; only cursor-follow needs a real loaded file
// to resolve positions against (see `tptpPaneUsable`).

let tptpEditor = null;
let tptpPaneOpen = false;
let tptpDecorationIds = [];

/** Lazily create the read-only TPTP editor the first time the pane opens. */
async function ensureTptpPane() {
  if (tptpEditor) return;
  monaco = await loadMonaco();
  const dark = darkThemeActive();
  tptpEditor = monaco.editor.create($('tptpContainer'), {
    value: '',
    language: 'tptp',
    theme: dark ? 'kif-dark' : 'kif-light',
    readOnly: true,
    automaticLayout: true,
    minimap: { enabled: false },
    lineNumbers: 'on',
    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
    fontSize: 13,
  });
}

/** Whether the current buffer is backed by a real loaded file — cursor-follow
 * needs one to resolve positions against the live KB; the pane itself doesn't. */
function tptpPaneUsable() {
  return !!(editCurrentFile
    && constituents.find((c) => c.name === editCurrentFile.name && c.origin === editCurrentFile.origin));
}

// The toggle is pure show/hide; PLACEMENT is not its concern — the pane sits
// below the editor normally and beside it in fullscreen, driven entirely by
// the body.edit-fullscreen CSS (see index.html).
async function setTptpPaneOpen(on) {
  if (on === tptpPaneOpen) return;
  tptpPaneOpen = on;
  const btn = $('editTptpToggle');
  btn.setAttribute('aria-pressed', String(on));
  $('editPane').classList.toggle('split', on);
  $('tptpPane').hidden = !on;
  monacoEditor?.layout();
  if (on) {
    await ensureTptpPane();
    tptpEditor?.layout();
    await refreshTptpPane();
    followTptpCursor();
  }
}

// The split-screen button opens a small menu (same pattern as the download
// menu) rather than toggling directly — its one entry for now is the TPTP
// translation pane; more split views can slot in later.
function setTptpMenuOpen(on) {
  const menu = $('editTptpMenu');
  const btn  = $('editTptpToggle');
  if (on) {
    const r = btn.getBoundingClientRect();
    menu.style.left = `${Math.round(r.left)}px`;
    menu.style.top  = `${Math.round(r.bottom + 4)}px`;
  }
  menu.hidden = !on;
  btn.setAttribute('aria-expanded', String(on));
}

// While a split view is open, the button acts as a plain dismiss — the menu
// only appears when there is something to choose.
$('editTptpToggle').onclick = () => {
  if (tptpPaneOpen) { setTptpPaneOpen(false); return; }
  setTptpMenuOpen($('editTptpMenu').hidden);
};
document.addEventListener('click', (e) => {
  if (e.target instanceof Element && e.target.closest('#editTptpToggle, #editTptpMenu')) return;
  setTptpMenuOpen(false);
});
document.addEventListener('keydown', (e) => { if (e.key === 'Escape') setTptpMenuOpen(false); });

$('tptpTranslationBtn').onclick = () => {
  setTptpMenuOpen(false);
  setTptpPaneOpen(!tptpPaneOpen);
};

// Whole-KB retranslation is the heavy half of this feature (thousands of
// axioms for a full SUMO load) — piggybacks on the SAME 400ms debounce as
// edit-validate (see `runEditValidateNow`) rather than firing separately,
// and only while the pane is actually open.
async function refreshTptpPane() {
  if (!tptpPaneOpen || !tptpEditor) return;
  $('tptpStatus').textContent = 'generating…';
  try {
    const { text } = await call('toTptpIndexed', {});
    if (!tptpEditor) return;
    tptpEditor.setValue(text);
    $('tptpStatus').textContent = `${text.split('\n').length} lines`;
    followTptpCursor();
  } catch (e) {
    $('tptpStatus').textContent = 'translation failed: ' + (e && e.message || e);
  }
}

// Cursor-follow: cheap (cache lookups against the last generated dump, no
// retranslation), so it can run on every cursor move with only a light
// debounce — mainly to avoid flooding postMessage while arrow-keying/scrolling.
let tptpFollowTimer = 0;
function scheduleTptpFollow() {
  clearTimeout(tptpFollowTimer);
  tptpFollowTimer = setTimeout(followTptpCursor, 120);
}

async function followTptpCursor() {
  if (!tptpPaneOpen || !tptpEditor || !monacoEditor || !tptpPaneUsable()) return;
  const model = monacoEditor.getModel();
  const pos = monacoEditor.getPosition();
  if (!model || !pos) return;
  // Rust's offsets are UTF-8 BYTE offsets; Monaco's are UTF-16 code-unit
  // offsets — re-encode the prefix rather than assuming they coincide (true
  // only for pure-ASCII text, which SUMO's documentation strings aren't
  // always).
  const prefix = model.getValueInRange({
    startLineNumber: 1, startColumn: 1,
    endLineNumber: pos.lineNumber, endColumn: pos.column,
  });
  const offset = new TextEncoder().encode(prefix).length;
  let line;
  try {
    ({ line } = await call('tptpLineForPosition', { file: editCurrentFile.name, offset }));
  } catch { return; }
  if (line == null || !tptpEditor) return;
  const lineNumber = line + 1; // 0-based from Rust -> Monaco's 1-based
  tptpEditor.revealLineInCenterIfOutsideViewport(lineNumber);
  tptpDecorationIds = tptpEditor.deltaDecorations(tptpDecorationIds, [{
    range: new monaco.Range(lineNumber, 1, lineNumber, 1),
    options: { isWholeLine: true, className: 'tptp-follow-line' },
  }]);
}

$('editSave').onclick = async () => {
  const btn = $('editSave');
  const status = $('editSaveStatus');
  const setStatus = (text, bad) => { status.style.color = bad ? 'var(--bad)' : ''; status.textContent = text; };

  if (!monacoEditor) return;
  let name, origin;
  if (editCurrentFile) {
    ({ name, origin } = editCurrentFile);
  } else {
    // A file created via "+ New file" has no name yet — ask for one now,
    // rather than expecting it to have been typed somewhere earlier (there
    // is nowhere left to type it in advance; the Open dialog no longer asks).
    const entered = window.prompt('Filename for this new file:', 'untitled.kif');
    if (entered === null) return;   // cancelled — leave the buffer as-is, no status change
    name = entered.trim();
    if (!name) { setStatus('Enter a filename to save.', true); return; }
    origin = 'file';
  }

  btn.disabled = true;   // icon-only button: disable, don't swap the label
  try {
    // Save is only offered for local files now (see updateEditActions), so the
    // old "this edit is session-only" warning for sumo/url origins is gone with
    // the button that could trigger it.
    const r = await updateConstituentText(name, monacoEditor.getValue(), origin);
    editCurrentFile = { name, origin };
    populateEditPicker();
    $('editPicker').value = `${name}|${origin}`;
    updateEditActions();
    updateEditFileLabel();
    runEditValidate();
    setStatus(r.notices.length ? r.notices.join(' | ') : `Saved ${name}.`, false);
  } catch (e) {
    setStatus(String(e && e.message || e), true);
  } finally {
    btn.disabled = false;
  }
};

// -- Home: what is loaded, at a glance ----------------------------------------
//
// Counts come from the worker (one pass over the KB); the upstream commit info
// is one unauthenticated GitHub call, cached for the session so revisiting the
// tab (or checking the KB snapshot cache — see fetchLastCommitInfo's other
// caller in tryRestoreFromCache) does not spend the 60/hour budget twice.

// Cache the promise, not the resolved value: two overlapping callers would
// both see a null value and each fire a request, spending two of the 60/hour
// unauthenticated budget on one page load.
let lastCommitPromise = null;

/** `{ sha, date }` of the latest commit on `SUMO.ref`, or `null` fields when
 *  the API call fails — best-effort, never thrown past this function.
 *  Shared by the stats tile (date) and the KB snapshot cache (sha, the
 *  version signal 'sumo'-origin constituents are pinned to). */
async function fetchLastCommitInfo() {
  if (!lastCommitPromise) {
    lastCommitPromise = (async () => {
      const commits = await githubApi(`/repos/${SUMO.owner}/${SUMO.repo}/commits?per_page=1`);
      const c = commits[0];
      const iso = c?.commit?.author?.date;
      return { sha: c?.sha ?? null, date: iso ? new Date(iso) : null };
    })().catch((e) => { lastCommitPromise = null; throw e; });
  }
  return lastCommitPromise;
}

/** The only part of Home derived from `promoting` — cheap, no RPC, so the
 *  post-processing window can redraw it without repeating a whole-KB pass. */
// The latest stats payload, kept for the clickable tiles' popovers.
let lastStats = null;

// -- Stat-tile popovers -------------------------------------------------------
//
// Clickable tiles (marked with the corner ⓘ) open a small animated overlay
// anchored beneath the tile. One popover at a time; outside click, Esc, or
// re-clicking the tile dismisses it.

function closeStatPopover() {
  document.querySelector('.stat-pop')?.remove();
  document.removeEventListener('click', onStatPopoverOutsideClick, true);
  document.removeEventListener('keydown', onStatPopoverEsc);
}

function onStatPopoverOutsideClick(e) {
  if (!e.target.closest('.stat-pop')) closeStatPopover();
}

function onStatPopoverEsc(e) {
  if (e.key === 'Escape') closeStatPopover();
}

function showStatPopover(tile, html) {
  const already = document.querySelector('.stat-pop');
  closeStatPopover();
  if (already && already.dataset.for === tile.id) return;   // toggle off
  const pop = document.createElement('div');
  pop.className = 'stat-pop';
  pop.dataset.for = tile.id;
  pop.innerHTML = html;
  document.body.appendChild(pop);
  const r = tile.getBoundingClientRect();
  pop.style.top = `${r.bottom + 6 + scrollY}px`;
  pop.style.left = `${Math.max(8, Math.min(r.left, innerWidth - pop.offsetWidth - 12)) + scrollX}px`;
  requestAnimationFrame(() => pop.classList.add('show'));
  // Deferred so the opening click doesn't immediately dismiss it.
  setTimeout(() => {
    document.addEventListener('click', onStatPopoverOutsideClick, true);
    document.addEventListener('keydown', onStatPopoverEsc);
  }, 0);
}

// Row grid: label | meter (or spacer) | right-aligned value.
const popRow = (label, value, extra = '', title = '') =>
  `<div class="pop-row"${title ? ` title="${escAttr(title)}"` : ''}><span class="pop-label">${label}</span>${
    extra || '<span></span>'}<span class="pop-n">${value}</span></div>`;

$('tileDoc')?.addEventListener('click', () => {
  const s = lastStats;
  if (!s) return;
  // Sub-1% coverage still deserves a signal, not a rounded-to-zero 0%.
  const pctTxt = (n) => {
    if (!n || !s.symbols) return '—';
    const p = (100 * n) / s.symbols;
    return p >= 1 ? `${Math.round(p)}%` : `${p.toFixed(1)}%`;
  };
  // Union of both coverage kinds: many languages ship termFormat labels
  // without a single documentation string (SUMO's German, French, …).
  const docs  = new Map((s.doc_languages  ?? []).map((l) => [l.language, l.documented]));
  const terms = new Map((s.term_languages ?? []).map((l) => [l.language, l.documented]));
  const langs = [...new Set([...docs.keys(), ...terms.keys()])].sort((a, b) =>
    (docs.get(b) ?? 0) - (docs.get(a) ?? 0) || (terms.get(b) ?? 0) - (terms.get(a) ?? 0));
  const rows = langs.map((lang) => popRow(
    esc(langLabel(lang)),
    pctTxt(terms.get(lang)),
    `<span class="pop-n">${pctTxt(docs.get(lang))}</span>`,
    `docs: ${fmtNum(docs.get(lang) ?? 0)} · labels: ${fmtNum(terms.get(lang) ?? 0)} · of ${fmtNum(s.symbols)} symbols`,
  )).join('');
  const header = popRow('<span class="hint">language</span>',
    '<span class="hint">labels</span>', '<span class="hint">docs</span>');
  showStatPopover($('tileDoc'), `<h4>Coverage by language</h4>${
    rows ? header + rows : '<div class="hint">no documentation or labels loaded</div>'}`);
});

$('tileRelations')?.addEventListener('click', () => {
  const s = lastStats;
  if (!s || !Number.isFinite(s.relations)) return;
  const other = Math.max(0, s.relations - (s.predicates ?? 0) - (s.functions ?? 0));
  showStatPopover($('tileRelations'), `<h4>Relations</h4>${
    popRow('predicates', fmtNum(s.predicates ?? 0))}${
    popRow('functions', fmtNum(s.functions ?? 0))}${
    popRow('other relations', fmtNum(other))}`);
});

function updateHomeNote(error) {
  $('statNote').textContent = error ? `Could not read KB stats: ${error}`
    : promoting ? 'Post-processing — counts will settle once axiomatization finishes.'
    : '';
}

async function refreshHomeStats() {
  // KB counts. These are only meaningful once promotion has run; while it is
  // still in flight the numbers are simply what has been ingested so far.
  try {
    const { stats } = await call('stats');
    lastStats = stats;
    // A stale engine (older wasm) omits newer fields — show a dash, not NaN.
    const num = (v) => (Number.isFinite(v) ? fmtNum(v) : '—');
    $('statFiles').textContent     = num(stats.files);
    $('statSymbols').textContent   = num(stats.symbols);
    $('statAxioms').textContent    = num(stats.axioms);
    $('statRules').textContent     = num(stats.rules);
    $('statClasses').textContent   = num(stats.classes);
    $('statInstances').textContent = num(stats.instances);
    $('statRelations').textContent = num(stats.relations);
    // Documentation coverage: % of symbols carrying a documentation string.
    const pct = stats.symbols && Number.isFinite(stats.documented)
      ? Math.round((100 * stats.documented) / stats.symbols) : null;
    $('statDoc').textContent = pct == null ? '—' : `${pct}%`;
    $('statDocBar').style.width = `${pct ?? 0}%`;
    $('statDoc').closest('.stat').title =
      `${fmtNum(stats.documented)} of ${fmtNum(stats.symbols)} symbols have documentation; ` +
      `${fmtNum(stats.labeled)} have a termFormat label`;
    // Diagnostics split: errors in red, warnings in amber.
    const errs  = diagnostics.filter((d) => d.severity === 'error').length;
    const warns = diagnostics.filter((d) => d.severity === 'warning').length;
    $('statDiag').innerHTML = diagnostics.length
      ? `<span style="color:var(--bad)">${fmtNum(errs)}</span> · <span style="color:var(--warn)">${fmtNum(warns)}</span>`
      : '<span style="color:var(--ok)">0</span>';
    updateHomeNote();
  } catch (e) {
    updateHomeNote(e.message || e);
  }

  // Upstream commit date, best effort — the rest of the page is useful without it.
  try {
    const { date: d } = await fetchLastCommitInfo();
    $('statCommit').textContent = d ? fmtDate(d) : 'unknown';
    $('statCommit').title = d ? d.toString() : '';
  } catch (e) {
    $('statCommit').textContent = '—';
    $('statCommit').title = `Could not reach GitHub: ${e.message || e}`;
  }
}

// -- Contribute: open a pull request against ontologyportal/sumo ---------------
//
// The editor buffer is proposed upstream as a branch + PR using a token the
// user supplies. The token is held in memory for the session; it is only
// persisted (localStorage) if the user ticks "remember", and never leaves the
// browser except as an Authorization header to api.github.com.

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
  if (editCurrentFile) return editCurrentFile.name;
  const v = $('editPicker').value;
  return v && v !== '__new__' ? v.slice(0, v.indexOf('|')) : '';
}

$('ghPropose').onclick = () => {
  if (!togglePanel('ghPropose', 'ghPanel')) return;
  $('ghToken').value = ghToken;
  $('ghRemember').checked = Boolean(localStorage.getItem(GH_TOKEN_KEY));
  const file = ghCurrentFile();
  if (!$('ghTitle').value) $('ghTitle').value = file ? `Update ${file}` : 'Update SUMO';
  ghSetStatus(file ? '' : 'Open a file in the editor first.', !file);
};

$('ghForget').onclick = () => {
  localStorage.removeItem(GH_TOKEN_KEY);
  ghToken = '';
  $('ghToken').value = '';
  $('ghRemember').checked = false;
  ghSetStatus('Token forgotten.');
};

// Keep a newly entered token available to read-only API calls even before the
// user submits a pull request. Persistence still requires explicit opt-in.
$('ghToken').addEventListener('change', () => {
  ghToken = $('ghToken').value.trim();
  if ($('ghRemember').checked && ghToken) localStorage.setItem(GH_TOKEN_KEY, ghToken);
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
  if (!monacoEditor) { ghSetStatus('The editor is still loading.', true); return; }

  // Remember only on explicit opt-in; otherwise keep it to this session.
  ghToken = token;
  if ($('ghRemember').checked) localStorage.setItem(GH_TOKEN_KEY, token);
  else localStorage.removeItem(GH_TOKEN_KEY);

  btn.disabled = true; btn.textContent = 'Submitting…';
  try {
    const { contribute } = await import('./github.js');
    const pr = await contribute({
      token, owner: SUMO.owner, repo: SUMO.repo,
      path: file,
      content: monacoEditor.getValue(),      // the live buffer, not the last save
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

// -- History: a file's commit timeline from GitHub -----------------------------
//
// Plain unauthenticated GitHub REST — the same public API the Knowledge base
// tab already uses for the file catalog, so no token and no dependency. That
// caps us at 60 requests/hour per IP, so results are cached per file for the
// session and only refetched on an explicit Refresh.

const historyCache = new Map();   // file -> commits[]
let historyShown = null;          // file currently rendered, so re-entry is free

/** Only `sumo`-origin constituents exist on GitHub; uploads/URLs have no history. */
function populateHistoryPicker() {
  const sel = $('historyPicker');
  if (!sel) return;
  const current = sel.value;
  const files = constituents.filter((c) => c.origin === 'sumo').map((c) => c.name);
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
function ensureHistory(file) {
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

boot();
