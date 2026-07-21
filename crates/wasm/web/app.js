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
 *   ./serve.sh   # → http://localhost:8080/
 *
 * The page owns the constituent list, OPFS, localStorage, and the editor; the
 * worker owns the Session. Self-contained: worker + pkg are siblings of this
 * file, so the whole demo can be dropped at any path (web/ locally, /browse/ on
 * GitHub Pages).
 */

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

const $ = (id) => document.getElementById(id);
const esc = (s) => String(s).replace(/[&<>]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' }[c]));
const fmtNum = (n) => Number(n).toLocaleString();
const fmtDate = (d) => d.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });

/** Unauthenticated GitHub REST, via the same client the contribute flow uses —
 *  one place for the Accept/API-version headers and the rate-limit wording. */
async function githubApi(path) {
  const { api } = await import('./github.js');
  return api(null, path);
}

let diagnostics = [];
let diagFilter = { file: '', severity: '' };
let constituents = [];   // [{ name, text, origin }] — the page's source of truth
let sumoCatalog = null;  // cached list of *.kif paths in the repo

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
  await rebuildSession();
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
    updateKbStatus();
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
      // Anything that renders the `promoting` flag has to be redrawn HERE.
      // Views refreshed inside the window (renderAll → refreshHomeStats) ran
      // while the flag was still set, so their "post-processing" wording is
      // stale the moment it clears.
      updateKbStatus();
      if (currentTab() === 'home') updateHomeNote();
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
}

const reprocess = () => withPostProcessing(promoteAndValidate);

/** Refresh every view that reflects KB contents (after promote+validate). */
function renderAll() {
  renderDiagnostics();
  renderConstituents();
  updateKbStatus();
  if (sumoCatalog) renderPicker();
  populateEditPicker();
  if (currentTab() === 'home') refreshHomeStats();   // counts moved
}

function setPromoteTabsEnabled(on) {
  for (const t of PROMOTE_TABS) {
    const btn = document.querySelector(`nav.tabs [data-tab=${t}]`);
    if (btn) { btn.classList.toggle('disabled', !on); btn.setAttribute('aria-disabled', String(!on)); }
  }
  if (!on && PROMOTE_TABS.includes(currentTab())) showTab('home');
}

function showToast(on) { const t = $('toast'); if (t) t.hidden = !on; }

function updateKbStatus() {
  const status = promoting
    ? 'post-processing…'
    : `<b>${diagnostics.length}</b> diagnostic${diagnostics.length === 1 ? '' : 's'}`;
  $('kbStatus').innerHTML =
    `<b>${constituents.length}</b> constituent${constituents.length === 1 ? '' : 's'} · ${status} · ` +
    `<a data-tab="kb" class="jump">manage</a>`;
}

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
    await call('boot');
    bootProgress('Engine ready');
    opfsRoot = await navigator.storage.getDirectory();
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
    updateKbStatus();
    // Honour the URL now that the constituents exist — ?tab=edit&file=…&l=…
    // needs them loaded before it can select a file in the editor.
    applyRoute();
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
// Routing lives entirely in the query string — ?tab=edit&file=Merge.kif&l=100.
// Deliberately NOT path-based (/edit): a tab path is not a real file, so it
// needs a server rewrite, and GitHub Pages (where this demo is published under
// /browse/) has none. Query-only routing needs no server support at all, so the
// same URLs work from `serve.sh`, from Pages, and from a plain file server.

const TABS = ['home', 'browse', 'kb', 'diagnostics', 'prover', 'audit', 'edit', 'history'];

// The directory this module was served from — "/" locally, "/browse/" on Pages.
// Deriving it from import.meta.url keeps the rewritten URL canonical at any
// mount point (and drops a stray /index.html).
const BASE = new URL('.', import.meta.url).pathname;

function currentTab() {
  return document.querySelector('nav.tabs button[aria-selected="true"]')?.dataset.tab || 'home';
}

/** The route encoded in the address bar: { tab, params }. */
function routeFromLocation() {
  const params = new URLSearchParams(location.search);
  const t = params.get('tab');
  return { tab: TABS.includes(t) ? t : 'home', params };
}

/** Write `tab` + `params` to the address bar without reloading. `home` is the
 *  default, so it is left out to keep the bare URL clean. */
function syncUrl(tab, params = new URLSearchParams(), { replace = false } = {}) {
  const p = new URLSearchParams(params);
  if (tab && tab !== 'home') p.set('tab', tab); else p.delete('tab');
  const qs = p.toString();
  history[replace ? 'replaceState' : 'pushState'](null, '', BASE + (qs ? `?${qs}` : ''));
}

/**
 * Show a tab. By default this records a history entry so Back/Forward work;
 * pass `{ push: false }` when reacting to the URL (boot, popstate) so we don't
 * re-push what we just read. `params` is carried into the address bar.
 */
function showTab(name, { push = true, params } = {}) {
  if (promoting && PROMOTE_TABS.includes(name)) return; // greyed while post-processing
  for (const btn of document.querySelectorAll('nav.tabs button')) {
    btn.setAttribute('aria-selected', String(btn.dataset.tab === name));
  }
  for (const p of document.querySelectorAll('.panel')) p.hidden = p.id !== `tab-${name}`;
  if (push) syncUrl(name, params ?? new URLSearchParams());
  if (name === 'home') refreshHomeStats();
  if (name === 'kb') loadSumoCatalog();
  if (name === 'edit') ensureEditorReady().catch(() => {}); // surfaced in-panel
  // Read the file straight off the URL: syncUrl (above) has already applied a
  // nav click, so this sees ?file=… on a deep link and nothing on a plain
  // click — one code path, and no double fetch from applyRoute.
  if (name === 'history') ensureHistory(new URLSearchParams(location.search).get('file'));
}

/**
 * Apply the current URL: switch to its tab and honour its deep-link params.
 * Runs after boot (constituents must exist) and on every popstate.
 *   ?tab=edit&file=Merge.kif&l=100   load that file in the editor, reveal line 100
 *   ?tab=kb / ?tab=audit / …         open that tab
 *   ?q=Human                         run the search
 *   ?sym=Human                       open the man page
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

  // ?sym= / ?q= belong to Browse. They predate the tab split and were emitted
  // without a ?tab=, so honour them on `home` too rather than breaking old links.
  if (tab === 'browse' || tab === 'home') {
    const sym = params.get('sym');
    const q = params.get('q');
    if (sym || q) {
      if (tab === 'home') showTab('browse', { push: false });
      if (sym) { openManPage(sym); }
      else { $('q').value = q; runSearch(q); }
      return;
    }
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
window.addEventListener('popstate', () => { applyRoute(); });

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
  list.innerHTML = constituents.map((c) => `
    <li class="loaded-row">
      <span><span class="sym">${esc(c.name)}</span> <span class="hint">${(c.text.length / 1000).toFixed(0)} KB</span></span>
      ${c.name === MERGE ? '<span class="hint">core</span>' : `<a class="rm" data-name="${esc(c.name)}" data-source="${c.origin}">remove</a>`}
    </li>`).join('');
}

$('loadedList').addEventListener('click', (e) => {
  const rm = e.target.closest('.rm');
  if (rm) { $('kbLog').textContent = ''; removeConstituent(rm.dataset.name, rm.dataset.source); }
});

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
    sumoCatalog = (tree.tree || []).filter((e) => e.type === 'blob' && /\.kif$/i.test(e.path)).map((e) => e.path).sort();
    renderPicker();
  } catch (e) {
    $('pickerStatus').textContent = 'could not load file list: ' + (e.message || e);
  }
}

function renderPicker() {
  const filter = $('fileFilter').value.toLowerCase();
  const loaded = new Set(constituents.filter((c) => c.origin === 'sumo').map((c) => c.name));
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
    try { const r = await ingestConstituent(path, text); if (r.added) added += 1; notices += r.notices.length; }
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
  const r = await ingestConstituent(url, await fetchText(url), 'url');
  renderConstituents();
  $('kbLog').style.color = '';
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
  const r = await ingestConstituent(file.name, text, 'file');
  renderConstituents();
  $('kbLog').style.color = '';
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

async function runSearch(query) {
  if (!query) { $('browseView').innerHTML = ''; return; }
  const { hits } = await call('search', { query, limit: 100 });
  if (hits.length === 0) {
    $('browseView').innerHTML = `<div class="card hint">No matches for <code>${esc(query)}</code>.</div>`;
    return;
  }
  const items = hits.map((h) => `
    <li>
      <a class="sym open" data-sym="${esc(h.symbol)}">${esc(h.symbol)}</a>
      <span class="kinds">${h.kinds.join(' · ') || h.source} · rank ${h.rank.toFixed(0)}</span>
      ${h.text ? `<div class="snippet">${esc(h.text)}</div>` : ''}
    </li>`).join('');
  $('browseView').innerHTML =
    `<div class="card">
       <div class="hint" style="margin-bottom:6px">${hits.length} result${hits.length === 1 ? '' : 's'} for <code>${esc(query)}</code></div>
       <ul class="results">${items}</ul>
     </div>`;
}

$('browseView').addEventListener('click', (e) => {
  const link = e.target.closest('.open');
  if (link) { e.preventDefault(); updateParams({ sym: link.dataset.sym }); openManPage(link.dataset.sym); }
});

/** Turn `&%Symbol` cross-reference markers in documentation text into man-page links. */
function linkifyDoc(text) {
  return String(text).split(/(&%[A-Za-z0-9_-]+)/).map((part) => {
    const m = part.match(/^&%([A-Za-z0-9_-]+)$/);
    return m ? `<a class="open xref" data-sym="${esc(m[1])}">${esc(m[1])}</a>` : esc(part);
  }).join('');
}

async function openManPage(symbol) {
  const { page: p } = await call('manpage', { symbol });
  if (!p) { $('browseView').innerHTML = `<div class="card hint">No man page for <code>${esc(symbol)}</code>.</div>`; return; }

  const docs = (v) => v.map((d) => `<div>${linkifyDoc(d.text)} <span class="hint">(${esc(d.language)})</span></div>`).join('');
  const links = (edges) => edges.length
    ? edges.map((edge) => `<code><span class="hint">${esc(edge.relation)}</span> <a class="open" data-sym="${esc(edge.parent)}">${esc(edge.parent)}</a></code>`).join('')
    : '<span class="hint">none</span>';
  const sig = () => {
    const parts = [];
    if (p.arity != null) parts.push(`arity ${p.arity < 0 ? 'variable' : p.arity}`);
    for (const d of p.domains) parts.push(`arg ${d.position}: ${esc(d.sort.class)}${d.sort.subclass ? ' (class)' : ''}`);
    if (p.range) parts.push(`range: ${esc(p.range.class)}`);
    return parts.length ? parts.join('<br>') : '<span class="hint">none declared</span>';
  };
  const field = (title, html) => `<div class="field"><h3>${title}</h3><div class="val">${html}</div></div>`;

  const references = () => {
    if (!p.references.length) return '<span class="hint">none</span>';
    const rows = p.references.map((r) => {
      return `<li>
        <pre class="ref-kif">${highlightKif(r.kif).replace(/\n$/, '')}</pre>
        <div class="ref-meta">
          ${locLink(r.file, r.line)}
          ${ghAnchor(r.file, r.line)}
        </div>
      </li>`;
    }).join('');
    return `<ol class="refs">${rows}</ol>`;
  };

  $('browseView').innerHTML = `
    <div class="card man">
      <a class="hint back" style="cursor:pointer">← back to results</a>
      <h2>${esc(p.name)}</h2>
      <div class="kinds">${p.kinds.join(' · ') || 'symbol'}</div>
      ${p.documentation.length ? field('Documentation', docs(p.documentation)) : ''}
      ${field('Parents', `<div class="tax">${links(p.parents)}</div>`)}
      ${field('Children', `<div class="tax">${links(p.children)}</div>`)}
      ${(p.arity != null || p.domains.length || p.range) ? field('Signature', sig()) : ''}
      ${p.term_format.length ? field('Term format', docs(p.term_format)) : ''}
      ${p.format.length ? field('Format', docs(p.format)) : ''}
      ${field('References', `<div class="hint" style="margin-bottom:4px">appears in ${p.appears_in_count} formula${p.appears_in_count === 1 ? '' : 's'} total (excluding documentation/taxonomy, listed below)</div>${references()}`)}
    </div>`;
  $('browseView').querySelector('.back').onclick = () => runSearch($('q').value.trim());
}

// -- Diagnostics --------------------------------------------------------------

function renderDiagnostics() {
  const files = [...new Set(diagnostics.map((d) => d.file).filter(Boolean))].sort();
  const fileSel = $('diagFileFilter');
  if (fileSel) {
    fileSel.innerHTML = `<option value="">All files</option>` +
      files.map((f) => `<option value="${esc(f)}">${esc(f)}</option>`).join('');
    // Only discard an unknown file filter once there is data to contradict it:
    // the route is applied right after ingest, while validation is still
    // running, and clearing it then would drop a filter from the URL.
    if (diagnostics.length && !files.includes(diagFilter.file)) diagFilter.file = '';
    fileSel.value = diagFilter.file;
  }
  const sevSel = $('diagSevFilter');
  if (sevSel) sevSel.value = diagFilter.severity;

  const filtered = diagnostics
    .map((d, i) => ({ d, i }))
    .filter(({ d }) => (!diagFilter.file || d.file === diagFilter.file) &&
      (!diagFilter.severity || d.severity === diagFilter.severity));

  const errs = diagnostics.filter((d) => d.severity === 'error').length;
  const filterActive = diagFilter.file || diagFilter.severity;
  const sum = $('diagSummary');
  if (sum) sum.innerHTML = diagnostics.length
    ? (filterActive
        ? `<b>${filtered.length}</b> of <b>${diagnostics.length}</b> diagnostic${diagnostics.length === 1 ? '' : 's'} shown`
        : `<b>${diagnostics.length}</b> diagnostic${diagnostics.length === 1 ? '' : 's'}`) +
      (errs ? ` (${errs} error${errs === 1 ? '' : 's'} total)` : '') +
      ` — click a <span class="loc">file:line</span> to open it in the editor`
    : 'No diagnostics — the loaded KB is clean.';

  const list = $('diagList');
  if (!list) return;
  list.innerHTML = filtered.length ? filtered.map(({ d, i }) => {
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
  updateKbStatus();
});

/** Mirror the active filters into the address bar so a filtered view is shareable. */
function syncDiagUrl() {
  updateParams({ file: diagFilter.file, sev: diagFilter.severity });
}

/**
 * Apply ?file / ?sev / ?l to the Diagnostics tab. Called both when the route is
 * applied and again once validation finishes — on a cold load the route runs
 * before any diagnostics exist, so the first pass has nothing to filter or
 * scroll to.
 */
function applyDiagRouteParams() {
  const params = new URLSearchParams(location.search);
  if ((params.get('tab') || 'home') !== 'diagnostics') return;
  diagFilter.file     = params.get('file') || '';
  diagFilter.severity = params.get('sev')  || '';
  renderDiagnostics();
  const line = Number(params.get('l'));
  if (Number.isFinite(line) && line > 0) scrollToDiagnostic(diagFilter.file, line);
}

$('diagFileFilter').addEventListener('change', () => {
  diagFilter.file = $('diagFileFilter').value; renderDiagnostics(); syncDiagUrl();
});
$('diagSevFilter').addEventListener('change', () => {
  diagFilter.severity = $('diagSevFilter').value; renderDiagnostics(); syncDiagUrl();
});

/**
 * Scroll to the diagnostic in `file` nearest `line` and flash it. Nearest
 * rather than exact: the caller's line comes from an edited buffer, whose line
 * numbers drift from the KB's as soon as anything above is inserted.
 */
function scrollToDiagnostic(file, line) {
  let best = -1, bestDist = Infinity;
  diagnostics.forEach((d, i) => {
    if (file && d.file !== file) return;
    const dist = Math.abs((d.line || 0) - line);
    if (dist < bestDist) { bestDist = dist; best = i; }
  });
  const el = best >= 0 && $('diagList').querySelector(`.diag[data-i="${best}"]`);
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
const KIF_TOKEN_RE = /(;[^\n]*)|("(?:[^"\\]|\\.)*")|([()])|([?@][A-Za-z0-9_-]+)|(-?\d+(?:\.\d+)?)|(<=>|=>|=)|([A-Za-z_][A-Za-z0-9_-]*)/g;

function highlightKif(src) {
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
      if (KIF_KEYWORDS.has(word)) out += `<span class="tok-kw">${esc(word)}</span>`;
      else if (afterOpenParen) out += `<span class="tok-fn">${esc(word)}</span>`; // relation/function symbol
      else out += esc(word);
      afterOpenParen = false;
    }
    last = KIF_TOKEN_RE.lastIndex;
  }
  out += esc(src.slice(last));
  return out + '\n'; // trailing line so a source ending in \n doesn't collapse height vs. the textarea
}

function attachKifHighlighting(textareaId, preId) {
  const ta = $(textareaId), hl = $(preId);
  const update = () => { hl.innerHTML = highlightKif(ta.value); };
  ta.addEventListener('input', update);
  ta.addEventListener('scroll', () => { hl.scrollTop = ta.scrollTop; hl.scrollLeft = ta.scrollLeft; });
  update();
}

attachKifHighlighting('assertions', 'assertionsHl');
attachKifHighlighting('pquery', 'pqueryHl');

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
  const dark = window.matchMedia?.('(prefers-color-scheme: dark)').matches;
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

$('proverSettingsBtn').onclick = () => togglePanel('proverSettingsBtn', 'proverSettings');
$('cfgReset').onclick = () => applyProverConfig(CFG_DEFAULTS);
for (const { id } of CFG_KNOBS) $(id).addEventListener('input', renderCfgSummary);
renderCfgSummary();

// -- Prover: tell + ask -------------------------------------------------------

$('prove').onclick = async () => {
  const btn = $('prove');
  btn.disabled = true; btn.textContent = 'Proving…';
  try {
    const { result } = await call('prove', {
      assertions: $('assertions').value.trim(),
      query: $('pquery').value,
      config: proverConfig(),
      session: 'user-assertions',
    });
    renderProof(result);
  } catch (e) {
    $('proverResult').hidden = false;
    $('pStatus').textContent = 'Error'; $('pStatus').className = 'status InputError';
    $('pSteps').textContent = String(e && e.message || e);
    $('pProof').innerHTML = ''; $('pRaw').textContent = ''; $('pGraphDot').textContent = '';
    $('pProseSlot').innerHTML = '';
    lastAskProof = [];
    invalidateAskGraph();
  } finally {
    btn.disabled = false; btn.textContent = 'Prove';
  }
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

/** One proof/contradiction step as an <li>. */
function proofStepRow(s) {
  const loc = locLink(s.file, s.line);
  const gh = ghAnchor(s.file, s.line);
  return `<li>
    <div class="hint">${esc(s.rule)}${premiseRefs(s)}</div>
    <pre class="ref-kif">${highlightKif(s.kif).replace(/\n$/, '')}</pre>
    ${loc || gh ? `<div class="ref-meta">${loc}${gh}</div>` : ''}
  </li>`;
}

const renderProofSteps = (steps) => steps.map(proofStepRow).join('');

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

function renderProof(r) {
  $('proverResult').hidden = false;
  $('pStatus').textContent = r.status; $('pStatus').className = 'status ' + r.status;
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
  btn.disabled = true; btn.textContent = 'Auditing…';
  try {
    // Audit inherits the Ask/Tell prover settings, but keeps its own time limit.
    const { result } = await call('audit', {
      config: proverConfig({ timeLimitSecs: $('auditTime').value }),
      limit: Math.max(1, Number($('auditLimit').value) || 5),
    });
    renderAudit(result);
  } catch (e) {
    $('auditResult').innerHTML = `<div class="card hint" style="color:var(--bad)">${esc(String(e && e.message || e))}</div>`;
  } finally {
    btn.disabled = false; btn.textContent = 'Run audit';
  }
};

function renderAudit(r) {
  const badge = `<span class="audit-status ${esc(r.status)}">${esc(r.status)}</span>`;
  const steps = r.given_steps != null ? `<span class="hint">${r.given_steps} given-clause steps</span>` : '';

  let verdict;
  if (r.status === 'Consistent') {
    verdict = 'No contradiction found — the loaded KB saturated cleanly.';
  } else if (r.inconsistent) {
    verdict = `${r.contradictions.length} distinct contradiction${r.contradictions.length === 1 ? '' : 's'} found.`;
  } else {
    verdict = 'No contradiction found within budget — inconclusive (raise the time limit and try again).';
  }

  let html = `
    <div class="card">
      <div class="inline" style="gap:10px">${badge}${steps}</div>
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

function loadMonaco() {
  if (monacoLoadPromise) return monacoLoadPromise;
  monacoLoadPromise = new Promise((resolve, reject) => {
    const script = document.createElement('script');
    script.src = `${MONACO_CDN}/loader.js`;
    script.onload = () => {
      window.require.config({ paths: { vs: MONACO_CDN } });
      window.require(['vs/editor/editor.main'], () => resolve(window.monaco), reject);
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

function scheduleEditValidate() {
  clearTimeout(editValidateTimer);
  editValidateTimer = setTimeout(runEditValidate, 400);
}

async function runEditValidate() {
  if (!monacoEditor) return;
  const text = monacoEditor.getValue();
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
  if (!monacoEditor) return;
  monaco.editor.setModelMarkers(monacoEditor.getModel(), 'sigma', diagsToMarkers(diags));
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
}

function onEditPickerChange() {
  const val = $('editPicker').value;
  if (val === '__new__') {
    editCurrentFile = null;
    $('editNewNameWrap').hidden = false;
    $('editNewName').value = '';
    setEditorContent('; New KIF file\n');
    updateEditActions();
    return;
  }
  $('editNewNameWrap').hidden = true;
  const sep = val.indexOf('|');
  const name = val.slice(0, sep), origin = val.slice(sep + 1);
  const c = constituents.find((x) => x.name === name && x.origin === origin);
  editCurrentFile = c ? { name: c.name, origin: c.origin } : null;
  setEditorContent(c ? c.text : '');
  updateEditActions();
}

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
  defineKifLanguage(monaco);
  const dark = window.matchMedia?.('(prefers-color-scheme: dark)').matches;
  monacoEditor = monaco.editor.create(container, {
    value: '',
    language: 'kif',
    theme: dark ? 'kif-dark' : 'kif-light',
    automaticLayout: true,
    minimap: { enabled: false },
    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
    fontSize: 13,
  });
  monacoEditor.onDidChangeModelContent(scheduleEditValidate);

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

/** Save the buffer to the user's local disk (a real download, independent of the in-browser OPFS/KB state). */
$('editDownload').onclick = () => {
  if (!monacoEditor) return;
  const name = editCurrentFile ? editCurrentFile.name : ($('editNewName').value.trim() || 'untitled.kif');
  const blob = new Blob([monacoEditor.getValue()], { type: 'text/plain' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url; a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
};

$('editSave').onclick = async () => {
  const btn = $('editSave');
  btn.disabled = true; btn.textContent = 'Saving…';
  try {
    if (!monacoEditor) return;
    const text = monacoEditor.getValue();
    let name, origin;
    if (editCurrentFile) {
      ({ name, origin } = editCurrentFile);
    } else {
      name = $('editNewName').value.trim();
      if (!name) throw new Error('Enter a filename first.');
      origin = 'file';
    }
    // Save is only offered for local files now (see updateEditActions), so the
    // old "this edit is session-only" warning for sumo/url origins is gone with
    // the button that could trigger it.
    const r = await updateConstituentText(name, text, origin);
    editCurrentFile = { name, origin };
    populateEditPicker();
    $('editPicker').value = `${name}|${origin}`;
    $('editNewNameWrap').hidden = true;
    updateEditActions();
    runEditValidate();
    $('editLog').style.color = '';
    $('editLog').textContent = r.notices.length ? r.notices.join(' | ') : `Saved ${name}.`;
  } catch (e) {
    $('editLog').style.color = 'var(--bad)';
    $('editLog').textContent = String(e && e.message || e);
  } finally {
    btn.disabled = false; btn.textContent = 'Save';
  }
};

// -- Home: what is loaded, at a glance ----------------------------------------
//
// Counts come from the worker (one pass over the KB); the upstream commit date
// is one unauthenticated GitHub call, cached for the session so revisiting the
// tab does not spend the 60/hour budget.

// Cache the promise, not the resolved value: two overlapping callers would
// both see a null value and each fire a request, spending two of the 60/hour
// unauthenticated budget on one page load.
let lastCommitPromise = null;

async function fetchLastCommitDate() {
  if (!lastCommitPromise) {
    lastCommitPromise = (async () => {
      const commits = await githubApi(`/repos/${SUMO.owner}/${SUMO.repo}/commits?per_page=1`);
      const iso = commits[0]?.commit?.author?.date;
      return iso ? new Date(iso) : null;
    })().catch((e) => { lastCommitPromise = null; throw e; });
  }
  return lastCommitPromise;
}

/** The only part of Home derived from `promoting` — cheap, no RPC, so the
 *  post-processing window can redraw it without repeating a whole-KB pass. */
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
    $('statFiles').textContent   = fmtNum(stats.files);
    $('statSymbols').textContent = fmtNum(stats.symbols);
    $('statAxioms').textContent  = fmtNum(stats.axioms);
    $('statRules').textContent   = fmtNum(stats.rules);
    updateHomeNote();
  } catch (e) {
    updateHomeNote(e.message || e);
  }

  // Upstream commit date, best effort — the rest of the page is useful without it.
  try {
    const d = await fetchLastCommitDate();
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

const GH_TOKEN_KEY = 'sumoBrowserGhToken';
let ghToken = localStorage.getItem(GH_TOKEN_KEY) || '';

function ghSetStatus(text, bad = false) {
  const el = $('ghStatus');
  el.textContent = text;
  el.style.color = bad ? 'var(--bad)' : '';
}

/** The file the Contribute panel acts on: whatever the Edit tab has open. */
function ghCurrentFile() {
  if (editCurrentFile) return editCurrentFile.name;
  const v = $('editPicker').value;
  return v && v !== '__new__' ? v.slice(0, v.indexOf('|')) : ($('editNewName').value.trim() || '');
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
