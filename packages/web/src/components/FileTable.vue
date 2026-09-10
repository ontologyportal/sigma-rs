<script setup lang="ts">
/** The library file table shared by the Knowledge base and Problems tabs:
 *  search/filter/sort over `rows`, a checkbox multi-select (v-model
 *  `selected`, the row keys) whose ticked rows are pending changes -- an
 *  unloaded row "will {load}", a loaded one "will {unload}" -- and an action
 *  bar that emits `save` with both lists. Loading/importing/removing is the
 *  parent's job; this component only presents and selects. */
import { computed, ref, watch } from "vue";
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
  /** Small tag before the name (Problems: "KIF" / "TPTP"). */
  badge?: string;
  /** Status column text when the row has no pending change. */
  status: string;
  statusKind: "in" | "out" | "locked";
  /** Optional trailing column text (Problems: the last result). */
  extra?: string;
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
  }>(),
  {
    loadedWord: "load",
    unloadedWord: "unload",
    extraHeader: "",
    saving: false,
    catalogNote: "",
    catalogError: false,
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
const sortKey = ref<SortKey>("status");

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

/** Toggle `row`; with Shift held, set the whole range from the last toggled
 *  row (in the current visible order) to the new state. */
function toggle(row: FileRow, e?: MouseEvent) {
  if (row.locked) return;
  const on = !selected.value.has(row.key);
  const next = new Set(selected.value);
  const set = (r: FileRow) => {
    if (r.locked) return;
    if (on) next.add(r.key);
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

function save() {
  if (!pendingAdds.value.length && !pendingRemoves.value.length) return;
  emit("save", pendingAdds.value, pendingRemoves.value);
}
</script>

<template>
  <div class="inline toolbar mt">
    <input
      v-model="search"
      type="search"
      class="search"
      placeholder="filter by name…"
      autocomplete="off"
      aria-label="Filter files by name"
    />
    <select v-model="statusFilter" aria-label="Status filter">
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
      <option value="status">Status</option>
    </select>
  </div>

  <div class="hint mt-sm">
    Tick files to {{ loadedWord }} or {{ unloadedWord }} them, then save.
    Shift-click selects a range.
    <span v-if="catalogNote" :class="{ bad: catalogError }">
      {{ catalogNote }}
    </span>
  </div>

  <div class="table-wrap mt-sm" @keydown.esc="clearSelection">
    <table>
      <thead>
        <tr>
          <th class="col-check"></th>
          <th>Name</th>
          <th class="col-source">Source</th>
          <th class="num">Size</th>
          <th>Status</th>
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
              :checked="selected.has(row.key)"
              :disabled="row.locked"
              :aria-label="`Select ${row.name}`"
              :title="
                row.loaded
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
          </td>
          <td class="hint col-source">{{ row.source }}</td>
          <td class="hint num">{{ formatSize(row.size) }}</td>
          <td class="status-cell">
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
              >{{ row.extra }}</span
            >
          </td>
        </tr>
        <tr v-if="!visible.length">
          <td :colspan="extraHeader ? 6 : 5" class="hint empty">
            No matching files.
          </td>
        </tr>
      </tbody>
    </table>
  </div>

  <div v-if="selectedRows.length" class="action-bar inline between center">
    <span class="inline tight center">
      <span class="pill pending" :title="pendingTip">Unsaved changes</span>
      <span class="hint">
        {{ selectedRows.length }} selected — {{ loadedWord }}
        {{ pendingAdds.length }}, {{ unloadedWord }}
        {{ pendingRemoves.length }}
      </span>
    </span>
    <span class="inline tight">
      <slot name="actions" :adds="pendingAdds" :removes="pendingRemoves" />
      <button
        class="btn ghost"
        type="button"
        :disabled="saving"
        @click="clearSelection"
      >
        Clear
      </button>
      <BusyButton
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
.table-wrap {
  max-height: 480px;
  overflow-y: auto;
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
  top: 0;
  z-index: 1;
  background: var(--card);
  border-bottom: 1px solid var(--line);
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
