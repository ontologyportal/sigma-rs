<script setup lang="ts">
/** The Problems tab's file table: imported tests (with their last result)
 *  and every test file the library offers but has not imported. "Save
 *  changes" imports the ticked available rows and removes the ticked
 *  imported ones; "Run selected" proves the ticked imported tests in order.
 *  Emits `log` for the messages the parent shows in its status line. */
import { computed, ref } from "vue";
import {
  LocalOrigin,
  RemoteOrigin,
  originId,
  sourceLabel,
  type Origin,
} from "../../models/Origin";
import { navigate } from "../../router";
import { fetchAllTexts } from "../../services/sources";
import { useLibraryStore, originForRepo } from "../../stores/library";
import { isTestFile, useTestsStore } from "../../stores/tests";
import { errMsg } from "../../utils/format";
import BusyButton from "../BusyButton.vue";
import FileTable, { type FileRow } from "../FileTable.vue";

const emit = defineEmits<{ log: [text: string, error?: boolean] }>();

const tests = useTestsStore();
const library = useLibraryStore();

const rowKey = (origin: Origin, name: string) => `${origin.kind}:${name}`;
const badge = (name: string) =>
  tests.dialect(name) === "kif" ? "KIF" : "TPTP";

const rows = computed<FileRow[]>(() => {
  const out: FileRow[] = tests.tests.map((t) => ({
    key: rowKey(t.origin, t.name),
    name: t.name,
    origin: t.origin,
    source: sourceLabel(t.origin),
    size: t.text.length,
    loaded: true,
    badge: badge(t.name),
    status: "imported",
    statusKind: "in",
    extra: t.outcome?.label ?? "",
    extraKind: t.outcome?.cls ?? "",
  }));
  const imported = new Set(out.map((r) => r.key));
  for (const e of library.entries) {
    if (!isTestFile(e.name)) continue;
    const origin =
      e.kind === "url" ? new RemoteOrigin(e.url) : new LocalOrigin();
    if (imported.has(rowKey(origin, e.name))) continue;
    out.push({
      key: rowKey(origin, e.name),
      name: e.name,
      origin,
      source: sourceLabel(origin),
      size: e.size,
      loaded: false,
      deletable: true,
      badge: badge(e.name),
      status: "available",
      statusKind: "out",
    });
  }
  for (const repo of library.repos) {
    const origin = originForRepo(repo);
    const source = sourceLabel(origin);
    for (const e of library.catalogs[library.repoId(repo)] ?? []) {
      if (!isTestFile(e.path)) continue;
      const name = origin.nameFor(e.path);
      if (imported.has(rowKey(origin, name))) continue;
      out.push({
        key: rowKey(origin, name),
        name,
        origin,
        source,
        size: e.size,
        loaded: false,
        badge: badge(name),
        status: "available",
        statusKind: "out",
      });
    }
  }
  return out;
});

const selected = ref(new Set<string>());
const saving = ref(false);
const running = ref(false);

async function deleteRow(row: FileRow) {
  if (!window.confirm(`Delete ${row.name} from the library?`)) return;
  try {
    await library.deleteEntry(row.name, row.origin.kind);
    emit("log", `Deleted ${row.name} from the library.`);
  } catch (e) {
    emit("log", errMsg(e), true);
  }
}

async function save(adds: FileRow[], removes: FileRow[]) {
  saving.value = true;
  try {
    let added = 0;
    const failed: string[] = [];
    const texts = adds.length
      ? await fetchAllTexts(adds, 6, (n) => {
          emit("log", `Fetching ${n}/${adds.length}…`);
        })
      : [];
    for (let i = 0; i < adds.length; i++) {
      const row = adds[i];
      const text = texts[i];
      if (text instanceof Error) {
        failed.push(`${row.name}: ${text.message}`);
        continue;
      }
      try {
        const r = await tests.add(row.name, text, row.origin);
        if (r.added) added += 1;
        else failed.push(...r.notices);
      } catch (e) {
        failed.push(`${row.name}: ${errMsg(e)}`);
      }
    }
    for (const row of removes)
      await tests.remove(row.name, originId(row.origin));
    selected.value = new Set();
    emit(
      "log",
      `Imported ${added}, removed ${removes.length}` +
        (failed.length ? ` (${failed.length} failed — ${failed[0]})` : "."),
      failed.length > 0,
    );
  } catch (e) {
    emit("log", errMsg(e), true);
  } finally {
    saving.value = false;
  }
}

/** The imported tests among `rows`, in table order. */
function importedTests(rows: FileRow[]) {
  return rows.flatMap((r) => {
    const t = tests.find(r.name);
    return t && originId(t.origin) === originId(r.origin) ? [t] : [];
  });
}

async function runSelected(removes: FileRow[]) {
  const list = importedTests(removes);
  if (!list.length) return;
  running.value = true;
  try {
    const { pass, ran } = await tests.runAll((t) => {
      emit("log", `Running ${t.name}…`);
    }, list);
    selected.value = new Set();
    emit("log", `${pass}/${ran} passed.`, pass < ran);
  } catch (e) {
    emit("log", errMsg(e), true);
  } finally {
    running.value = false;
  }
}
</script>

<template>
  <FileTable
    v-model:selected="selected"
    :rows="rows"
    :saving="saving || running"
    loaded-word="import"
    unloaded-word="remove"
    extra-header="Result"
    :catalog-note="library.catalogNote.text"
    :catalog-error="library.catalogNote.error"
    @save="save"
    @delete="deleteRow"
    @open="(row) => navigate('prover', { test: row.name })"
  >
    <template #actions="{ removes }">
      <BusyButton
        ghost
        :busy="running"
        :disabled="saving || !importedTests(removes).length"
        label="Run selected"
        busy-label="Running…"
        title="Prove the ticked imported tests against the loaded KB"
        @click="runSelected(removes)"
      />
    </template>
  </FileTable>
</template>
