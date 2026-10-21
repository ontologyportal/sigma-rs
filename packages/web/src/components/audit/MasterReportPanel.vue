<script setup lang="ts">
import type { AuditReport } from "../../composables/useAuditReplay";
import Disclosure from "../Disclosure.vue";

defineProps<{
  report: AuditReport | null;
  loading: boolean;
  error: string;
  busy: boolean;
  auditing: boolean;
  stopRequested: boolean;
  message: string;
}>();

const emit = defineEmits<{
  confirm: [];
  stop: [];
  close: [];
}>();
</script>

<template>
  <div class="workflow-report">
    <p>
      SUMO runs a two-hour contradiction audit each night. This report records
      the contradictions found and the steps that produced them. Continue to
      synchronize your workspace with the report's verified master-branch inputs
      and replay only those steps. Reports cannot be loaded if master has
      changed since the audit ran.
    </p>
    <p>
      After replaying, edit existing formulas and use Recheck reported
      contradictions to test your changes without replacing your work. Adding or
      removing formulas, or changes that make a target ambiguous, invalidate
      rechecking. Keep this app session open while editing.
    </p>
    <p v-if="loading" role="status">
      Fetching the latest completed master audit...
    </p>
    <p v-if="error" class="hint bad" role="alert">{{ error }}</p>
    <template v-if="report">
      <p>
        <a :href="report.runUrl" target="_blank" rel="noopener noreferrer"
          >View workflow run</a
        >
      </p>
      <p v-if="report.unavailable" class="hint bad" role="alert">
        {{ report.unavailable }}
      </p>
      <p v-else-if="!report.replay.findings.length">
        This audit reported no contradictions to replay.
      </p>
      <template v-else>
        <p class="hint bad">
          Continuing will replace your loaded knowledge base and prover settings
          with the workflow's exact inputs. This may overwrite work you have
          saved, including local edits to loaded or audited constituents. Save a
          separate copy of any work you want to keep.
        </p>
        <p>
          Confirm to synchronize and replay the reported contradiction steps.
        </p>
      </template>
      <Disclosure summary="Read contradictions.json">
        <pre class="report-text">{{ report.json }}</pre>
      </Disclosure>
    </template>
    <p v-if="message" role="status">{{ message }}</p>
    <div class="panel-actions">
      <button
        class="btn ghost"
        type="button"
        :disabled="busy"
        @click="emit('close')"
      >
        Close
      </button>
      <button
        v-if="busy && auditing"
        class="btn ghost"
        type="button"
        :disabled="stopRequested"
        @click="emit('stop')"
      >
        Stop after this subproblem
      </button>
      <button
        class="btn"
        type="button"
        :disabled="
          loading ||
          busy ||
          !report ||
          !!report.unavailable ||
          !report.replay.findings.length
        "
        @click="emit('confirm')"
      >
        {{ busy ? "Replaying..." : "Confirm: replace work and replay" }}
      </button>
    </div>
  </div>
</template>

<style scoped>
.workflow-report {
  display: grid;
  gap: 12px;
  line-height: 1.5;
}
.workflow-report p {
  margin: 0;
}
.report-text {
  max-height: 45vh;
  overflow: auto;
  white-space: pre-wrap;
}
.panel-actions {
  display: flex;
  justify-content: flex-end;
  flex-wrap: wrap;
  gap: 10px;
  margin-top: 4px;
}
.panel-actions button:first-child {
  margin-right: auto;
}
</style>
