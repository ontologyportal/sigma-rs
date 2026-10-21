<script setup lang="ts">
import { ref } from "vue";
import CopyButton from "./CopyButton.vue";
import BaseDialog from "./BaseDialog.vue";
import type { ProofReport } from "../utils/proofReport";
import { exportProofReport } from "../services/proof-report";
import { downloadText, errMsg } from "../utils/format";

const props = defineProps<{ report: ProofReport }>();
const open = ref(false);
const text = ref("");
const status = ref("");
const busy = ref(false);
async function prepare() {
  open.value = true;
  busy.value = true;
  text.value = "";
  status.value = "";
  try {
    text.value = await exportProofReport(props.report, (message) => {
      status.value = message;
    });
    status.value = "All input files match pushed branch HEAD.";
  } catch (e) {
    status.value = errMsg(e);
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <button class="btn ghost small" @click="prepare">Share result</button>
  <BaseDialog
    v-model="open"
    title="Share proof report"
    width="min(800px, 92vw)"
  >
    <p>
      The report includes repository commit references, the cited axioms,
      problem settings, and proof. Complete KIF files are not included.
    </p>
    <p>
      <strong
        >You cannot export your proof if any local input file differs from the
        pushed HEAD of its GitHub branch.</strong
      >
      Either push your changes to the head of the branch or reset your local
      files to the head of the branch, then rerun the proof before exporting.
    </p>
    <p v-if="text">
      Recipients can import this report even after the branch advances. Copy or
      download it below.
    </p>
    <p v-if="busy">Preparing report...</p>
    <p v-if="status" role="status">{{ status }}</p>
    <textarea
      v-if="text"
      :value="text"
      readonly
      aria-label="Copyable proof report"
      style="width: 100%; height: 320px"
    />
    <template #actions>
      <CopyButton :text="text" label="Copy report" :disabled="!text || busy" />
      <button
        class="btn"
        :disabled="!text || busy"
        @click="downloadText('sigma-proof-report.json', text)"
      >
        Download report
      </button>
      <button class="btn ghost" @click="open = false">Close</button>
    </template>
  </BaseDialog>
</template>
