<script setup lang="ts">
import type { Contradiction } from "../../composables/useAuditRun";
import { proofSummary } from "../../utils/contradictionReport";
import { fmtTime } from "../../utils/format";
import Card from "../Card.vue";
import ProofView from "../ProofView.vue";
import DiagnoseContradiction from "./DiagnoseContradiction.vue";
import ReportContradiction from "./ReportContradiction.vue";

/** One contradiction an audit found: its number, size and when it turned
 *  up, the proof (foldable via v-model `collapsed`), and the diagnose /
 *  report actions. */
defineProps<{
  contradiction: Contradiction;
  /** 1-based position in the run's list. */
  number: number;
  /** The backend that found it, for the report. */
  backend: string;
  timeLimitSecs: number;
  collapsed: boolean;
}>();
const emit = defineEmits<{ "update:collapsed": [collapsed: boolean] }>();
</script>

<template>
  <Card>
    <div class="contradiction-hd">
      <span
        >Contradiction {{ number }} · {{ contradiction.steps.length }} step{{
          contradiction.steps.length === 1 ? "" : "s"
        }}</span
      >
      <time
        class="hint"
        :datetime="new Date(contradiction.foundAt).toISOString()"
        :title="new Date(contradiction.foundAt).toLocaleString()"
        >found {{ fmtTime(new Date(contradiction.foundAt)) }}</time
      >
    </div>
    <ProofView
      :steps="contradiction.steps"
      :prologue="contradiction.proof_tptp_prologue"
      :prose="contradiction.prose"
      :prose-missing="contradiction.prose_missing"
      :graphviz="contradiction.graphviz"
      :steps-summary="proofSummary(contradiction.steps)"
      :steps-open="!collapsed"
      @steps-toggle="emit('update:collapsed', !$event)"
    />
    <div class="contradiction-actions">
      <DiagnoseContradiction :steps="contradiction.steps" />
      <ReportContradiction
        :steps="contradiction.steps"
        :prose="contradiction.prose"
        :backend="backend"
        :time-limit-secs="timeLimitSecs"
      />
    </div>
  </Card>
</template>

<style scoped>
.contradiction-hd {
  display: flex;
  align-items: baseline;
  gap: 12px;
  font-weight: 600;
}
.contradiction-hd time {
  margin-left: auto;
  font-weight: normal;
  font-variant-numeric: tabular-nums;
}
.contradiction-actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  margin-top: 10px;
}
</style>
