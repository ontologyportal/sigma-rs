<script setup lang="ts">
/** The library file table shared by the Knowledge base and Inference Tests tabs:
 *  search/filter/sort over `rows`, a checkbox multi-select (v-model
 *  `selected`, the row keys) whose ticked rows are pending changes -- an
 *  unloaded row "will {load}", a loaded one "will {unload}" -- and an action
 *  bar that emits `save` with both lists. Loading/importing/removing is the
 *  parent's job; this component only presents and selects. With
 *  `selectOnly` there is no load state: no Status column, filter or Save,
 *  and the ticked rows are a plain selection for the `actions` slot. */
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import type { GitOrigin, Origin } from "../models/Origin";
import { formatSize } from "../utils/format";
import BusyButton from "./BusyButton.vue";

export interface FileRow {
  /** What the selection set holds -- unique across the table. */
  key: string;
  name: string;
  origin: Origin;
  /** Source column text: `GitHub` (upstream), `owner/repo@branch`, `Local`, `URL`. */
  source: string;
  size: number;
  /** "In": a loaded constituent / an imported test. */
  loaded: boolean;
  /** Shown, never selectable (the core file). */
  locked?: boolean;
  /** A local/URL library entry that is not loaded: can be deleted. */
  deletable?: boolean;
  /** Small tag before the name (Inference Tests: "KIF" / "TPTP"). */
  badge?: string;
  /** A warning pill after the status, explained by its hover tooltip. */
  flag?: { label: string; tip: string };
  /** Status column text when the row has no pending change. */
  status: string;
  statusKind: "in" | "out" | "locked";
  /** Optional trailing column text (Inference Tests: the last result). */
  extra?: string;
  /** Hover text for `extra`. */
  extraTip?: string;
  extraKind?: "ok" | "bad" | "mut" | "";
}

const props = withDefaults(
  defineProps<{
    rows: FileRow[];
    /** The verb for taking a row "in" (pending badge, hint, filter). */
    loadedWord?: string;
    /** The verb for taking a row "out". */
    unloadedWord?: string;
    /** Shows the extra column, with this header. */
    extraHeader?: string;
    saving?: boolean;
    /** "loading upstream file list..." or an error, shown after the hint. */
    catalogNote?: string;
    catalogError?: boolean;
    /** Checkboxes show the state a save would leave (loaded rows ticked;
     *  untick to take one out) instead of marking the rows to change. */
    checkedIsLoaded?: boolean;
    /** Selection only: hide the load state and the Save action. */
    selectOnly?: boolean;
  }>(),
  {
    loadedWord: "load",
    unloadedWord: "unload",
    extraHeader: "",
    saving: false,
    catalogNote: "",
    catalogError: false,
    checkedIsLoaded: false,
    selectOnly: false,
  },
);

const emit = defineEmits<{
  save: [adds: FileRow[], removes: FileRow[]];
  delete: [row: FileRow];
  open: [row: FileRow];
}>();

const selected = defineModel<Set<string>>("selected", {
  default: () => new Set<string>(),
});

// -- Toolbar ------------------------------------------------------------------

type StatusFilter = "all" | "in" | "out";
type SourceFilter = "all" | "github" | "repos" | "file" | "url";
type SortKey = "name" | "size" | "source" | "status";

const search = ref("");
const statusFilter = ref<StatusFilter>("all");
const sourceFilter = ref<SourceFilter>("all");
const sortKey = ref<SortKey>(props.selectOnly ? "name" : "status");

const capitalize = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);
const inLabel = computed(() => capitalize(`${props.loadedWord}ed`));

const statusRank = (r: FileRow) =>
  r.statusKind === "locked" ? 0 : r.statusKind === "in" ? 1 : 2;

function matchesSource(r: FileRow): boolean {
  switch (sourceFilter.value) {
    case "all":
      return true;
    case "github":
      return r.origin.kind === "sumo" && (r.origin as GitOrigin).isDefault;
    case "repos":
      return r.origin.kind === "sumo" && !(r.origin as GitOrigin).isDefault;
    default:
      return r.origin.kind === sourceFilter.value;
  }
}

const visible = computed<FileRow[]>(() => {
  const needle = search.value.trim().toLowerCase();
  const list = props.rows.filter((r) => {
    if (needle && !r.name.toLowerCase().includes(needle)) return false;
    if (statusFilter.value === "in" && !r.loaded) return false;
    if (statusFilter.value === "out" && r.loaded) return false;
    return matchesSource(r);
  });
  const byName = (a: FileRow, b: FileRow) => a.name.localeCompare(b.name);
  switch (sortKey.value) {
    case "size":
      return list.sort((a, b) => b.size - a.size || byName(a, b));
    case "source":
      return list.sort(
        (a, b) => a.source.localeCompare(b.source) || byName(a, b),
      );
    case "status":
      return list.sort((a, b) => statusRank(a) - statusRank(b) || byName(a, b));
    default:
      return list.sort(byName);
  }
});

// The table grows with the page; the toolbar sticks to the top of the
// window and the column headers stick just below it.
const bar = ref<HTMLElement | null>(null);
const barHeight = ref(0);
const headTop = computed(() => `${barHeight.value}px`);
let barObserver: ResizeObserver | null = null;
onMounted(() => {
  if (!bar.value) return;
  barObserver = new ResizeObserver(([entry]) => {
    barHeight.value = entry.target.getBoundingClientRect().height;
  });
  barObserver.observe(bar.value);
});
onBeforeUnmount(() => barObserver?.disconnect());

// -- Selection ----------------------------------------------------------------

const lastToggled = ref<string | null>(null);

// Prune keys whose rows disappeared (a removed file, a catalog reload).
watch(
  () => props.rows,
  (list) => {
    const keys = new Set(list.map((r) => r.key));
    const kept = [...selected.value].filter((k) => keys.has(k));
    if (kept.length !== selected.value.size) selected.value = new Set(kept);
  },
);

const selectedRows = computed(() =>
  props.rows.filter((r) => selected.value.has(r.key)),
);
const pendingAdds = computed(() => selectedRows.value.filter((r) => !r.loaded));
const pendingRemoves = computed(() =>
  selectedRows.value.filter((r) => r.loaded),
);

const pendingTip = computed(
  () =>
    `Unsaved change: nothing is ${props.loadedWord}ed or ${props.unloadedWord}ed until you click Save changes.`,
);

/** Whether `row`'s checkbox is ticked. `selected` always holds the rows
 *  with a pending change; with `checkedIsLoaded` the box shows the state
 *  after saving (loaded, flipped by a pending change) rather than the
 *  pending mark itself. */
const baseChecked = (r: FileRow) => props.checkedIsLoaded && r.loaded;
const isChecked = (r: FileRow) => baseChecked(r) !== selected.value.has(r.key);

/** Toggle `row`'s checkbox; with Shift held, set every box in the range from
 *  the last toggled row (in the current visible order) to the same state. */
function toggle(row: FileRow, e?: MouseEvent) {
  if (row.locked) return;
  const want = !isChecked(row);
  const next = new Set(selected.value);
  const set = (r: FileRow) => {
    if (r.locked) return;
    if (want !== baseChecked(r)) next.add(r.key);
    else next.delete(r.key);
  };
  const list = visible.value;
  const from = lastToggled.value
    ? list.findIndex((r) => r.key === lastToggled.value)
    : -1;
  const to = list.findIndex((r) => r.key === row.key);
  if (e?.shiftKey && from !== -1 && to !== -1) {
    const [lo, hi] = from < to ? [from, to] : [to, from];
    for (let i = lo; i <= hi; i++) set(list[i]);
  } else {
    set(row);
  }
  selected.value = next;
  lastToggled.value = row.key;
}

function onRowClick(row: FileRow, e: MouseEvent) {
  const target = e.target as HTMLElement;
  if (target.closest("a, input")) return;
  toggle(row, e);
}

function clearSelection() {
  selected.value = new Set();
  lastToggled.value = null;
}

/** Table width in columns, for the empty-state row. */
const columns = computed(
  () => 4 + (props.selectOnly ? 0 : 1) + (props.extraHeader ? 1 : 0),
);

function save() {
  if (!pendingAdds.value.length && !pendingRemoves.value.length) return;
  emit("save", pendingAdds.value, pendingRemoves.value);
}
</script>

<template>
  <div ref="bar" class="table-bar">
    <div class="inline toolbar">
      <input
        v-model="search"
        type="search"
        class="search"
        placeholder="filter by name…"
        autocomplete="off"
        aria-label="Filter files by name"
      />
      <select
        v-if="!selectOnly"
        v-model="statusFilter"
        aria-label="Status filter"
      >
        <option value="all">All</option>
        <option value="in">{{ inLabel }}</option>
        <option value="out">Available</option>
      </select>
      <select v-model="sourceFilter" aria-label="Source filter">
        <option value="all">All sources</option>
        <option value="github">GitHub</option>
        <option value="repos">Other repos</option>
        <option value="file">Local</option>
        <option value="url">URL</option>
      </select>
      <select v-model="sortKey" aria-label="Sort by">
        <option value="name">Name</option>
        <option value="size">Size</option>
        <option value="source">Source</option>
        <option v-if="!selectOnly" value="status">Status</option>
      </select>
    </div>

    <div class="hint mt-sm">
      <template v-if="selectOnly">Tick rows to select them.</template>
      <template v-else-if="checkedIsLoaded"
        >Tick files to {{ loadedWord }} them and untick {{ loadedWord }}ed ones
        to {{ unloadedWord }} them, then save.</template
      >
      <template v-else
        >Tick files to {{ loadedWord }} or {{ unloadedWord }} them, then
        save.</template
      >
      Shift-click sets a range.
      <span v-if="catalogNote" :class="{ bad: catalogError }">
        {{ catalogNote }}
      </span>
    </div>
  </div>

  <div class="table-wrap" @keydown.esc="clearSelection">
    <table>
      <thead>
        <tr>
          <th class="col-check"></th>
          <th>Name</th>
          <th class="col-source">Source</th>
          <th class="num">Size</th>
          <th v-if="!selectOnly">Status</th>
          <th v-if="extraHeader" class="col-extra">{{ extraHeader }}</th>
        </tr>
      </thead>
      <tbody>
        <tr
          v-for="row in visible"
          :key="row.key"
          :class="{ selected: selected.has(row.key), locked: row.locked }"
          @click="onRowClick(row, $event)"
        >
          <td class="col-check">
            <input
              type="checkbox"
              :checked="isChecked(row)"
              :disabled="row.locked"
              :aria-label="
                checkedIsLoaded
                  ? `${capitalize(loadedWord)} ${row.name}`
                  : `Select ${row.name}`
              "
              :title="
                selectOnly
                  ? undefined
                  : checkedIsLoaded
                    ? row.loaded
                      ? `Untick to ${unloadedWord} on save`
                      : `Tick to ${loadedWord} on save`
                    : row.loaded
                      ? `Tick to ${unloadedWord} on save`
                      : `Tick to ${loadedWord} on save`
              "
              @click.stop="toggle(row, $event)"
            />
          </td>
          <td>
            <span v-if="row.badge" class="badge">{{ row.badge }}</span>
            <a
              v-if="row.loaded"
              class="mono name"
              title="Open"
              @click="emit('open', row)"
              >{{ row.name }}</a
            >
            <span v-else class="mono name">{{ row.name }}</span>
            <a
              v-if="selectOnly && row.deletable"
              class="hint delete"
              title="Delete from the library"
              @click="emit('delete', row)"
              >delete</a
            >
          </td>
          <td class="hint col-source">{{ row.source }}</td>
          <td class="hint num">{{ formatSize(row.size) }}</td>
          <td v-if="!selectOnly" class="status-cell">
            <span
              v-if="selected.has(row.key)"
              class="pill pending"
              :class="{ unload: row.loaded }"
              :title="pendingTip"
              >will {{ row.loaded ? unloadedWord : loadedWord }}</span
            >
            <span v-else-if="row.statusKind === 'in'" class="pill">{{
              row.status
            }}</span>
            <span v-else class="hint">{{ row.status }}</span>
            <span
              v-if="row.flag"
              class="pill flag"
              tabindex="0"
              :title="row.flag.tip"
              :aria-label="`${row.flag.label}: ${row.flag.tip}`"
              >{{ row.flag.label }}</span
            >
            <a
              v-if="row.deletable"
              class="hint delete"
              title="Remove from the library"
              @click="emit('delete', row)"
              >delete</a
            >
          </td>
          <td v-if="extraHeader" class="col-extra">
            <span
              v-if="row.extra"
              class="result"
              :class="row.extraKind || ''"
              :title="row.extraTip"
              >{{ row.extra }}</span
            >
          </td>
        </tr>
        <tr v-if="!visible.length">
          <td :colspan="columns" class="hint empty">No matching files.</td>
        </tr>
      </tbody>
    </table>
  </div>

  <div v-if="selectedRows.length" class="action-bar inline between center">
    <span class="inline tight center">
      <span v-if="!selectOnly" class="pill pending" :title="pendingTip"
        >Unsaved changes</span
      >
      <span v-if="selectOnly" class="hint"
        >{{ selectedRows.length }} selected</span
      >
      <span v-else class="hint">
        {{ selectedRows.length }}
        {{
          checkedIsLoaded
            ? selectedRows.length === 1
              ? "change"
              : "changes"
            : "selected"
        }}
        — {{ loadedWord }} {{ pendingAdds.length }}, {{ unloadedWord }}
        {{ pendingRemoves.length }}
      </span>
    </span>
    <span class="inline tight">
      <slot
        name="actions"
        :adds="pendingAdds"
        :removes="pendingRemoves"
        :rows="selectedRows"
      />
      <button
        class="btn ghost"
        type="button"
        :disabled="saving"
        @click="clearSelection"
      >
        Clear
      </button>
      <BusyButton
        v-if="!selectOnly"
        :busy="saving"
        label="Save changes"
        busy-label="Saving…"
        @click="save"
      />
    </span>
  </div>
</template>

<style scoped>
.toolbar {
  align-items: center;
}
.search {
  flex: 1 1 200px;
  width: auto;
  min-width: 0;
}
.table-bar {
  position: sticky;
  top: 0;
  z-index: 2;
  margin-top: 12px;
  padding: 8px 0;
  background: var(--card);
}
.table-wrap {
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--bg);
}
table {
  width: 100%;
  border-collapse: collapse;
  font-size: 13px;
}
thead th {
  position: sticky;
  top: v-bind(headTop);
  z-index: 1;
  background: var(--card);
  /* A collapsed border does not travel with a sticky cell; the shadow does. */
  box-shadow: inset 0 -1px 0 var(--line);
  text-align: left;
  font-weight: 600;
  font-size: 12px;
  color: var(--muted);
  padding: 8px 10px;
}
td {
  padding: 7px 10px;
  border-bottom: 1px solid var(--line);
  vertical-align: middle;
}
tbody tr:last-child td {
  border-bottom: none;
}
tbody tr {
  cursor: pointer;
}
tbody tr.locked {
  cursor: default;
}
tbody tr:hover {
  background: color-mix(in srgb, var(--fg) 4%, transparent);
}
tbody tr.selected {
  background: color-mix(in srgb, var(--accent) 10%, transparent);
}
.col-check {
  width: 32px;
  text-align: center;
}
.col-check input {
  margin: 0;
  width: auto;
  padding: 0;
}
.badge {
  display: inline-block;
  margin-right: 6px;
  padding: 0 5px;
  border-radius: 4px;
  font-size: 10px;
  font-weight: 700;
  letter-spacing: 0.04em;
  background: color-mix(in srgb, var(--muted) 18%, transparent);
  color: var(--muted);
}
.name {
  font-weight: 600;
  word-break: break-all;
}
.num {
  text-align: right;
  white-space: nowrap;
}
.empty {
  text-align: center;
}
.status-cell {
  white-space: nowrap;
}
.delete {
  margin-left: 10px;
  font-size: 12px;
}
.delete:hover {
  color: var(--bad);
}
.pill {
  padding: 2px 9px;
  border-radius: 20px;
  font-weight: 600;
  font-size: 12px;
  background: color-mix(in srgb, var(--accent) 18%, transparent);
  color: var(--accent);
}
/* A row's warning (a stale or unavailable constituent). */
.pill.flag {
  margin-left: 6px;
  background: color-mix(in srgb, var(--warn) 16%, transparent);
  color: var(--warn);
  cursor: help;
}
/* A ticked row's pending change: accent for taking in, amber for taking out. */
.pill.pending {
  background: color-mix(in srgb, var(--accent) 12%, transparent);
  border: 1px dashed var(--accent);
  cursor: help;
}
.pill.pending.unload {
  background: color-mix(in srgb, var(--warn) 14%, transparent);
  border-color: var(--warn);
  color: var(--warn);
}
/* The extra column's badge (a test outcome): pass / fail / inconclusive. */
.result {
  font-size: 11px;
  padding: 1px 7px;
  border-radius: 9px;
  font-weight: 600;
  white-space: nowrap;
  background: color-mix(in srgb, var(--muted) 18%, transparent);
  color: var(--muted);
}
.result.ok {
  background: color-mix(in srgb, var(--ok) 18%, transparent);
  color: var(--ok);
}
.result.bad {
  background: color-mix(in srgb, var(--bad) 18%, transparent);
  color: var(--bad);
}
.action-bar {
  position: sticky;
  bottom: 0;
  margin-top: 10px;
  padding: 8px 0 0;
  border-top: 1px solid var(--line);
  background: var(--card);
}
/* Phones: the source column is the least useful one, drop it for room. */
@media (max-width: 520px) {
  .col-source {
    display: none;
  }
}
</style>
