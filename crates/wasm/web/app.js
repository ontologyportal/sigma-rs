/**
 * SUMO browser — a demo over the SDK-shaped facade (`Session` / `Config`).
 * Autoloads SUMO's foundational ontology (Merge.kif) from
 * github.com/ontologyportal/sumo, and lets you add/remove constituents. Tabs:
 *   Home         — symbol search → results → man page
 *   Knowledge base — manage loaded constituents (add from SUMO / URL / upload)
 *   Diagnostics  — the KB's validation findings
 *   Prover       — tell assertions + ask a query, in-browser
 *
 * Must be served over HTTP — browsers block ES modules + wasm fetch on file://.
 *   ./serve.sh   # → http://localhost:8080/
 *
 * Self-contained: imports `./pkg/…` (not `../pkg/…`) so the whole demo can be
 * dropped at any path — served from web/ locally, or published under /browse/
 * on GitHub Pages — with pkg/ as a sibling of this file.
 */
import { init, Session, Config } from './pkg/sdk.mjs';

const SUMO = { owner: 'ontologyportal', repo: 'sumo', ref: 'HEAD' };
const MERGE = 'Merge.kif'; // the foundational ontology, loaded on startup
const MIDLEVEL = 'Mid-level-ontology.kif'; // the foundational ontology, loaded on startup
const rawUrl = (path) => `https://raw.githubusercontent.com/${SUMO.owner}/${SUMO.repo}/${SUMO.ref}/${path}`;
const SUMO_FILE_SETTING = "sumoFiles";
let savedConstituents = JSON.parse(localStorage.getItem(SUMO_FILE_SETTING) || "null") || [
  { name: MERGE, origin: "sumo"},
  { name: MIDLEVEL, origin: "sumo"}
];
let opfsRoot = null;

const $ = (id) => document.getElementById(id);
const esc = (s) => String(s).replace(/[&<>]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' }[c]));

let session = null;
let diagnostics = [];
let diagFilter = { file: '', severity: '' };
let constituents = [];   // [{ name, text }] — cached so remove/reset rebuild without refetch
let sumoCatalog = null;  // cached list of *.kif paths in the repo

function newSession() {
  const cfg = new Config();
  cfg.wantProof = true;
  return new Session({ config: cfg });
}

async function fromOrigin(origin, file) {
  if (origin === "sumo") return await fetchText(rawUrl(file))
  if (origin === "url") return await fetchText(file)
  if (origin === "file") {
    if (opfsRoot === null) { throw "File system not initialized yet"; }
    let handle = await opfsRoot.getFileHandle(file);
    let vFile = await handle.getFile();
    return await vFile.text();
  }
}

async function fetchText(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${url}: HTTP ${r.status}`);
  return r.text();
}

// -- KB state mutations -------------------------------------------------------

/**
 * Load one constituent's text into the KB and cache it. The constituent is
 * always tracked once loaded — `loadKif` still ingests content that carries
 * non-fatal notices (e.g. "duplicate formula ignored"), so those are returned
 * as informational, not treated as a failure.
 * @returns {{ added: boolean, notices: string[] }}
 */
function addConstituent(name, text, origin = 'sumo') {
  if (constituents.some((c) => c.name === name)) return { added: false, notices: [`${name}: already loaded`] };
  const notices = session.kb.loadKif(text, name); // native loadKif promotes into the axiom base
  constituents.push({ name, text, origin });
  if (savedConstituents.find((c) => c.name == name && c.origin == origin) === undefined) {
    savedConstituents.push({name, origin});
    localStorage.setItem(SUMO_FILE_SETTING, JSON.stringify(savedConstituents));
  }
  refreshAll();
  return { added: true, notices };
}

/** Rebuild the session from the current (cached) constituents — used by remove/reset. */
function rebuildSession() {
  session = newSession();
  for (const c of constituents) session.kb.loadKif(c.text, c.name);
  refreshAll();
}

/**
 * Save `text` as constituent `name`/`origin` — updates it in place if
 * already loaded, else adds it as a brand-new constituent. Used by the Edit
 * tab's Save button.
 *
 * For `file`-origin constituents, persists to OPFS FIRST (awaited), mirroring
 * the Knowledge base tab's upload flow (`$('kbFile').onchange`), which writes
 * to OPFS before ever registering the constituent. That ordering matters:
 * `addConstituent` tracks `{name, origin}` in `savedConstituents`/localStorage
 * regardless of whether a backing file exists, and `boot()` reloads every
 * tracked constituent from `fromOrigin` on next launch — a `file`-origin
 * entry with no OPFS handle throws there, and since `boot()` wraps its whole
 * load loop in one try/catch, that throw aborts loading every OTHER
 * constituent too, not just the broken one.
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
  if (idx === -1) return addConstituent(name, text, origin);
  constituents[idx] = { ...constituents[idx], text };
  rebuildSession();
  return { added: false, notices: [] };
}

function removeConstituent(name, origin = 'sumo') {
  constituents = constituents.filter((c) => c.name !== name || c.origin !== origin);
  savedConstituents = savedConstituents.filter((c) => c.name !== name || c.origin !== origin);
  localStorage.setItem(SUMO_FILE_SETTING, JSON.stringify(savedConstituents));
  if (origin === 'file') {
    opfsRoot.getFileHandle(name).then((h) => h.remove());
  }
  rebuildSession();
}

function resetToMerge() {
  const merge = constituents.find((c) => c.name === MERGE);
  constituents = merge ? [merge] : [];
  rebuildSession();
}

/** Re-validate and refresh every view that reflects KB contents. */
function refreshAll() {
  diagnostics = session.validate();
  renderDiagnostics();
  renderConstituents();
  updateKbStatus();
  if (sumoCatalog) renderPicker();
  populateEditPicker();
}

function updateKbStatus() {
  $('kbStatus').innerHTML =
    `<b>${constituents.length}</b> constituent${constituents.length === 1 ? '' : 's'} · ` +
    `<b>${diagnostics.length}</b> diagnostic${diagnostics.length === 1 ? '' : 's'} · ` +
    `<a data-tab="kb" class="jump">manage</a>`;
}

// -- Boot ---------------------------------------------------------------------

async function boot() {
  try {
    $('overlayMsg').textContent = 'Loading application...';
    await init();
    session = newSession();
    opfsRoot = await navigator.storage.getDirectory();
    let i = 1;
    const total = savedConstituents.length;

    for (const {name, origin} of savedConstituents) {
      $('overlayMsg').textContent = `Loading ${name} (${i}/${total})...`;
      const text = await fromOrigin(origin, name);
      $('overlayMsg').textContent = 'Loading & validating...';
      addConstituent(name, text, origin);  
      i+=1;
    }

    $('overlay').remove();
  } catch (e) {
    $('overlayMsg').textContent = 'Failed to load SUMO.';
    $('overlayErr').textContent = String(e && e.message || e) + '  (Try checking your network connection.)';
    document.querySelector('.spinner')?.remove();
  }
}

// -- Tabs ---------------------------------------------------------------------

function showTab(name) {
  for (const btn of document.querySelectorAll('nav.tabs button')) {
    btn.setAttribute('aria-selected', String(btn.dataset.tab === name));
  }
  for (const p of document.querySelectorAll('.panel')) p.hidden = p.id !== `tab-${name}`;
  if (name === 'kb') loadSumoCatalog();
  if (name === 'edit') ensureEditorReady();
}

document.querySelector('nav.tabs').addEventListener('click', (e) => {
  const tab = e.target.closest('button')?.dataset.tab;
  if (tab) showTab(tab);
});
document.addEventListener('click', (e) => {
  const jump = e.target.closest('.jump');
  if (jump) { e.preventDefault(); showTab(jump.dataset.tab); }
});

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

async function loadSumoCatalog() {
  if (sumoCatalog) return;
  $('pickerStatus').textContent = 'loading file list…';
  try {
    const tree = await (await fetch(`https://api.github.com/repos/${SUMO.owner}/${SUMO.repo}/git/trees/${SUMO.ref}?recursive=1`)).json();
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
  let added = 0, notices = 0;
  for (const path of paths) {
    $('kbLog').style.color = ''; $('kbLog').textContent = `Fetching ${path}…`;
    const r = addConstituent(path, await fetchText(rawUrl(path)));
    if (r.added) added += 1;
    notices += r.notices.length;
  }
  $('kbLog').textContent = `Added ${added}/${paths.length} constituent(s)` + (notices ? ` (${notices} load notice(s))` : '') + '.';
});

$('addUrl').onclick = (e) => withBusy(e.target, async () => {
  const url = $('kbUrl').value.trim();
  if (!url) { $('kbLog').textContent = 'Enter a URL first.'; return; }
  const r = addConstituent(url, await fetchText(url), 'url');
  $('kbLog').style.color = '';
  $('kbLog').textContent = r.added ? `Added ${url}` + (r.notices.length ? ` (${r.notices.length} notice(s))` : '') + '.' : r.notices.join(' | ');
});

$('kbFile').onchange = (e) => withBusy($('addUrl'), async () => {
  const file = e.target.files[0];
  if (!file) return;
  const text = await file.text();
  if (!opfsRoot === null) throw "File system not yet initialized";
  console.log(file.name);
  const handle = await opfsRoot.getFileHandle(file.name, { create: true });
  const stream = await handle.createWritable();
  await stream.write(text);
  await stream.close();
  const r = addConstituent(file.name, text, 'file');
  $('kbLog').style.color = '';
  $('kbLog').textContent = r.added ? `Added ${file.name}` + (r.notices.length ? ` (${r.notices.length} notice(s))` : '') + '.' : r.notices.join(' | ');
});

// -- Home: search → results → man page ----------------------------------------

$('searchForm').addEventListener('submit', (e) => { e.preventDefault(); runSearch($('q').value.trim()); });

function runSearch(query) {
  if (!query) { $('homeView').innerHTML = ''; return; }
  const hits = session.search(query, { limit: 100 });
  if (hits.length === 0) {
    $('homeView').innerHTML = `<div class="card hint">No matches for <code>${esc(query)}</code>.</div>`;
    return;
  }
  const items = hits.map((h) => `
    <li>
      <a class="sym open" data-sym="${esc(h.symbol)}">${esc(h.symbol)}</a>
      <span class="kinds">${h.kinds.join(' · ') || h.source} · rank ${h.rank.toFixed(0)}</span>
      ${h.text ? `<div class="snippet">${esc(h.text)}</div>` : ''}
    </li>`).join('');
  $('homeView').innerHTML =
    `<div class="card">
       <div class="hint" style="margin-bottom:6px">${hits.length} result${hits.length === 1 ? '' : 's'} for <code>${esc(query)}</code></div>
       <ul class="results">${items}</ul>
     </div>`;
}

$('homeView').addEventListener('click', (e) => {
  const link = e.target.closest('.open');
  if (link) { e.preventDefault(); openManPage(link.dataset.sym); }
});

/** Turn `&%Symbol` cross-reference markers in documentation text into man-page links. */
function linkifyDoc(text) {
  return String(text).split(/(&%[A-Za-z0-9_-]+)/).map((part) => {
    const m = part.match(/^&%([A-Za-z0-9_-]+)$/);
    return m ? `<a class="open xref" data-sym="${esc(m[1])}">${esc(m[1])}</a>` : esc(part);
  }).join('');
}

function openManPage(symbol) {
  const p = session.manpage(symbol);
  if (!p) { $('homeView').innerHTML = `<div class="card hint">No man page for <code>${esc(symbol)}</code>.</div>`; return; }

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
      const gh = ghLink(r.file, r.line);
      const loc = r.file ? `${esc(r.file)}:${r.line}` : null;
      return `<li>
        <pre class="ref-kif">${highlightKif(r.kif).replace(/\n$/, '')}</pre>
        <div class="ref-meta">
          ${loc ? `<span class="hint ref-loc">${loc}</span>` : ''}
          ${gh ? `<a class="hint gh" href="${gh}" target="_blank" rel="noopener">GitHub ↗</a>` : ''}
        </div>
      </li>`;
    }).join('');
    return `<ol class="refs">${rows}</ol>`;
  };

  $('homeView').innerHTML = `
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
  $('homeView').querySelector('.back').onclick = () => runSearch($('q').value.trim());
}

// -- Diagnostics --------------------------------------------------------------

function renderDiagnostics() {
  const files = [...new Set(diagnostics.map((d) => d.file).filter(Boolean))].sort();
  const fileSel = $('diagFileFilter');
  if (fileSel) {
    fileSel.innerHTML = `<option value="">All files</option>` +
      files.map((f) => `<option value="${esc(f)}">${esc(f)}</option>`).join('');
    if (!files.includes(diagFilter.file)) diagFilter.file = '';
    fileSel.value = diagFilter.file;
  }
  const sevSel = $('diagSevFilter');
  if (sevSel) sevSel.value = diagFilter.severity;

  const filtered = diagnostics
    .map((d, i) => ({ d, i }))
    .filter(({ d }) => (!diagFilter.file || d.file === diagFilter.file) &&
      (!diagFilter.severity || d.severity === diagFilter.severity));

  const errs = diagnostics.filter((d) => d.severity === 'Error').length;
  const filterActive = diagFilter.file || diagFilter.severity;
  const sum = $('diagSummary');
  if (sum) sum.innerHTML = diagnostics.length
    ? (filterActive
        ? `<b>${filtered.length}</b> of <b>${diagnostics.length}</b> diagnostic${diagnostics.length === 1 ? '' : 's'} shown`
        : `<b>${diagnostics.length}</b> diagnostic${diagnostics.length === 1 ? '' : 's'}`) +
      (errs ? ` (${errs} error${errs === 1 ? '' : 's'} total)` : '') +
      ` — click a <span class="loc">file:line</span> to view the source`
    : 'No diagnostics — the loaded KB is clean.';

  const list = $('diagList');
  if (!list) return;
  list.innerHTML = filtered.length ? filtered.map(({ d, i }) => {
    const gh = ghLink(d.file, d.line);
    const loc = d.file ? `${esc(d.file)}:${d.line}` : '(no location)';
    return `<div class="diag" data-i="${i}" data-sev="${esc(d.severity)}">
      <div class="diag-head">
        <span class="sev ${esc(d.severity)}">${esc(d.severity)}</span>
        <a class="loc">${loc}</a>
        <span class="code">[${esc(d.kind)}/${esc(d.code)}]</span>
        ${gh ? `<a class="hint gh" href="${gh}" target="_blank" rel="noopener">GitHub ↗</a>` : ''}
        <span class="msg">${esc(d.message)}</span>
      </div>
    </div>`;
  }).join('') : `<div class="hint">${diagnostics.length ? 'No diagnostics match the current filters.' : ''}</div>`;
}

/** Lines around `line` from the cached constituent text, or null if unavailable. */
function sourceAt(file, line, ctx = 3) {
  const c = constituents.find((x) => x.name === file);
  if (!c || !(line > 0)) return null;
  const lines = c.text.split('\n');
  const start = Math.max(1, line - ctx), end = Math.min(lines.length, line + ctx);
  const rows = [];
  for (let n = start; n <= end; n++) rows.push({ n, text: (lines[n - 1] ?? '').replace(/\r$/, ''), hit: n === line });
  return rows;
}

/** GitHub blob deep-link for a SUMO-sourced constituent, else null. */
function ghLink(file, line) {
  const c = constituents.find((x) => x.name === file);
  if (!c || c.origin !== 'sumo') return null;
  return `https://github.com/${SUMO.owner}/${SUMO.repo}/blob/${SUMO.ref}/${file}#L${line}`;
}

// Click a diagnostic's location → toggle an inline source view at that line.
$('diagList').addEventListener('click', (e) => {
  if (e.target.closest('a.gh')) return; // let the GitHub link open normally
  const head = e.target.closest('.diag-head');
  if (!head) return;
  const diag = head.closest('.diag');
  const existing = diag.querySelector('.src');
  if (existing) { existing.remove(); return; } // toggle off
  const d = diagnostics[+diag.dataset.i];
  const rows = sourceAt(d.file, d.line);
  diag.insertAdjacentHTML('beforeend', rows
    ? `<div class="src"><table>${rows.map((r) =>
        `<tr class="${r.hit ? 'hit' : ''}"><td class="ln">${r.n}</td><td>${esc(r.text)}</td></tr>`).join('')}</table></div>`
    : `<div class="hint" style="margin-top:6px">Source for <code>${esc(d.file || '?')}</code> isn't among the loaded constituents.</div>`);
});

$('revalidate').onclick = () => { diagnostics = session.validate(); renderDiagnostics(); };

$('diagFileFilter').addEventListener('change', () => { diagFilter.file = $('diagFileFilter').value; renderDiagnostics(); });
$('diagSevFilter').addEventListener('change', () => { diagFilter.severity = $('diagSevFilter').value; renderDiagnostics(); });

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
// Cytoscape + the dagre layout extension are loaded lazily from a CDN on
// first use (same rationale as Monaco: keep the base page light). The graph
// itself is built directly from the steps JSON — not from the `graphviz` DOT
// string also on the result, which is kept alongside as a raw-text fallback
// (mirroring the "raw engine output" collapsible) rather than parsed back.

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

/** Loads cytoscape.js then cytoscape-dagre (which self-registers onto `window.cytoscape` once it sees the global is present). */
function loadCytoscape() {
  if (cytoscapeLoadPromise) return cytoscapeLoadPromise;
  cytoscapeLoadPromise = (async () => {
    await loadScript(`https://cdn.jsdelivr.net/npm/cytoscape@${CYTOSCAPE_VERSION}/dist/cytoscape.min.js`);
    await loadScript(`https://cdn.jsdelivr.net/npm/cytoscape-dagre@${CYTOSCAPE_DAGRE_VERSION}/dist/cytoscape-dagre.min.js`);
    return window.cytoscape;
  })();
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

/** Create a Cytoscape instance inside `container` from `steps`, top-down (dagre) layout. `tipEl`, if given, shows the full KIF of whichever node was last tapped/hovered (node labels stay short). */
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

/**
 * Wire a `<details>` element to lazily render its proof graph the first time
 * it's opened (a hidden container has zero size, so Cytoscape can't lay out
 * until then), and re-fit on later opens. `getSteps()` is called fresh each
 * time so the same wiring keeps working across re-runs (Ask/Tell reuses one
 * `<details>`); call the returned `invalidate()` after `getSteps()`'s data
 * changes so an already-open graph re-renders instead of going stale.
 */
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

// -- Prover: tell + ask -------------------------------------------------------

$('prove').onclick = () => {
  const btn = $('prove');
  btn.disabled = true; btn.textContent = 'Proving…';
  setTimeout(() => {
    try {
      const cfg = new Config();
      cfg.wantProof = true;
      cfg.timeLimitSecs = Number($('ptime').value) || 0;
      session.configure(cfg);

      const SESS = 'user-assertions';
      session.flushSession(SESS);
      const assertions = $('assertions').value.trim();
      if (assertions) {
        const t = session.tell(assertions, SESS);
        if (!t.ok) throw new Error('assertion parse errors: ' + t.errors.slice(0, 3).join('; '));
      }
      renderProof(session.ask($('pquery').value, { session: SESS }));
    } catch (e) {
      $('proverResult').hidden = false;
      $('pStatus').textContent = 'Error'; $('pStatus').className = 'status InputError';
      $('pSteps').textContent = String(e && e.message || e);
      $('pProof').innerHTML = ''; $('pRaw').textContent = ''; $('pGraphDot').textContent = '';
      lastAskProof = [];
      invalidateAskGraph();
    } finally {
      btn.disabled = false; btn.textContent = 'Prove';
    }
  }, 0);
};

let lastAskProof = [];
const invalidateAskGraph = wireProofGraph(
  $('pGraphDetails'), $('pGraphContainer'), $('pGraphTip'), () => lastAskProof);

function renderProof(r) {
  $('proverResult').hidden = false;
  $('pStatus').textContent = r.status; $('pStatus').className = 'status ' + r.status;
  $('pSteps').textContent = r.given_steps != null ? `${r.given_steps} given-clause steps` : '';
  $('pProof').innerHTML = r.proof.map((s) => `<li><span class="rule">${esc(s.rule)}</span>: ${esc(s.kif)}</li>`).join('');
  $('pRaw').textContent = r.raw_output || '(none)';
  $('pGraphDot').textContent = r.graphviz || '(none)';
  lastAskProof = r.proof;
  invalidateAskGraph();
}

// -- Audit: whole-KB consistency check -----------------------------------------

$('runAudit').onclick = () => {
  const btn = $('runAudit');
  btn.disabled = true; btn.textContent = 'Auditing…';
  setTimeout(() => {
    try {
      const cfg = new Config();
      cfg.wantProof = true;
      cfg.timeLimitSecs = Number($('auditTime').value) || 0;
      session.configure(cfg);
      const limit = Math.max(1, Number($('auditLimit').value) || 5);
      renderAudit(session.auditConsistency(limit));
    } catch (e) {
      $('auditResult').innerHTML = `<div class="card hint" style="color:var(--bad)">${esc(String(e && e.message || e))}</div>`;
    } finally {
      btn.disabled = false; btn.textContent = 'Run audit';
    }
  }, 0);
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

  html += r.contradictions.map((c, i) => {
    const rows = c.steps.map((s) => {
      const gh = ghLink(s.file, s.line);
      const loc = s.file ? `${esc(s.file)}:${s.line}` : null;
      return `<li>
        <div class="hint">${esc(s.rule)}${s.premises.length ? ` <span class="hint">(from step${s.premises.length === 1 ? '' : 's'} ${s.premises.join(', ')})</span>` : ''}</div>
        <pre class="ref-kif">${highlightKif(s.kif).replace(/\n$/, '')}</pre>
        ${loc || gh ? `<div class="ref-meta">${loc ? `<span class="hint ref-loc">${loc}</span>` : ''}${gh ? `<a class="hint gh" href="${gh}" target="_blank" rel="noopener">GitHub ↗</a>` : ''}</div>` : ''}
      </li>`;
    }).join('');
    return `<div class="card">
      <div class="contradiction-hd">Contradiction #${i + 1} — ${c.steps.length} step${c.steps.length === 1 ? '' : 's'}</div>
      <ol class="refs">${rows}</ol>
      <details class="proof-graph-details" style="margin-top:10px">
        <summary class="hint">proof graph</summary>
        <div class="graph-container"></div>
        <div class="hint graph-tip"></div>
        <details class="graph-dot-toggle"><summary>graphviz (DOT) source</summary><pre>${esc(c.graphviz || '(none)')}</pre></details>
      </details>
    </div>`;
  }).join('');

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
//
// Monaco is loaded lazily from a CDN on first visit to the tab — it's a ~5MB
// AMD bundle, so the base page stays light until someone actually opens the
// editor. A single reused editor instance + model swaps content on file
// switch; diagnostics come from `validateFormula` (a scratch-session parse,
// no KB mutation) so markers track the buffer's own line numbers live as you
// type, independent of whatever file the text originated from.

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

/** Monarch tokenizer mirroring `highlightKif` (see the Ask/Tell editor) — parens,
 * operators, variables, strings, numbers, comments, and the relation/function
 * symbol immediately after `(` (via the `afterOpen` state). */
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
      [/./, { token: '@rematch', next: '@pop' }], // nested "(", string, etc. right after "(" — not a function name
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
  });
  // Colors mirror the CSS custom-highlighter tokens (.tok-paren/.tok-kw/…).
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

function runEditValidate() {
  if (!monacoEditor) return;
  let diags = [];
  try { diags = session.validateFormula(monacoEditor.getValue()); }
  catch (e) { $('editStatus').textContent = 'parse error: ' + (e && e.message || e); return; }
  monaco.editor.setModelMarkers(monacoEditor.getModel(), 'sigma', diagsToMarkers(diags));
  const errs = diags.filter((d) => d.severity === 'error').length;
  $('editStatus').textContent = diags.length
    ? `${diags.length} diagnostic${diags.length === 1 ? '' : 's'}${errs ? ` (${errs} error${errs === 1 ? '' : 's'})` : ''}`
    : 'no diagnostics';
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

function onEditPickerChange() {
  const val = $('editPicker').value;
  if (val === '__new__') {
    editCurrentFile = null;
    $('editNewNameWrap').hidden = false;
    $('editNewName').value = '';
    setEditorContent('; New KIF file\n');
    return;
  }
  $('editNewNameWrap').hidden = true;
  const sep = val.indexOf('|');
  const name = val.slice(0, sep), origin = val.slice(sep + 1);
  const c = constituents.find((x) => x.name === name && x.origin === origin);
  editCurrentFile = c ? { name: c.name, origin: c.origin } : null;
  setEditorContent(c ? c.text : '');
}

async function ensureEditorReady() {
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
  populateEditPicker();
  onEditPickerChange();
}

$('editPicker').addEventListener('change', onEditPickerChange);

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

// Not routed through `withBusy` — it hardcodes error output to #kbLog, which
// would silently misdirect Edit-tab errors into the (hidden) Knowledge base
// tab. Inline busy-toggle instead, matching the Ask/Tell and Audit handlers.
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
    // Only `file`-origin constituents persist to OPFS (see updateConstituentText) —
    // `sumo`/`url` ones are refetched from the network on every boot, so an
    // in-memory edit here is silently gone the moment the page reloads.
    if (origin === 'sumo' || origin === 'url') {
      alert(
        `"${name}" was loaded from ${origin === 'sumo' ? 'the SUMO GitHub repo' : 'a URL'}, ` +
        `not uploaded to this browser. Your changes are saved in memory for this session only — ` +
        `they will be discarded once you refresh the page.`
      );
    }
    const r = await updateConstituentText(name, text, origin);
    editCurrentFile = { name, origin };
    populateEditPicker();
    $('editPicker').value = `${name}|${origin}`;
    $('editNewNameWrap').hidden = true;
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

boot();
