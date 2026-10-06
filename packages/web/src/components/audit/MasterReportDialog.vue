<script setup lang="ts">
import type { AuditReport } from "../../composables/useAuditReplay";
import BaseDialog from "../BaseDialog.vue";
import Disclosure from "../Disclosure.vue";

/** The nightly master-branch audit report: what it is, its contradictions,
 *  and the confirmation that replaces the workspace with its inputs and
 *  replays it. Presentation only -- the Audit tab owns the replay. */
defineProps<{
  /** Open state (v-model). */
  modelValue: boolean;
  report: AuditReport | null;
  loading: boolean;
  error: string;
  /** A replay is running (blocks closing and a second confirm). */
  busy: boolean;
  /** The replay is auditing a subproblem right now (Stop applies). */
  auditing: boolean;
  stopRequested: boolean;
  message: string;
}>();

const emit = defineEmits<{
  "update:modelValue": [open: boolean];
  confirm: [];
  stop: [];
}>();
</script>

<template>
  <BaseDialog
    :model-value="modelValue"
    title="Latest Contradiction Report"
    width="min(800px, 94vw)"
    :dismissible="!busy"
    @update:model-value="emit('update:modelValue', $event)"
  >
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
          WARNING: Continuing will replace your loaded knowledge base and prover
          settings with the workflow's exact inputs. This may overwrite work you
          have saved, including local edits to loaded or audited constituents.
          Save a separate copy of any work you want to keep.
        </p>
        <p>
          Confirm to synchronize with contradiction report and replay the
          reported contradiction steps.
        </p>
      </template>
      <Disclosure summary="Read contradictions.json">
        <pre class="report-text">{{ report.json }}</pre>
      </Disclosure>
    </template>
    <p v-if="message" role="status">{{ message }}</p>
    <template #actions>
      <button
        class="btn ghost"
        type="button"
        :disabled="busy"
        @click="emit('update:modelValue', false)"
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
    </template>
  </BaseDialog>
</template>

<style scoped>
.report-text {
  max-height: 45vh;
  overflow: auto;
  white-space: pre-wrap;
}
</style>
