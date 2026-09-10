<script setup lang="ts">
/** One unified list of every file in the library -- loaded (KB constituents
 *  and imported tests) and available (unloaded library entries and repo
 *  catalog files) -- with search/filter/sort and a checkbox multi-select
 *  whose "Save changes" loads the unloaded rows and unloads the loaded ones
 *  in ONE batch (one post-processing pass). Unloading keeps a file in the
 *  library; local/URL entries have a separate "delete" action.
 *  Emits `log` for the messages the parent shows in its status line. */
import { computed, ref, watch } from "vue";
import { MERGE } from "../../constants";
import {
  GitOrigin,
  LocalOrigin,
  RemoteOrigin,
  originForKind,
  type Origin,
  type OriginKind,
} from "../../models/Origin";
import { navigate } from "../../router";
import { fetchAllTexts } from "../../services/sources";
import { useKBStore } from "../../stores/kb";
import { useLibraryStore, originForRepo } from "../../stores/library";
import { useTestsStore } from "../../stores/tests";
import { errMsg, formatSize } from "../../utils/format";
import BusyButton from "../BusyButton.vue";

/** One table row: a loaded constituent/test or an available library file. */
export interface Row {
  /** `kind:name` -- what the selection set holds. */
  key: string;
  name: string;
  kind: OriginKind;
  origin: Origin;
  /** Source column text: `GitHub` (upstream), `owner/repo@branch`, `Local`, `URL`. */
  source: string;
  /** Text length when loaded, the library/upstream size otherwise. */
  size: number;
  loaded: boolean;
  /** The foundational file (Merge.kif): shown, never selectable. */
  core: boolean;
  /** A `.kif.tq` test -- imported to the tests store, not the KB. */
  test: boolean;
  /** A local/URL library entry that is not loaded: can be deleted. */
  deletable: boolean;
}

const emit = defineEmits<{ log: [text: string, error?: boolean] }>();

const kb = useKBStore();
const tests = useTestsStore();
const library = useLibraryStore();

const isTest = (name: string) => /\.tq$/i.test(name);
const rowKey = (kind: OriginKind, name: string) => `${kind}:${name}`;

function sourceLabel(origin: Origin): string {
  switch (origin.kind) {
    case "sumo":
      return (origin as GitOrigin).isDefault
        ? "GitHub"
        : (origin as GitOrigin).label;
    case "file":
      return "Local";
    case "url":
      return "URL";
  }
}

const rows = computed<Row[]>(() => {
  const out: Row[] = kb.constituents.map((c) => ({
    key: rowKey(c.origin.kind, c.name),
    name: c.name,
    kind: c.origin.kind,
    origin: c.origin,
    source: sourceLabel(c.origin),
    size: c.text.length,
    loaded: true,
    core: c.name === MERGE,
    test: isTest(c.name),
    deletable: false,
  }));
  for (const t of tests.tests) {
    const origin = originForKind(t.origin);
    out.push({
      key: rowKey(t.origin, t.name),
      name: t.name,
      kind: t.origin,
      origin,
      source: sourceLabel(origin),
      size: t.text.length,
      loaded: true,
      core: false,
      test: true,
      deletable: false,
    });
  }
  const loaded = new Set(out.map((r) => r.key));
  for (const e of library.entries) {
    if (loaded.has(rowKey(e.kind, e.name))) continue;
    const origin =
      e.kind === "url" ? new RemoteOrigin(e.url) : new LocalOrigin();
    out.push({
      key: rowKey(e.kind, e.name),
      name: e.name,
      kind: e.kind,
      origin,
      source: sourceLabel(origin),
      size: e.size,
      loaded: false,
      core: false,
      test: false,
      deletable: true,
    });
  }
  for (const repo of library.repos) {
    const origin = originForRepo(repo);
    const source = sourceLabel(origin);
    for (const e of library.catalogs[library.repoId(repo)] ?? []) {
      const name = origin.nameFor(e.path);
      if (loaded.has(rowKey("sumo", name))) continue;
      out.push({
        key: rowKey("sumo", name),
        name,
        kind: "sumo",
        origin,
        source,
        size: e.size,
        loaded: false,
        core: false,
        test: isTest(e.path),
        deletable: false,
      });
    }
  }
  return out;
});

// -- Toolbar ------------------------------------------------------------------

type StatusFilter = "all" | "loaded" | "available";
type SourceFilter = "all" | "github" | "repos" | "file" | "url";
type SortKey = "name" | "size" | "source" | "status";

const search = ref("");
const statusFilter = ref<StatusFilter>("all");
const sourceFilter = ref<SourceFilter>("all");
const sortKey = ref<SortKey>("status");
const showTests = ref(false);

const statusRank = (r: Row) => (r.core ? 0 : r.loaded ? 1 : 2);
const statusLabel = (r: Row) =>
  r.core ? "core" : r.loaded ? "loaded" : "available";

function matchesSource(r: Row): boolean {
  switch (sourceFilter.value) {
    case "all":
      return true;
    case "github":
      return r.kind === "sumo" && (r.origin as GitOrigin).isDefault;
    case "repos":
      return r.kind === "sumo" && !(r.origin as GitOrigin).isDefault;
    default:
      return r.kind === sourceFilter.value;
  }
}

const visible = computed<Row[]>(() => {
  const needle = search.value.trim().toLowerCase();
  const list = rows.value.filter((r) => {
    if (r.test && !showTests.value) return false;
    if (needle && !r.name.toLowerCase().includes(needle)) return false;
    if (statusFilter.value === "loaded" && !r.loaded) return false;
    if (statusFilter.value === "available" && r.loaded) return false;
    return matchesSource(r);
  });
  const byName = (a: Row, b: Row) => a.name.localeCompare(b.name);
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

const catalogErrors = computed(() =>
  library.repos
    .map((r) => {
      const err = library.catalogErrors[library.repoId(r)];
      return err ? `${originForRepo(r).label}: ${err}` : "";
    })
    .filter(Boolean),
);
const catalogNote = computed(() => {
  if (catalogErrors.value.length)
    return `could not load file list — ${catalogErrors.value.join("; ")}`;
  const pending = library.repos.some(
    (r) => !library.catalogs[library.repoId(r)],
  );
  return pending ? "loading file lists…" : "";
});

// -- Selection ----------------------------------------------------------------

const selected = ref(new Set<string>());
const lastToggled = ref<string | null>(null);

// Prune keys whose rows disappeared (a removed constituent, a catalog reload).
watch(rows, (list) => {
  const keys = new Set(list.map((r) => r.key));
  for (const k of [...selected.value])
    if (!keys.has(k)) selected.value.delete(k);
});

const selectedRows = computed(() =>
  rows.value.filter((r) => selected.value.has(r.key)),
);
const pendingAdds = computed(() => selectedRows.value.filter((r) => !r.loaded));
const pendingRemoves = computed(() =>
  selectedRows.value.filter((r) => r.loaded),
);

function setSelected(row: Row, on: boolean) {
  if (row.core) return;
  if (on) selected.value.add(row.key);
  else selected.value.delete(row.key);
}

/** Toggle `row`; with Shift held, set the whole range from the last toggled
 *  row (in the current visible order) to the new state. */
function toggle(row: Row, e?: MouseEvent) {
  if (row.core) return;
  const on = !selected.value.has(row.key);
  const list = visible.value;
  const from = lastToggled.value
    ? list.findIndex((r) => r.key === lastToggled.value)
    : -1;
  const to = list.findIndex((r) => r.key === row.key);
  if (e?.shiftKey && from !== -1 && to !== -1) {
    const [lo, hi] = from < to ? [from, to] : [to, from];
    for (let i = lo; i <= hi; i++) setSelected(list[i], on);
  } else {
    setSelected(row, on);
  }
  lastToggled.value = row.key;
}

function onRowClick(row: Row, e: MouseEvent) {
  const target = e.target as HTMLElement;
  if (target.closest("a, input")) return;
  toggle(row, e);
}

function clearSelection() {
  selected.value.clear();
  lastToggled.value = null;
}

// -- Delete -------------------------------------------------------------------

async function deleteRow(row: Row) {
  if (!row.deletable) return;
  if (!window.confirm(`Delete ${row.name} from the library?`)) return;
  try {
    await library.deleteEntry(row.name, row.kind);
    selected.value.delete(row.key);
    emit("log", `Deleted ${row.name} from the library.`);
  } catch (e) {
    emit("log", errMsg(e), true);
  }
}

// -- Save ---------------------------------------------------------------------

const saving = ref(false);

async function save() {
  const adds = pendingAdds.value;
  const removes = pendingRemoves.value;
  if (!adds.length && !removes.length) return;
  saving.value = true;
  try {
    let added = 0;
    let removed = 0;
    const failed: string[] = [];

    const texts = adds.length
      ? await fetchAllTexts(adds, 6, (n) => {
          emit("log", `Fetching ${n}/${adds.length}…`);
        })
      : [];
    const kbAdds: { name: string; text: string; origin: Origin }[] = [];
    for (let i = 0; i < adds.length; i++) {
      const row = adds[i];
      const text = texts[i];
      if (text instanceof Error) {
        failed.push(`${row.name}: ${text.message}`);
        continue;
      }
      if (!row.test) {
        kbAdds.push({ name: row.name, text, origin: row.origin });
        continue;
      }
      try {
        const r = await tests.add(row.name, text, row.kind);
        if (r.added) added += 1;
        else failed.push(...r.notices);
      } catch (e) {
        failed.push(`${row.name}: ${errMsg(e)}`);
      }
    }

    for (const row of removes.filter((r) => r.test)) {
      await tests.remove(row.name, row.kind);
      removed += 1;
    }

    const kbRemoves = removes
      .filter((r) => !r.test)
      .map((r) => ({ name: r.name, kind: r.kind }));
    if (kbAdds.length || kbRemoves.length) {
      emit(
        "log",
        `Loading ${kbAdds.length}, unloading ${kbRemoves.length}; axiomatizing…`,
      );
      const r = await kb.applyChanges({ add: kbAdds, remove: kbRemoves });
      added += r.added;
      removed += r.removed;
      failed.push(...r.failed);
    }

    clearSelection();
    emit(
      "log",
      `Added ${added}, removed ${removed}` +
        (failed.length ? ` (${failed.length} failed — ${failed[0]})` : "."),
      failed.length > 0,
    );
  } catch (e) {
    emit("log", errMsg(e), true);
  } finally {
    saving.value = false;
  }
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
      aria-label="Filter constituents by name"
    />
    <select v-model="statusFilter" aria-label="Status filter">
      <option value="all">All</option>
      <option value="loaded">Loaded</option>
      <option value="available">Available</option>
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
    <label class="check">
      <input v-model="showTests" type="checkbox" />
      Show tests
    </label>
  </div>

  <div class="hint mt-sm">
    Tick files to load or unload them, then save. Shift-click selects a range.
    <span v-if="catalogNote" :class="{ bad: catalogErrors.length > 0 }">
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
        </tr>
      </thead>
      <tbody>
        <tr
          v-for="row in visible"
          :key="row.key"
          :class="{ selected: selected.has(row.key), core: row.core }"
          @click="onRowClick(row, $event)"
        >
          <td class="col-check">
            <input
              type="checkbox"
              :checked="selected.has(row.key)"
              :disabled="row.core"
              :aria-label="`Select ${row.name}`"
              @click.stop="toggle(row, $event)"
            />
          </td>
          <td>
            <a
              v-if="row.loaded && !row.test"
              class="mono name"
              title="Open in the editor"
              @click="navigate('edit', { file: row.name })"
              >{{ row.name }}</a
            >
            <span v-else class="mono name">{{ row.name }}</span>
          </td>
          <td class="hint col-source">{{ row.source }}</td>
          <td class="hint num">{{ formatSize(row.size) }}</td>
          <td class="status-cell">
            <span v-if="row.loaded && !row.core" class="pill">loaded</span>
            <span v-else class="hint">{{ statusLabel(row) }}</span>
            <a
              v-if="row.deletable"
              class="hint delete"
              title="Remove from the library"
              @click="deleteRow(row)"
              >delete</a
            >
          </td>
        </tr>
        <tr v-if="!visible.length">
          <td colspan="5" class="hint empty">No matching files.</td>
        </tr>
      </tbody>
    </table>
  </div>

  <div v-if="selectedRows.length" class="action-bar inline between center">
    <span class="hint">
      {{ selectedRows.length }} selected — load {{ pendingAdds.length }}, unload
      {{ pendingRemoves.length }}
    </span>
    <span class="inline tight">
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
tbody tr.core {
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
