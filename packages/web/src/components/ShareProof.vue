<script setup lang="ts">
import { computed, ref } from "vue";
import CopyButton from "./CopyButton.vue";
import BaseDialog from "./BaseDialog.vue";
import { createSumoIssue, type IssueRef } from "../api/github";
import { SUMO } from "../constants";
import { useAuthStore } from "../stores/auth";
import {
  parseProofReport,
  proofIssueBody,
  type ProofReport,
  type SharedProofReport,
} from "../utils/proofReport";
import { exportProofReport } from "../services/proof-report";
import { downloadText, errMsg } from "../utils/format";

const props = defineProps<{ report: ProofReport }>();
const auth = useAuthStore();
const open = ref(false);
const mode = ref<"share" | "issue">("share");
const text = ref("");
const status = ref("");
const busy = ref(false);
const posting = ref(false);
const issueError = ref("");
const created = ref<IssueRef | null>(null);
const shared = ref<SharedProofReport | null>(null);
const title = ref("");
const description = ref("");
const issueSizeError = computed(() => {
  if (!text.value) return "";
  try {
    proofIssueBody(text.value, description.value);
    return "";
  } catch (e) {
    return errMsg(e);
  }
});

function suggestedTitle(report: SharedProofReport): string {
  const axiom = report.axioms[0];
  const detail =
    report.problem.command === "prove"
      ? report.problem.query.replace(/\s+/g, " ").slice(0, 80) || "Conjecture"
      : axiom?.file && axiom.line
        ? `${axiom.file}:${axiom.line}`
        : "SUMO audit result";
  return `${report.problem.command === "audit" ? "[Contradiction]" : "[Proof]"} ${detail}`;
}

async function postIssue() {
  if (!text.value || !shared.value || !title.value.trim() || posting.value)
    return;
  if (!auth.token) {
    auth.openLoginDialog();
    return;
  }
  posting.value = true;
  issueError.value = "";
  try {
    const prefix =
      shared.value.problem.command === "audit" ? "[Contradiction]" : "[Proof]";
    const suffix = title.value
      .trim()
      .replace(/^\[(?:Contradiction|Proof)\]\s*/i, "")
      .trim();
    if (!suffix) throw new Error("Enter an issue title.");
    created.value = await createSumoIssue(
      `${prefix} ${suffix}`,
      proofIssueBody(text.value, description.value),
    );
  } catch (e) {
    issueError.value = errMsg(e);
  } finally {
    posting.value = false;
  }
}
async function prepare(nextMode: "share" | "issue") {
  mode.value = nextMode;
  open.value = true;
  busy.value = true;
  text.value = "";
  status.value = "";
  shared.value = null;
  created.value = null;
  issueError.value = "";
  description.value = "";
  try {
    text.value = await exportProofReport(props.report, (message) => {
      status.value = message;
    });
    shared.value = parseProofReport(text.value);
    title.value = suggestedTitle(shared.value);
    status.value = "All input files match pushed branch HEAD.";
  } catch (e) {
    status.value = errMsg(e);
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <button
    class="btn ghost small"
    type="button"
    :disabled="busy"
    @click="prepare('share')"
  >
    Share result
  </button>
  <button
    class="btn ghost small"
    type="button"
    :disabled="busy"
    @click="prepare('issue')"
  >
    Report proof to GitHub
  </button>
  <BaseDialog
    v-model="open"
    :title="mode === 'issue' ? 'Report proof to GitHub' : 'Share proof report'"
    width="min(800px, 92vw)"
    :dismissible="!posting"
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
    <p v-if="text && mode === 'share'">
      Recipients can import this report even after the branch advances. Copy or
      download it below.
    </p>
    <p v-if="busy">Preparing report...</p>
    <p v-if="status" role="status">{{ status }}</p>
    <textarea
      v-if="text && mode === 'share'"
      :value="text"
      readonly
      aria-label="Copyable proof report"
      style="width: 100%; height: 320px"
    />
    <section
      v-if="mode === 'issue' && shared?.result.proof.length"
      class="issue-form"
    >
      <h4>Report to SUMO on GitHub</h4>
      <p>
        This creates an issue in {{ SUMO.owner }}/{{ SUMO.repo }} with the
        reproducible report attached. The issue title will start with
        {{
          shared.problem.command === "audit" ? "[Contradiction]" : "[Proof]"
        }}.
      </p>
      <template v-if="created">
        <p role="status">
          Created
          <a :href="created.url" target="_blank" rel="noopener noreferrer"
            >issue #{{ created.number }}</a
          >.
        </p>
      </template>
      <template v-else>
        <label for="proof-issue-title">Title</label>
        <input id="proof-issue-title" v-model="title" maxlength="256" />
        <label for="proof-issue-description">Description</label>
        <textarea
          id="proof-issue-description"
          v-model="description"
          rows="4"
          placeholder="Explain why this proof or contradiction matters."
        />
        <p v-if="issueSizeError" class="bad" role="alert">
          {{ issueSizeError }}
        </p>
        <p v-if="issueError" class="bad" role="alert">{{ issueError }}</p>
        <button
          v-if="!auth.signedIn"
          class="btn"
          type="button"
          @click="auth.openLoginDialog()"
        >
          Log in with GitHub
        </button>
        <button
          v-else
          class="btn"
          type="button"
          :disabled="posting || !!issueSizeError || !title.trim()"
          @click="postIssue"
        >
          {{ posting ? "Creating issue..." : "Create SUMO issue" }}
        </button>
      </template>
    </section>
    <p v-if="mode === 'issue' && shared && !shared.result.proof.length">
      This result has no proof steps to report. You can still copy or download
      its JSON report.
    </p>
    <template #actions>
      <CopyButton :text="text" label="Copy report" :disabled="!text || busy" />
      <button
        class="btn"
        :disabled="!text || busy"
        @click="downloadText('sigma-proof-report.json', text)"
      >
        Download report
      </button>
      <button class="btn ghost" :disabled="posting" @click="open = false">
        Close
      </button>
    </template>
  </BaseDialog>
</template>

<style scoped>
.issue-form {
  margin-top: 18px;
  padding-top: 14px;
  border-top: 1px solid var(--line);
}
.issue-form h4 {
  margin: 0 0 8px;
}
.issue-form label {
  display: block;
  margin: 10px 0 4px;
}
.issue-form input,
.issue-form textarea {
  width: 100%;
  box-sizing: border-box;
}
.issue-form button {
  margin-top: 12px;
}
.bad {
  color: var(--bad);
}
</style>
