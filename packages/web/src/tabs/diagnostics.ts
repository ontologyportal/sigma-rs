/** Diagnostics tab: the KB's validation findings, faceted filters, pagination,
 *  and the deep links into and out of them. */

import { state } from '../state.ts';
import { call } from '../rpc.ts';
import { $, esc, togglePanel, withBusy, targetEl } from '../dom.ts';
import { locLink, ghAnchor } from '../citations.ts';
import { navigate, updateParams, routeFromLocation } from '../router.ts';
import { markStatsStale } from './home-stats.ts';

const DIAG_SEV_ORDER = ['error', 'warning', 'info', 'hint'];
const DIAG_SEV_RANK = new Map(DIAG_SEV_ORDER.map((severity, index) => [severity, index]));

// A full SUMO load can produce thousands of Hint-severity completeness
// findings alone; pagination keeps `diagList` from rendering them all as DOM
// nodes at once.
const DIAG_PAGE_SIZE = 50;

const DIAG_DIMS = ['file', 'severity', 'kind', 'code'];

const diagFilter = { file: '', severity: '', kind: '', code: '', page: 0 };

/**
 * One pass over `diagnostics` yielding everything a render needs:
 *
 *   filtered  `{d, i}` pairs (`i` = index into the full array) matching every
 *             active filter — what is actually shown, and what deep-link
 *             scrolling pages against.
 *   counts    per dimension, value -> occurrences within that dimension's own
 *             pool: the diagnostics matching every active filter EXCEPT that
 *             one. That is what makes the dropdowns faceted (e.g. once
 *             Severity is Error, the File counts are error counts per file)
 *             rather than frozen at the unfiltered totals.
 *   totals    each pool's size, for the "All <label> (N)" row.
 *   errors    unfiltered error count, for the summary and the tab badge.
 *
 * A diagnostic belongs to dimension `dim`'s pool exactly when `dim` is its
 * ONLY mismatch, and to all four pools when it matches everything — which is
 * what lets one walk replace the five it used to take.
 */
function facetDiagnostics() {
  const counts = { file: new Map(), severity: new Map(), kind: new Map(), code: new Map() };
  const totals = { file: 0, severity: 0, kind: 0, code: 0 };
  const filtered = [];
  let errors = 0;
  const tally = (dim, d) => {
    totals[dim] += 1;
    const v = d[dim];
    if (v) counts[dim].set(v, (counts[dim].get(v) || 0) + 1);
  };
  state.diagnostics.forEach((d, i) => {
    if (d.severity === 'error') errors += 1;
    let missed = null, misses = 0;
    for (const dim of DIAG_DIMS) {
      if (diagFilter[dim] && d[dim] !== diagFilter[dim]) { missed = dim; if (++misses > 1) break; }
    }
    if (misses > 1) return;
    if (misses === 1) { tally(missed, d); return; }
    filtered.push({ d, i });
    for (const dim of DIAG_DIMS) tally(dim, d);
  });
  return { counts, totals, filtered, errors };
}

/**
 * Populate `sel` from `counts` (value -> occurrences, already faceted), each
 * option labelled with its count — `"All <label> (total)"` plus one row per
 * value, `"value (n)"`. `opts.order` fixes a leading sort order (severity's
 * error/warning/info/hint); unlisted values fall back to alphabetical after
 * it. `opts.labelFn` formats a value for display (raw value if omitted).
 *
 * Resets `diagFilter[stateKey]` to '' when its current value has no match —
 * a sibling filter changing (or a fresh validate()) can invalidate an
 * existing selection; this is the one place that's discovered and healed,
 * mirroring how a stale `?file=`/`?kind=` from the URL is handled the same
 * way on first load.
 */
function populateDiagFilter(sel, stateKey, counts, total, allLabel, opts: { order?: string[]; labelFn?: (v: string) => string } = {}) {
  if (!sel) return;
  const { order, labelFn } = opts;
  const values = [...counts.keys()].sort((a, b) => {
    const ia = order ? order.indexOf(a) : -1, ib = order ? order.indexOf(b) : -1;
    if (ia !== -1 || ib !== -1) return (ia === -1 ? 999 : ia) - (ib === -1 ? 999 : ib);
    return a < b ? -1 : a > b ? 1 : 0;
  });
  sel.innerHTML = `<option value="">${allLabel} (${total})</option>` +
    values.map((v) => `<option value="${esc(v)}">${esc(labelFn ? labelFn(v) : v)} (${counts.get(v)})</option>`).join('');
  if (state.diagnostics.length && diagFilter[stateKey] && !counts.has(diagFilter[stateKey])) {
    diagFilter[stateKey] = '';
  }
  sel.value = diagFilter[stateKey];
}

const activeFilters = () => DIAG_DIMS.map((k) => diagFilter[k]).join(' ');

/** Fill the four dropdowns and return the facets they were built from.
 *  `populateDiagFilter` heals a filter whose value matches nothing, which
 *  widens every OTHER dimension's pool — so a heal costs one more pass. It
 *  cannot cascade: clearing a filter only ever admits more diagnostics. */
function renderDiagFilters() {
  for (;;) {
    const before = activeFilters();
    const facets = facetDiagnostics();
    populateDiagFilter($('diagFileFilter'), 'file',     facets.counts.file,     facets.totals.file,     'All files');
    populateDiagFilter($('diagSevFilter'),  'severity', facets.counts.severity, facets.totals.severity, 'All severities',
      { order: DIAG_SEV_ORDER, labelFn: (s) => s[0].toUpperCase() + s.slice(1) });
    populateDiagFilter($('diagKindFilter'), 'kind',     facets.counts.kind,     facets.totals.kind,     'All types');
    populateDiagFilter($('diagCodeFilter'), 'code',     facets.counts.code,     facets.totals.code,     'All codes');
    if (activeFilters() === before) return facets;
  }
}

export function renderDiagnostics() {
  const { filtered, errors: errs } = renderDiagFilters();

  // Active-filter count on the Filter button, so it's meaningful collapsed.
  const activeCount = DIAG_DIMS.filter((k) => diagFilter[k]).length;
  const badge = $('diagFilterBadge');
  if (badge) badge.textContent = activeCount || '';

  // Count chip on the Diagnostics tab button — glanceable KB health from any
  // tab. Hidden at zero; colored by worst severity present.
  const tabBtn = document.querySelector('nav.tabs [data-tab="diagnostics"]');
  if (tabBtn) {
    tabBtn.querySelector('.tab-badge')?.remove();
    if (state.diagnostics.length) {
      tabBtn.insertAdjacentHTML('beforeend',
        `<span class="tab-badge${errs ? ' err' : ''}">${state.diagnostics.length}</span>`);
    }
  }

  const filterActive = diagFilter.file || diagFilter.severity || diagFilter.kind || diagFilter.code;
  const sum = $('diagSummary');
  if (sum) sum.innerHTML = state.diagnostics.length
    ? (filterActive
        ? `<b>${filtered.length}</b> of <b>${state.diagnostics.length}</b> diagnostic${state.diagnostics.length === 1 ? '' : 's'} shown`
        : `<b>${state.diagnostics.length}</b> diagnostic${state.diagnostics.length === 1 ? '' : 's'}`) +
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
    }).join('') : `<div class="hint">${state.diagnostics.length ? 'No diagnostics match the current filters.' : ''}</div>`;
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

$('revalidate').onclick = () => withBusy($('revalidate'), async () => {
  state.diagnostics = (await call('validate')).diagnostics;
  markStatsStale();
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
 *
 * The tab comes from `routeFromLocation`, not from a raw `?tab=` read: routing
 * is path-based, so on /diagnostics there is no `tab` param to find.
 */
export function applyDiagRouteParams() {
  const { tab, params } = routeFromLocation();
  if (tab !== 'diagnostics') return;
  const next = {
    file:     params.get('file') || '',
    severity: params.get('sev')  || '',
    kind:     params.get('kind') || '',
    code:     params.get('code') || '',
  };
  const changed = DIAG_DIMS.some((k) => diagFilter[k] !== next[k]);
  Object.assign(diagFilter, next);
  diagFilter.page = Math.max(0, (Number(params.get('p')) || 1) - 1);
  // A shared/deep-linked URL that already carries a filter should show it
  // expanded, not hide the active filter behind a collapsed button. Only when
  // the URL actually brought something new: re-applying the same filters after
  // a promote must not reopen a panel the user has since collapsed.
  if (changed && DIAG_DIMS.some((k) => diagFilter[k])) {
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
  const { filtered } = facetDiagnostics();
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
  const a = targetEl(e).closest<HTMLElement>('a.jump-diag');
  if (!a) return;
  e.preventDefault();
  navigate('diagnostics', { file: a.dataset.file, l: a.dataset.line });
});
