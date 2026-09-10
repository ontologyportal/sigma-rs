<script setup lang="ts">
import { computed, nextTick, ref } from "vue";
import BusyButton from "../components/BusyButton.vue";
import Card from "../components/Card.vue";
import SourceLoc from "../components/SourceLoc.vue";
import { useTabQuery } from "../composables/useTabQuery";
import { updateParams } from "../router";
import { useKBStore, type Diagnostic } from "../stores/kb";

const DIAG_SEV_ORDER = ["error", "warning", "info", "hint"];

// A full SUMO load can produce thousands of Hint-severity completeness
// findings alone; pagination keeps the list from rendering them all as DOM
// nodes at once.
const DIAG_PAGE_SIZE = 50;

const DIAG_DIMS = ["file", "severity", "kind", "code"] as const;
type Dim = (typeof DIAG_DIMS)[number];
type Filter = Record<Dim, string>;
type Counts = Record<Dim, Map<string, number>>;
type Option = { value: string; label: string };
type Facets = {
  counts: Counts;
  totals: Record<Dim, number>;
  filtered: { d: Diagnostic; i: number }[];
  errors: number;
};

const DIM_LABEL: Record<Dim, string> = {
  file: "File",
  severity: "Severity",
  kind: "Type",
  code: "Code",
};

/**
 * One pass over `diagnostics` yielding everything a render needs:
 *
 *   filtered  `{d, i}` pairs (`i` = index into the full array) matching every
 *             active filter -- what is actually shown, and what deep-link
 *             scrolling pages against.
 *   counts    per dimension, value -> occurrences within that dimension's own
 *             pool: the diagnostics matching every active filter EXCEPT that
 *             one. That is what makes the dropdowns faceted (e.g. once
 *             Severity is Error, the File counts are error counts per file)
 *             rather than frozen at the unfiltered totals.
 *   totals    each pool's size, for the "All <label> (N)" row.
 *   errors    unfiltered error count, for the summary.
 *
 * A diagnostic belongs to dimension `dim`'s pool exactly when `dim` is its
 * ONLY mismatch, and to all four pools when it matches everything -- which is
 * what lets one walk replace the five it used to take.
 */
function facetDiagnostics(diagnostics: Diagnostic[], filter: Filter): Facets {
  const counts: Counts = {
    file: new Map(),
    severity: new Map(),
    kind: new Map(),
    code: new Map(),
  };
  const totals: Record<Dim, number> = {
    file: 0,
    severity: 0,
    kind: 0,
    code: 0,
  };
  const filtered: { d: Diagnostic; i: number }[] = [];
  let errors = 0;
  const tally = (dim: Dim, d: Diagnostic) => {
    totals[dim] += 1;
    const v = d[dim];
    if (v) counts[dim].set(v, (counts[dim].get(v) || 0) + 1);
  };
  diagnostics.forEach((d, i) => {
    if (d.severity === "error") errors += 1;
    let missed: Dim | null = null;
    let misses = 0;
    for (const dim of DIAG_DIMS) {
      if (filter[dim] && d[dim] !== filter[dim]) {
        missed = dim;
        if (++misses > 1) break;
      }
    }
    if (misses > 1) return;
    if (misses === 1) {
      tally(missed!, d);
      return;
    }
    filtered.push({ d, i });
    for (const dim of DIAG_DIMS) tally(dim, d);
  });
  return { counts, totals, filtered, errors };
}

/** Options for one filter select, each labelled with its (already faceted)
 *  count -- `"All <label> (total)"` plus one row per value, `"value (n)"`.
 *  `opts.order` fixes a leading sort order (severity's error/warning/info/hint);
 *  unlisted values fall back to alphabetical after it. `opts.labelFn` formats
 *  a value for display (raw value if omitted). */
function buildDiagFilterOptions(
  counts: Map<string, number>,
  total: number,
  allLabel: string,
  opts: { order?: string[]; labelFn?: (v: string) => string } = {},
): Option[] {
  const { order, labelFn } = opts;
  const values = [...counts.keys()].sort((a, b) => {
    const ia = order ? order.indexOf(a) : -1;
    const ib = order ? order.indexOf(b) : -1;
    if (ia !== -1 || ib !== -1)
      return (ia === -1 ? 999 : ia) - (ib === -1 ? 999 : ib);
    return a < b ? -1 : a > b ? 1 : 0;
  });
  return [
    { value: "", label: `${allLabel} (${total})` },
    ...values.map((v) => ({
      value: v,
      label: `${labelFn ? labelFn(v) : v} (${counts.get(v)})`,
    })),
  ];
}

const kb = useKBStore();
const { query, onQuery, str, num } = useTabQuery(["diagnostics"]);

const filter = computed<Filter>(() => ({
  file: str(query.value.file),
  severity: str(query.value.sev),
  kind: str(query.value.kind),
  code: str(query.value.code),
}));
const page = computed(() => Math.max(0, (num(query.value.p) ?? 1) - 1));

/** The URL's filter with any value that matches nothing in its dimension
 *  cleared -- a sibling filter, a fresh validate(), or a stale deep link can
 *  all leave a selection with no match. Clearing one filter widens every
 *  OTHER dimension's pool, so a heal costs one more pass; it cannot cascade
 *  (clearing only ever admits more diagnostics). The healed value is never
 *  written back to the URL. */
const healed = computed<{ filter: Filter; facets: Facets }>(() => {
  const diagnostics = kb.diagnostics;
  let f = filter.value;
  for (;;) {
    const facets = facetDiagnostics(diagnostics, f);
    const next = { ...f };
    let changed = false;
    for (const dim of DIAG_DIMS) {
      if (
        diagnostics.length &&
        next[dim] &&
        !facets.counts[dim].has(next[dim])
      ) {
        next[dim] = "";
        changed = true;
      }
    }
    if (!changed) return { filter: f, facets };
    f = next;
  }
});
const effectiveFilter = computed(() => healed.value.filter);
const facets = computed(() => healed.value.facets);

const selectOptions = computed<Record<Dim, Option[]>>(() => {
  const { counts, totals } = facets.value;
  return {
    file: buildDiagFilterOptions(counts.file, totals.file, "All files"),
    severity: buildDiagFilterOptions(
      counts.severity,
      totals.severity,
      "All severities",
      {
        order: DIAG_SEV_ORDER,
        labelFn: (s) => s[0].toUpperCase() + s.slice(1),
      },
    ),
    kind: buildDiagFilterOptions(counts.kind, totals.kind, "All types"),
    code: buildDiagFilterOptions(counts.code, totals.code, "All codes"),
  };
});

const activeCount = computed(
  () => DIAG_DIMS.filter((k) => effectiveFilter.value[k]).length,
);
const filterActive = computed(() => activeCount.value > 0);
const total = computed(() => kb.diagnostics.length);
const errors = computed(() => facets.value.errors);
const filteredCount = computed(() => facets.value.filtered.length);

// Clamp the page to the current (possibly just-filtered/shrunk) result set
// so a filter change or a smaller re-validation never strands the view
// past the end.
const pageCount = computed(() =>
  Math.max(1, Math.ceil(filteredCount.value / DIAG_PAGE_SIZE)),
);
const pageIndex = computed(() => Math.min(page.value, pageCount.value - 1));
const pageItems = computed(() => {
  const start = pageIndex.value * DIAG_PAGE_SIZE;
  return facets.value.filtered.slice(start, start + DIAG_PAGE_SIZE);
});
const emptyHint = computed(() =>
  pageItems.value.length
    ? ""
    : total.value
      ? "No diagnostics match the current filters."
      : "",
);
/** Prev/Next pager info beneath the list -- `null` entirely when everything
 *  fits on one page. */
const pager = computed(() => {
  if (pageCount.value <= 1) return null;
  const from = filteredCount.value ? pageIndex.value * DIAG_PAGE_SIZE + 1 : 0;
  const to = Math.min(filteredCount.value, from + DIAG_PAGE_SIZE - 1);
  return {
    page: pageIndex.value,
    pageCount: pageCount.value,
    from,
    to,
    total: filteredCount.value,
  };
});

const filterOpen = ref(false);
const revalidating = ref(false);
const listRef = ref<HTMLElement | null>(null);
let flashTimer: ReturnType<typeof setTimeout> | null = null;

function toggleFilterPanel() {
  filterOpen.value = !filterOpen.value;
}

/** The URL keys for a filter (`p` is 1-based in the URL -- friendlier to
 *  read/type than the internal 0-based index; omitted on the first page). */
function filterParams(f: Filter, pageIdx: number) {
  return {
    file: f.file,
    sev: f.severity,
    kind: f.kind,
    code: f.code,
    p: pageIdx ? pageIdx + 1 : null,
  };
}

function setFilter(dim: Dim, value: string) {
  const next = { ...effectiveFilter.value, [dim]: value };
  // Picking a type invalidates a code chosen under a different type -- clear
  // it rather than leave a stale, now-impossible combination active.
  if (dim === "kind") next.code = "";
  updateParams(filterParams(next, 0));
}

function onSelect(dim: Dim, e: Event) {
  setFilter(dim, (e.target as HTMLSelectElement).value);
}

function goToPage(pageIdx: number) {
  updateParams(filterParams(effectiveFilter.value, pageIdx));
}

async function revalidate() {
  revalidating.value = true;
  try {
    await kb.validate();
  } finally {
    revalidating.value = false;
  }
}

/**
 * Jump to whichever page contains the diagnostic nearest `line` (within the
 * active filters) and flash it. Nearest rather than exact: the caller's line
 * comes from an edited buffer, whose line numbers drift from the KB's as soon
 * as anything above is inserted. When that page is not the one showing, the
 * URL is moved onto it (dropping `l`, so the re-run of the query handler has
 * nothing left to do).
 */
async function scrollToDiagnostic(line: number) {
  const { filtered } = facets.value;
  let bestPos = -1;
  let bestDist = Infinity;
  filtered.forEach(({ d }, pos) => {
    const dist = Math.abs((d.line || 0) - line);
    if (dist < bestDist) {
      bestDist = dist;
      bestPos = pos;
    }
  });
  if (bestPos < 0) return;
  const targetPage = Math.floor(bestPos / DIAG_PAGE_SIZE);
  if (targetPage !== pageIndex.value) {
    const { l: _l, ...rest } = query.value;
    await updateParams({ ...rest, p: targetPage ? targetPage + 1 : null });
  }
  await nextTick();

  const el = listRef.value?.querySelector<HTMLElement>(
    `.diag[data-i="${filtered[bestPos].i}"]`,
  );
  if (!el) return;
  el.scrollIntoView({ block: "center", behavior: "smooth" });
  if (flashTimer) clearTimeout(flashTimer);
  listRef.value
    ?.querySelectorAll(".diag-target")
    .forEach((n) => n.classList.remove("diag-target"));
  el.classList.add("diag-target");
  flashTimer = setTimeout(() => {
    el.classList.remove("diag-target");
    flashTimer = null;
  }, 2200);
}

let lastApplied: Filter | null = null;
onQuery((q) => {
  const next: Filter = {
    file: str(q.file),
    severity: str(q.sev),
    kind: str(q.kind),
    code: str(q.code),
  };
  // A shared/deep-linked URL that already carries a filter should show it
  // expanded, not hide the active filter behind a collapsed button. Only when
  // the URL actually brought something new: re-applying the same filters must
  // not reopen a panel the user has since collapsed.
  const changed =
    !lastApplied || DIAG_DIMS.some((k) => lastApplied![k] !== next[k]);
  lastApplied = next;
  if (changed && DIAG_DIMS.some((k) => next[k])) filterOpen.value = true;
  // `?l` (deep-link to one diagnostic) takes precedence over `?p` -- it jumps
  // to whatever page that diagnostic actually falls on.
  const line = num(q.l);
  if (line) scrollToDiagnostic(line);
});
</script>

<template>
  <Card>
    <div class="inline between">
      <div class="hint">
        <template v-if="total">
          <template v-if="filterActive">
            <b>{{ filteredCount }}</b> of <b>{{ total }}</b> diagnostic{{
              total === 1 ? "" : "s"
            }}
            shown
          </template>
          <template v-else>
            <b>{{ total }}</b> diagnostic{{ total === 1 ? "" : "s" }}
          </template>
          <template v-if="errors">
            ({{ errors }} error{{ errors === 1 ? "" : "s" }} total)</template
          >
          — click a <span class="loc">file:line</span> to open it in the editor
        </template>
        <template v-else>No diagnostics — the loaded KB is clean.</template>
      </div>
      <div class="inline tight">
        <button
          class="btn ghost filter-btn"
          type="button"
          :aria-expanded="filterOpen"
          title="Filter diagnostics"
          @click="toggleFilterPanel"
        >
          <svg
            viewBox="0 0 16 16"
            width="13"
            height="13"
            fill="currentColor"
            aria-hidden="true"
          >
            <path
              d="M1 2.75A.75.75 0 0 1 1.75 2h12.5a.75.75 0 0 1 .6 1.2L10 9.65v4.6a.75.75 0 0 1-1.14.64l-2.5-1.5A.75.75 0 0 1 6 12.75V9.65L1.15 3.2A.75.75 0 0 1 1 2.75Z"
            />
          </svg>
          Filter<span class="filter-badge">{{ activeCount || "" }}</span>
        </button>
        <BusyButton
          :busy="revalidating"
          label="Re-validate"
          @click="revalidate"
        />
      </div>
    </div>
    <div class="settings" v-show="filterOpen">
      <div class="diag-filter-stack">
        <div v-for="dim in DIAG_DIMS" :key="dim">
          <label :for="`diag-filter-${dim}`">{{ DIM_LABEL[dim] }}</label>
          <select
            :id="`diag-filter-${dim}`"
            :value="effectiveFilter[dim]"
            @change="onSelect(dim, $event)"
          >
            <option
              v-for="o in selectOptions[dim]"
              :key="o.value"
              :value="o.value"
            >
              {{ o.label }}
            </option>
          </select>
        </div>
      </div>
    </div>
    <div ref="listRef" class="mt">
      <div v-if="emptyHint" class="hint">{{ emptyHint }}</div>
      <div
        v-for="{ i, d } in pageItems"
        :key="i"
        class="diag"
        :data-i="i"
        :data-sev="d.severity"
      >
        <div class="diag-head">
          <span class="sev" :class="d.severity">{{ d.severity }}</span>
          <SourceLoc
            v-if="d.file"
            :file="d.file"
            :line="d.line"
            variant="loc"
          />
          <span v-else class="loc">(no location)</span>
          <span class="code">[{{ d.kind }}/{{ d.code }}]</span>
          <span class="msg">{{ d.message }}</span>
        </div>
      </div>
    </div>
    <div class="diag-pager" v-if="pager">
      <button
        class="btn ghost"
        type="button"
        :disabled="pager.page === 0"
        @click="goToPage(pager.page - 1)"
      >
        ‹ Prev
      </button>
      <span class="hint">
        {{ pager.from }}–{{ pager.to }} of {{ pager.total }} · page
        {{ pager.page + 1 }} of
        {{ pager.pageCount }}
      </span>
      <button
        class="btn ghost"
        type="button"
        :disabled="pager.page >= pager.pageCount - 1"
        @click="goToPage(pager.page + 1)"
      >
        Next ›
      </button>
    </div>
  </Card>
</template>

<style scoped>
.diag {
  border-bottom: 1px solid var(--line);
  border-left: 3px solid transparent;
  padding: 8px 2px 8px 8px;
}
.diag:last-child {
  border-bottom: none;
}
.diag[data-sev="error"] {
  border-left-color: var(--bad);
}
.diag[data-sev="warning"] {
  border-left-color: var(--warn);
}
.diag[data-sev="info"],
.diag[data-sev="hint"] {
  border-left-color: var(--accent);
}
.diag-head {
  display: flex;
  gap: 8px;
  align-items: baseline;
  flex-wrap: wrap;
}
.diag .loc {
  font-family: var(--mono);
  font-size: 12px;
  color: var(--accent);
  cursor: pointer;
}
.diag .loc:hover {
  text-decoration: underline;
}
.diag .code {
  font-family: var(--mono);
  font-size: 11px;
  color: var(--muted);
}
.diag .msg {
  font-size: 13px;
}
/* Brief flash on the diagnostic a deep link landed on. */
.diag.diag-target {
  background: color-mix(in srgb, var(--accent) 14%, transparent);
  border-radius: 6px;
  transition: background 1s ease;
}
/* Filter button + count badge (same shape as the tab-badge diagnostic
   count) -- the four selects live in the .settings panel it toggles, so the
   row above never wraps. */
.filter-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}
.filter-badge:empty {
  display: none;
}
.filter-badge {
  display: inline-block;
  min-width: 15px;
  padding: 0 5px;
  border-radius: 999px;
  font-size: 10px;
  font-weight: 700;
  line-height: 15px;
  text-align: center;
  background: color-mix(in srgb, var(--accent) 20%, transparent);
  color: var(--accent);
}
/* One filter per row, full width -- a long file path or code name can
   never push the panel wider than its card, unlike a multi-column grid. */
.diag-filter-stack {
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.diag-filter-stack select {
  width: 100%;
  max-width: 100%;
}
.diag-pager {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 12px;
  margin-top: 10px;
  padding-top: 10px;
  border-top: 1px solid var(--line);
}
.diag-pager button.btn.ghost:disabled {
  opacity: 0.35;
  cursor: default;
}
</style>
