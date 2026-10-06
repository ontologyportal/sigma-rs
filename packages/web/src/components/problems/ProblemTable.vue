<script setup lang="ts">
/** The Inference Tests tab's file table: every test in the library (with its
 *  last result once run; a test's text is fetched when it is first run or
 *  opened), and the ones that could not be loaded (with why). Ticked rows
 *  are a selection for "Run selected", which proves them in order.
 *  Emits `log` for the messages the parent shows in its status line. */
import { computed, ref } from "vue";
import { originId, sourceLabel, type Origin } from "../../models/Origin";
import { navigate } from "../../router";
import { useLibraryStore } from "../../stores/library";
import { useTestsStore } from "../../stores/tests";
import { errMsg } from "../../utils/format";
import BusyButton from "../BusyButton.vue";
import FileTable, { type FileRow } from "../FileTable.vue";

const emit = defineEmits<{ log: [text: string, error?: boolean] }>();

const tests = useTestsStore();
const library = useLibraryStore();

const rowKey = (origin: Origin, name: string) => `${originId(origin)}:${name}`;
const badge = (name: string) =>
  tests.dialect(name) === "kif" ? "KIF" : "TPTP";

const failedError = computed(
  () => new Map(tests.failed.map((f) => [rowKey(f.origin, f.name), f.error])),
);

const rows = computed<FileRow[]>(() => [
  ...tests.tests.map((t): FileRow => ({
    key: rowKey(t.origin, t.name),
    name: t.name,
    origin: t.origin,
    source: sourceLabel(t.origin),
    size: t.text.length,
    loaded: true,
    deletable: t.origin.kind !== "sumo",
    badge: badge(t.name),
    status: "",
    statusKind: "in",
    extra: t.outcome?.label ?? "",
    extraKind: t.outcome?.cls ?? "",
  })),
  // Not fetched yet: runnable all the same (running loads it), unless an
  // earlier attempt found it can't be loaded.
  ...tests.available.map((t): FileRow => {
    const key = rowKey(t.origin, t.name);
    const error = failedError.value.get(key);
    return {
      key,
      name: t.name,
      origin: t.origin,
      source: sourceLabel(t.origin),
      size: t.size,
      loaded: !error,
      locked: !!error,
      deletable: t.origin.kind !== "sumo",
      badge: badge(t.name),
      status: "",
      statusKind: error ? "out" : "in",
      extra: error ? "can't load" : "",
      extraKind: error ? "bad" : "",
      extraTip: error,
    };
  }),
]);

const selected = ref(new Set<string>());

async function deleteRow(row: FileRow) {
  if (!window.confirm(`Delete ${row.name} from the library?`)) return;
  try {
    await library.deleteEntry(row.name, row.origin.kind);
    await tests.remove(row.name, originId(row.origin));
    emit("log", `Deleted ${row.name} from the library.`);
  } catch (e) {
    emit("log", errMsg(e), true);
  }
}

/** The runnable tests among `rows`, in table order. */
function testsOf(rows: FileRow[]) {
  return rows.filter((r) => !r.locked);
}

async function runSelected(picked: FileRow[]) {
  const list = testsOf(picked);
  if (!list.length) return;
  try {
    const { pass, ran } = await tests.runAll((t) => {
      emit("log", `Running ${t.name}…`);
    }, list);
    selected.value = new Set();
    emit("log", `${pass}/${ran} passed.`, pass < ran);
  } catch (e) {
    emit("log", errMsg(e), true);
  }
}
</script>

<template>
  <FileTable
    v-model:selected="selected"
    :rows="rows"
    :saving="tests.running"
    select-only
    extra-header="Result"
    :catalog-note="library.catalogNote.text"
    :catalog-error="library.catalogNote.error"
    @delete="deleteRow"
    @open="(row) => navigate('prover', { test: row.name })"
  >
    <template #actions="{ rows: picked }">
      <BusyButton
        ghost
        :busy="tests.running"
        :disabled="!testsOf(picked).length"
        label="Run selected"
        busy-label="Running…"
        title="Prove the ticked tests against the loaded KB"
        @click="runSelected(picked)"
      />
    </template>
  </FileTable>
</template>
