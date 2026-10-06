<script setup lang="ts">
/** Inference Tests tab: the test-file table (every `.kif.tq` / `.p` / `.tptp`
 *  test the library offers) with the Import dialog in its header, and the
 *  `test` prover profile that bulk runs use -- a column on the right in the
 *  classic layout, a toggled panel in the comfortable one. Tests run against
 *  the loaded KB instead of joining it. */
import { computed, onActivated, onMounted, ref, watch } from "vue";
import { useLibraryStore } from "../stores/library";
import { useShellStore } from "../stores/shell";
import { useTestsStore } from "../stores/tests";
import { useStatus } from "../composables/useStatus";
import Card from "../components/Card.vue";
import StatusLine from "../components/StatusLine.vue";
import ImportDialog from "../components/kb/ImportDialog.vue";
import ProblemTable from "../components/problems/ProblemTable.vue";
import ProverOptions from "../components/ProverOptions.vue";
import ProverSideStack from "../components/ProverSideStack.vue";
import ProofHistory from "../components/ProofHistory.vue";
import HistoryToggle from "../components/HistoryToggle.vue";
import ProverChips from "../components/ProverChips.vue";
import { navigate } from "../router";
import type { ProofRun } from "../stores/runHistory";
import Row from "../components/Row.vue";
import Col from "../components/Col.vue";

const tests = useTestsStore();
const library = useLibraryStore();
const shell = useShellStore();
const isCompact = computed(() => shell.isCompact);
const optionsOpen = ref(false);
/** Comfortable layout: the run history toggles open in the card. */
const historyOpen = ref(false);

/** A history entry opens its test in Ask/Tell. */
function openRun(run: ProofRun) {
  if (run.title) navigate("prover", { test: run.title });
}

onMounted(() => library.loadCatalogs());
onActivated(() => library.loadCatalogs());
// Accepting an update moves a pinned source's commit: list its files anew.
watch(
  () => library.repos.map((r) => library.catalogRef(r)).join("|"),
  () => library.loadCatalogs(),
);

const timeNote =
  "Used by Run selected. A test's own (time N) overrides the time limit.";

const tableLog = useStatus();
const importOpen = ref(false);

const summary = computed(() => {
  const failed = tests.failed.length;
  const n = tests.tests.length + tests.available.length - failed;
  return (
    `${n} test${n === 1 ? "" : "s"}` +
    (failed ? ` · ${failed} can't be loaded` : "")
  );
});
</script>

<template>
  <Row>
    <Col :span="isCompact ? 12 : 8">
      <Card title="Inference Tests" :description="summary">
        <template #header>
          <div class="inline tight">
            <ProverChips
              v-if="isCompact"
              v-model:open="optionsOpen"
              profile="test"
              :disabled="tests.running"
            />
            <HistoryToggle
              v-if="isCompact"
              v-model:open="historyOpen"
              title="Earlier test runs"
            />
            <button class="btn ghost" type="button" @click="importOpen = true">
              Import…
            </button>
          </div>
        </template>
        <div v-if="isCompact && optionsOpen" class="options-panel">
          <ProverOptions profile="test" :disabled="tests.running" />
          <p class="hint">{{ timeNote }}</p>
        </div>
        <ProofHistory
          v-if="isCompact && historyOpen"
          class="options-panel"
          feed="test"
          @pick="openRun"
        />
        <ProblemTable @log="(text, error) => tableLog.set(text, error)" />
        <StatusLine
          class="mt-sm"
          :text="tableLog.text"
          :error="tableLog.error"
        />
      </Card>
    </Col>
    <Col v-if="!isCompact" :span="4" style="margin-left: 15px">
      <ProverSideStack
        profile="test"
        feed="test"
        :disabled="tests.running"
        @pick="openRun"
      >
        <p class="hint">{{ timeNote }}</p>
      </ProverSideStack>
    </Col>
  </Row>

  <ImportDialog
    v-model="importOpen"
    accept="test"
    @imported="(msg) => tableLog.set(msg)"
  />
</template>

<style scoped>
.options-panel {
  margin-top: 12px;
  padding-bottom: 12px;
  border-bottom: 1px solid var(--line);
}
</style>
