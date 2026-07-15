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
const rawUrl = (path) => `https://raw.githubusercontent.com/${SUMO.owner}/${SUMO.repo}/${SUMO.ref}/${path}`;

const $ = (id) => document.getElementById(id);
const esc = (s) => String(s).replace(/[&<>]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' }[c]));

let session = null;
let diagnostics = [];
let constituents = [];   // [{ name, text }] — cached so remove/reset rebuild without refetch
let sumoCatalog = null;  // cached list of *.kif paths in the repo

function newSession() {
  const cfg = new Config();
  cfg.wantProof = true;
  return new Session({ config: cfg });
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
  refreshAll();
  return { added: true, notices };
}

/** Rebuild the session from the current (cached) constituents — used by remove/reset. */
function rebuildSession() {
  session = newSession();
  for (const c of constituents) session.kb.loadKif(c.text, c.name);
  refreshAll();
}

function removeConstituent(name) {
  constituents = constituents.filter((c) => c.name !== name);
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
    $('overlayMsg').textContent = 'Loading the WASM module…';
    await init();
    session = newSession();

    $('overlayMsg').textContent = `Fetching ${MERGE} from ${SUMO.owner}/${SUMO.repo}…`;
    const text = await fetchText(rawUrl(MERGE));
    $('overlayMsg').textContent = 'Loading & validating…';
    addConstituent(MERGE, text);

    $('overlay').remove();
  } catch (e) {
    $('overlayMsg').textContent = 'Failed to load SUMO.';
    $('overlayErr').textContent = String(e && e.message || e) + '  (GitHub reachable? Try reloading.)';
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
      ${c.name === MERGE ? '<span class="hint">core</span>' : `<a class="rm" data-name="${esc(c.name)}">remove</a>`}
    </li>`).join('');
}

$('loadedList').addEventListener('click', (e) => {
  const rm = e.target.closest('.rm');
  if (rm) { $('kbLog').textContent = ''; removeConstituent(rm.dataset.name); }
});
$('resetKb').onclick = () => { $('kbLog').textContent = ''; resetToMerge(); };

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
  const loaded = new Set(constituents.map((c) => c.name));
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
  const r = addConstituent(file.name, await file.text(), 'file');
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

function openManPage(symbol) {
  const p = session.manpage(symbol);
  if (!p) { $('homeView').innerHTML = `<div class="card hint">No man page for <code>${esc(symbol)}</code>.</div>`; return; }

  const docs = (v) => v.map((d) => `<div>${esc(d.text)} <span class="hint">(${esc(d.language)})</span></div>`).join('');
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
      ${field('References', `appears in ${p.appears_in_count} formula${p.appears_in_count === 1 ? '' : 's'}; consequent of ${p.consequent_count}`)}
    </div>`;
  $('homeView').querySelector('.back').onclick = () => runSearch($('q').value.trim());
}

// -- Diagnostics --------------------------------------------------------------

function renderDiagnostics() {
  const errs = diagnostics.filter((d) => d.severity === 'Error').length;
  const sum = $('diagSummary');
  if (sum) sum.innerHTML = diagnostics.length
    ? `<b>${diagnostics.length}</b> diagnostic${diagnostics.length === 1 ? '' : 's'}` +
      (errs ? ` (${errs} error${errs === 1 ? '' : 's'})` : '') +
      ` — click a <span class="loc">file:line</span> to view the source`
    : 'No diagnostics — the loaded KB is clean.';

  const list = $('diagList');
  if (!list) return;
  list.innerHTML = diagnostics.map((d, i) => {
    const gh = ghLink(d.file, d.line);
    const loc = d.file ? `${esc(d.file)}:${d.line}` : '(no location)';
    return `<div class="diag" data-i="${i}">
      <div class="diag-head">
        <span class="sev ${esc(d.severity)}">${esc(d.severity)}</span>
        <a class="loc">${loc}</a>
        <span class="code">[${esc(d.kind)}/${esc(d.code)}]</span>
        ${gh ? `<a class="hint gh" href="${gh}" target="_blank" rel="noopener">GitHub ↗</a>` : ''}
        <span class="msg">${esc(d.message)}</span>
      </div>
    </div>`;
  }).join('');
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
      $('pProof').innerHTML = ''; $('pRaw').textContent = '';
    } finally {
      btn.disabled = false; btn.textContent = 'Prove';
    }
  }, 0);
};

function renderProof(r) {
  $('proverResult').hidden = false;
  $('pStatus').textContent = r.status; $('pStatus').className = 'status ' + r.status;
  $('pSteps').textContent = r.given_steps != null ? `${r.given_steps} given-clause steps` : '';
  $('pProof').innerHTML = r.proof.map((s) => `<li><span class="rule">${esc(s.rule)}</span>: ${esc(s.kif)}</li>`).join('');
  $('pRaw').textContent = r.raw_output || '(none)';
}

boot();
