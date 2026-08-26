/** Browse (Home) tab: search → results → man page, plus the `/` and Esc
 *  shortcuts that drive it. */

import { state } from '../state.ts';
import { call } from '../rpc.ts';
import { $, esc, targetEl } from '../dom.ts';
import { kifCiteRow } from '../proof-view.ts';
import { taxonomyWidget, fillAncestors } from './taxonomy.ts';
import { currentTab, showTab, updateParams } from '../router.ts';

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
export function setBrowseHome(show) {
  const el = $('browseHome');
  if (el) el.hidden = !show;
  if (show) $('browseView').innerHTML = '';
}

// The welcome card's example queries.
document.addEventListener('click', (e) => {
  const a = targetEl(e).closest<HTMLElement>('a.try-q');
  if (!a) return;
  e.preventDefault();
  $('q').value = a.textContent;
  updateParams({ q: a.textContent });
  runSearch(a.textContent);
});

// Only the newest in-flight search may render: typing "proc" fires four
// requests and the slower ones must not clobber the freshest results.
let searchSeq = 0;

export async function runSearch(query) {
  if (!query) { setBrowseHome(true); return; }
  const seq = ++searchSeq;
  setBrowseHome(false);
  let { hits } = await call('search', { query, limit: 100, language: state.uiLanguage });
  if (seq !== searchSeq) return;
  // The language filter is a preference, not a wall: a KB documented in
  // another language must stay searchable on the default (English) setting.
  let langNote = '';
  if (hits.length === 0) {
    ({ hits } = await call('search', { query, limit: 100 }));
    if (seq !== searchSeq) return;
    if (hits.length) langNote = ` (no ${esc(langLabel(state.uiLanguage))} matches — showing all languages)`;
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
export function langLabel(symbol) {
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
  return pick(state.uiLanguage).length ? pick(state.uiLanguage)
    : pick('EnglishLanguage').length ? pick('EnglishLanguage')
    : entries;
}

export async function openManPage(symbol) {
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

// -- The man page's reference list --------------------------------------------

/** Reference-filter <select>, offering only the categories present among `refs`.
 *  Value encoding: '' = all; 'fact' = any plain fact (a relation atom, possibly
 *  under `not`); 'fact:<n>' = plain fact with the symbol at argument n (0 = the
 *  relation itself); '=>' '<=>' 'and' 'or' = a top-level logical operator.
 *  `kind`/`arg_pos` come from the core classification in `manpage_to_js`. */
function refFilterControl(refs) {
  const facts = refs.filter((r) => r.kind === 'fact');
  const positions = [...new Set(facts.map((r) => r.arg_pos).filter((n) => n != null))].sort((a: number, b: number) => a - b);
  const ops = [['=>', 'Implications (⇒)'], ['<=>', 'Biconditionals (⇔)'], ['and', 'Conjunctions (and)'], ['or', 'Disjunctions (or)']]
    .filter(([k]) => refs.some((r) => r.kind === k));
  const opt = (v, label) => `<option value="${esc(v)}">${esc(label)}</option>`;
  const posLabel = (n) => n === 0 ? 'symbol as the relation (arg 0)' : `symbol as argument ${n}`;
  const count = (pred) => refs.filter(pred).length;
  return `<label class="ref-filter"><span class="hint">Filter</span>
    <select id="refFilter">
      ${opt('', `All (${refs.length})`)}
      ${facts.length ? opt('fact', `Plain facts (${facts.length})`) : ''}
      ${positions.map((n) => opt(`fact:${n}`, `  ${posLabel(n)} (${count((r) => r.kind === 'fact' && r.arg_pos === n)})`)).join('')}
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
