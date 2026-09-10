/** Diagnostics tab state: the KB's validation findings, faceted filters,
 *  pagination, and the deep links into and out of them.
 *
 * Reactive state (not DOM ids) so `DiagnosticsTab.vue` can bind to it
 * directly; `renderDiagnostics`/`applyDiagRouteParams` stay the entry points
 * `kb.ts`'s validate flow and `router.ts`'s route dispatch call into --
 * both touch nearly all the state here, which is why it stays a companion
 * module rather than component-local. The pager/filter-select UI handlers
 * (nothing outside `DiagnosticsTab.vue` calls them) live in the component
 * directly instead. */

import { reactive, ref, nextTick } from 'vue';
import type { LocationQuery } from 'vue-router';
import { state } from '../state.ts';
import { locLink, ghAnchor } from '../citations.ts';
import { navigate } from '../router.ts';
import { targetEl } from '../dom.ts';

const DIAG_SEV_ORDER = ['error', 'warning', 'info', 'hint'];

// A full SUMO load can produce thousands of Hint-severity completeness
// findings alone; pagination keeps the list from rendering them all as DOM
// nodes at once.
const DIAG_PAGE_SIZE = 50;

const DIAG_DIMS = ['file', 'severity', 'kind', 'code'] as const;

export const diagFilter = reactive({ file: '', severity: '', kind: '', code: '', page: 0 });
export const diagFilterOpen = ref(false);

export const diagSummaryHtml = ref('');
export const diagFilterBadge = ref<number | ''>('');
export const diagFilterSelects = reactive({
  file: [] as { value: string; label: string }[],
  severity: [] as { value: string; label: string }[],
  kind: [] as { value: string; label: string }[],
  code: [] as { value: string; label: string }[],
});
export const diagPageItems = ref<{ i: number; sev: string; locHtml: string; kind: string; code: string; ghHtml: string; message: string }[]>([]);
export const diagEmptyHint = ref('');
export const diagPager = ref<{ page: number; pageCount: number; from: number; to: number; total: number } | null>(null);

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

/** Options for one filter select, each labelled with its (already faceted)
 *  count — `"All <label> (total)"` plus one row per value, `"value (n)"`.
 *  `opts.order` fixes a leading sort order (severity's error/warning/info/hint);
 *  unlisted values fall back to alphabetical after it. `opts.labelFn` formats
 *  a value for display (raw value if omitted).
 *
 *  Resets `diagFilter[stateKey]` to '' when its current value has no match —
 *  a sibling filter changing (or a fresh validate()) can invalidate an
 *  existing selection; this is the one place that's discovered and healed,
 *  mirroring how a stale `?file=`/`?kind=` from the URL is handled the same
 *  way on first load. */
function buildDiagFilterOptions(stateKey, counts, total, allLabel, opts: { order?: string[]; labelFn?: (v: string) => string } = {}) {
  const { order, labelFn } = opts;
  const values = [...counts.keys()].sort((a, b) => {
    const ia = order ? order.indexOf(a) : -1, ib = order ? order.indexOf(b) : -1;
    if (ia !== -1 || ib !== -1) return (ia === -1 ? 999 : ia) - (ib === -1 ? 999 : ib);
    return a < b ? -1 : a > b ? 1 : 0;
  });
  if (state.diagnostics.length && diagFilter[stateKey] && !counts.has(diagFilter[stateKey])) {
    diagFilter[stateKey] = '';
  }
  return [
    { value: '', label: `${allLabel} (${total})` },
    ...values.map((v) => ({ value: v, label: `${labelFn ? labelFn(v) : v} (${counts.get(v)})` })),
  ];
}

const activeFilters = () => DIAG_DIMS.map((k) => diagFilter[k]).join(' ');

/** Fill the four filter selects and return the facets they were built from.
 *  `buildDiagFilterOptions` heals a filter whose value matches nothing, which
 *  widens every OTHER dimension's pool — so a heal costs one more pass. It
 *  cannot cascade: clearing a filter only ever admits more diagnostics. */
function computeDiagFilters() {
  for (;;) {
    const before = activeFilters();
    const facets = facetDiagnostics();
    diagFilterSelects.file = buildDiagFilterOptions('file', facets.counts.file, facets.totals.file, 'All files');
    diagFilterSelects.severity = buildDiagFilterOptions('severity', facets.counts.severity, facets.totals.severity, 'All severities',
      { order: DIAG_SEV_ORDER, labelFn: (s) => s[0].toUpperCase() + s.slice(1) });
    diagFilterSelects.kind = buildDiagFilterOptions('kind', facets.counts.kind, facets.totals.kind, 'All types');
    diagFilterSelects.code = buildDiagFilterOptions('code', facets.counts.code, facets.totals.code, 'All codes');
    if (activeFilters() === before) return facets;
  }
}

export function renderDiagnostics() {
  const { filtered, errors: errs } = computeDiagFilters();

  // Active-filter count on the Filter button, so it's meaningful collapsed.
  const activeCount = DIAG_DIMS.filter((k) => diagFilter[k]).length;
  diagFilterBadge.value = activeCount || '';

  // Count chip on the Diagnostics tab button — glanceable KB health from any
  // tab. Hidden at zero; colored by worst severity present. The tab bar is
  // static shell markup (outside any Vue mount point), so this stays a
  // direct DOM write, same as the shell's own tab-switching code.
  const tabBtn = document.querySelector('nav.tabs [data-tab="diagnostics"]');
  if (tabBtn) {
    tabBtn.querySelector('.tab-badge')?.remove();
    if (state.diagnostics.length) {
      tabBtn.insertAdjacentHTML('beforeend',
        `<span class="tab-badge${errs ? ' err' : ''}">${state.diagnostics.length}</span>`);
    }
  }

  const filterActive = diagFilter.file || diagFilter.severity || diagFilter.kind || diagFilter.code;
  diagSummaryHtml.value = state.diagnostics.length
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

  diagPageItems.value = pageItems.map(({ d, i }) => ({
    i,
    sev: d.severity,
    locHtml: d.file ? locLink(d.file, d.line, 'loc') : '<span class="loc">(no location)</span>',
    kind: d.kind,
    code: d.code,
    ghHtml: ghAnchor(d.file, d.line),
    message: d.message,
  }));
  diagEmptyHint.value = pageItems.length ? '' : (state.diagnostics.length ? 'No diagnostics match the current filters.' : '');
  diagPager.value = renderDiagPagerInfo(filtered.length, pageCount);
}

/** Prev/Next pager info beneath the diagnostics list — `null` entirely when
 *  everything fits on one page. */
function renderDiagPagerInfo(total, pageCount) {
  if (pageCount <= 1) return null;
  const page = diagFilter.page;
  const from = total ? page * DIAG_PAGE_SIZE + 1 : 0;
  const to = Math.min(total, from + DIAG_PAGE_SIZE - 1);
  return { page, pageCount, from, to, total };
}

export function toggleDiagFilterPanel(force?: boolean) {
  diagFilterOpen.value = force !== undefined ? force : !diagFilterOpen.value;
}

/**
 * Apply ?file / ?sev / ?kind / ?code / ?p / ?l to the Diagnostics tab, from
 * `query` (the diagnostics route's own `LocationQuery` -- see `router.ts`'s
 * `applyTabEffects`, the only caller while actually ON that route). Called
 * both when the route is applied and again once validation finishes — on a
 * cold load the route runs before any diagnostics exist, so the first pass
 * has nothing to filter or scroll to. `?l` (deep-link to one diagnostic)
 * takes precedence over `?p` — it jumps to whatever page that diagnostic
 * actually falls on.
 *
 * `kb.ts`'s post-validate re-apply doesn't have a route to hand in (it may
 * run while a different tab is showing) -- it passes `null` and this is a
 * no-op then, matching the old `routeFromLocation` guard's intent.
 */
export function applyDiagRouteParams(query: LocationQuery | null) {
  if (!query) return;
  const str = (v: string | (string | null)[] | null | undefined) => (typeof v === 'string' ? v : '') || '';
  const next = {
    file:     str(query.file),
    severity: str(query.sev),
    kind:     str(query.kind),
    code:     str(query.code),
  };
  const changed = DIAG_DIMS.some((k) => diagFilter[k] !== next[k]);
  Object.assign(diagFilter, next);
  diagFilter.page = Math.max(0, (Number(query.p) || 1) - 1);
  // A shared/deep-linked URL that already carries a filter should show it
  // expanded, not hide the active filter behind a collapsed button. Only when
  // the URL actually brought something new: re-applying the same filters after
  // a promote must not reopen a panel the user has since collapsed.
  if (changed && DIAG_DIMS.some((k) => diagFilter[k])) {
    toggleDiagFilterPanel(true);
  }
  renderDiagnostics();
  const line = Number(query.l);
  if (Number.isFinite(line) && line > 0) scrollToDiagnostic(line);
}

/**
 * Jump to whichever page contains the diagnostic nearest `line` (within the
 * active file/severity filter) and flash it. Nearest rather than exact: the
 * caller's line comes from an edited buffer, whose line numbers drift from
 * the KB's as soon as anything above is inserted.
 */
async function scrollToDiagnostic(line) {
  const { filtered } = facetDiagnostics();
  let bestPos = -1, bestDist = Infinity;
  filtered.forEach(({ d }, pos) => {
    const dist = Math.abs((d.line || 0) - line);
    if (dist < bestDist) { bestDist = dist; bestPos = pos; }
  });
  if (bestPos < 0) return;
  diagFilter.page = Math.floor(bestPos / DIAG_PAGE_SIZE);
  renderDiagnostics();
  await nextTick(); // let Vue patch the list for the new page before querying it

  const el = document.querySelector(`#diagList .diag[data-i="${filtered[bestPos].i}"]`);
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
