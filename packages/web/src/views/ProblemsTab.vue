<script setup lang="ts">
/** Problems tab: the test-file table (imported `.kif.tq` / `.p` / `.tptp`
 *  tests and every one the library offers) with the Import dialog in its
 *  header. Tests run against the loaded KB instead of joining it. */
import { computed, onActivated, onMounted, ref } from "vue";
import { useLibraryStore } from "../stores/library";
import { isTestFile, useTestsStore } from "../stores/tests";
import { useStatus } from "../composables/useStatus";
import Card from "../components/Card.vue";
import StatusLine from "../components/StatusLine.vue";
import ImportDialog from "../components/kb/ImportDialog.vue";
import ProblemTable from "../components/problems/ProblemTable.vue";

const tests = useTestsStore();
const library = useLibraryStore();

onMounted(() => library.loadCatalogs());
onActivated(() => library.loadCatalogs());

const tableLog = useStatus();
const importOpen = ref(false);

const summary = computed(() => {
  const inLibrary =
    library.entries.filter((e) => isTestFile(e.name)).length +
    Object.values(library.catalogs).reduce(
      (n, c) => n + (c ?? []).filter((e) => isTestFile(e.path)).length,
      0,
    );
  return `${tests.tests.length} imported · ${inLibrary} in library`;
});
</script>

<template>
  <Card title="Problems" :description="summary">
    <template #header>
      <button class="btn ghost" type="button" @click="importOpen = true">
        Import…
      </button>
    </template>
    <ProblemTable @log="(text, error) => tableLog.set(text, error)" />
    <StatusLine class="mt-sm" :text="tableLog.text" :error="tableLog.error" />
  </Card>

  <ImportDialog
    v-model="importOpen"
    accept="test"
    @imported="(msg) => tableLog.set(msg)"
  />
</template>
