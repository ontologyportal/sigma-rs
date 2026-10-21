<script setup lang="ts">
import { computed, ref, shallowRef } from "vue";
import { onBeforeRouteLeave } from "vue-router";
import type { AskResult, AuditResult } from "sigmakee/sdk";
import BaseDialog from "./BaseDialog.vue";
import Disclosure from "./Disclosure.vue";
import ProofView from "./ProofView.vue";
import {
  fetchSumoIssue,
  searchSumoContradictions,
  sumoIssueNumber,
  type IssueRef,
} from "../api/github";
import { AUDIT_ENGINE, PROVER_BUILDS } from "../constants";
import { importProofReport } from "../services/proof-report";
import {
  assertProofCompatible,
  parseProofReport,
  proofReportFromIssue,
  proofReplayOutcome,
  type SharedProofReport,
} from "../utils/proofReport";
import { errMsg } from "../utils/format";
import { currentTab, navigate } from "../router";
import { useShellStore } from "../stores/shell";

const props = defineProps<{
  disabled?: boolean;
  allowIssue?: boolean;
  unified?: boolean;
  workflowBusy?: boolean;
}>();
const emit = defineEmits<{ workflowOpen: [] }>();
const open = ref(false);
const busy = ref(false);
const fetching = ref(false);
const searching = ref(false);
const issueInput = ref("");
const issueNumber = ref<number | null>(null);
const issueMode = ref(false);
const browseMode = ref(false);
const fileMode = ref(false);
const workflowMode = ref(false);
const issueResults = ref<IssueRef[]>([]);
const nextIssuePage = ref<number | null>(null);
const incompleteSearch = ref(false);
const report = shallowRef<SharedProofReport | null>(null);
const result = shallowRef<AskResult | AuditResult | null>(null);
const error = ref("");
const message = ref("");
const compatible = ref(false);
const confirmed = ref(false);
const fileInput = ref<HTMLInputElement | null>(null);
const fileName = ref("");
const sources = computed(() => {
  const unique = new Map<
    string,
    SharedProofReport["constituents"][number]["source"]
  >();
  for (const file of report.value?.constituents ?? []) {
    const source = file.source;
    unique.set(
      `${source.repository}@${source.branch}:${source.commit}`,
      source,
    );
  }
  return [...unique.values()];
});
const proverLabel = computed(() => {
  const backend = report.value?.prover.backend;
  return backend === "native" ? "SUPr" : backend === "e" ? "E" : "Vampire";
});
const proofs = computed(() => {
  const r = result.value;
  return !r
    ? []
    : "contradictions" in r
      ? r.contradictions.map((c) => c.steps)
      : [r.proof ?? []];
});
onBeforeRouteLeave(() => !busy.value && !props.workflowBusy);

function backToSources() {
  issueMode.value = false;
  browseMode.value = false;
  fileMode.value = false;
  workflowMode.value = false;
  report.value = null;
  result.value = null;
  error.value = "";
  message.value = "";
  confirmed.value = false;
  compatible.value = false;
}

function openSources() {
  backToSources();
  open.value = true;
}

function chooseFile() {
  backToSources();
  fileMode.value = true;
  fileInput.value?.click();
}

function openWorkflow() {
  backToSources();
  workflowMode.value = true;
  emit("workflowOpen");
}

function openIssue() {
  issueMode.value = true;
  browseMode.value = false;
  fileMode.value = false;
  workflowMode.value = false;
  issueInput.value = "";
  issueNumber.value = null;
  report.value = null;
  result.value = null;
  error.value = "";
  message.value = "";
  confirmed.value = false;
  compatible.value = false;
  open.value = true;
}

function openBrowse() {
  openIssue();
  browseMode.value = true;
  issueResults.value = [];
  nextIssuePage.value = 1;
  incompleteSearch.value = false;
  void loadMoreIssues();
}

async function loadMoreIssues() {
  const page = nextIssuePage.value;
  if (searching.value || page === null) return;
  searching.value = true;
  error.value = "";
  try {
    const found = await searchSumoContradictions(page);
    issueResults.value = [...issueResults.value, ...found.issues];
    nextIssuePage.value = found.hasMore ? page + 1 : null;
    incompleteSearch.value ||= found.incomplete;
  } catch (e) {
    error.value = errMsg(e);
  } finally {
    searching.value = false;
  }
}

async function loadIssue() {
  if (fetching.value) return;
  fetching.value = true;
  error.value = "";
  report.value = null;
  compatible.value = false;
  try {
    const number = sumoIssueNumber(issueInput.value);
    const body = await fetchSumoIssue(number);
    const loaded = proofReportFromIssue(body);
    assertProofCompatible(loaded, AUDIT_ENGINE, PROVER_BUILDS);
    report.value = loaded;
    issueNumber.value = number;
    fileName.value = `SUMO issue #${number}`;
    compatible.value = true;
  } catch (e) {
    error.value = errMsg(e);
  } finally {
    fetching.value = false;
  }
}

function chooseIssue(number: number) {
  issueInput.value = String(number);
  error.value = "";
}

function backToIssues() {
  report.value = null;
  result.value = null;
  issueNumber.value = null;
  compatible.value = false;
  confirmed.value = false;
  error.value = "";
  message.value = "";
}

async function selectFile(event: Event) {
  const file = (event.target as HTMLInputElement).files?.[0];
  if (!file) return;
  open.value = true;
  issueMode.value = false;
  browseMode.value = false;
  fileMode.value = true;
  workflowMode.value = false;
  fileName.value = file.name;
  report.value = null;
  result.value = null;
  error.value = "";
  message.value = "";
  compatible.value = false;
  confirmed.value = false;
  try {
    if (file.size > 8_000_000)
      throw new Error("Proof report exceeds the 8 MB limit.");
    report.value = parseProofReport(await file.text());
    assertProofCompatible(report.value, AUDIT_ENGINE, PROVER_BUILDS);
    compatible.value = true;
  } catch (e) {
    error.value = errMsg(e);
  } finally {
    (event.target as HTMLInputElement).value = "";
  }
}

async function replay() {
  if (!report.value || !compatible.value || !confirmed.value || busy.value)
    return;
  if (report.value.problem.command === "audit") {
    error.value = "";
    try {
      await navigate("audit");
      if (currentTab() !== "audit")
        throw new Error(
          "Could not open the Audit tab. Try again when the current operation finishes.",
        );
      useShellStore().auditReportRequest = report.value;
      open.value = false;
      confirmed.value = false;
    } catch (e) {
      error.value = errMsg(e);
    }
    return;
  }
  busy.value = true;
  error.value = "";
  result.value = null;
  try {
    result.value = await importProofReport(report.value, (value) => {
      message.value = value;
    });
    message.value = proofReplayOutcome(report.value, result.value);
    confirmed.value = false;
  } catch (e) {
    error.value = errMsg(e);
    message.value = "";
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <button
    class="btn ghost small"
    type="button"
    :disabled="disabled || busy"
    @click="unified ? openSources() : fileInput?.click()"
  >
    {{ unified ? "Import contradiction" : "Import proof report" }}
  </button>
  <button
    v-if="allowIssue && !unified"
    class="btn ghost small"
    type="button"
    :disabled="disabled || busy"
    @click="openIssue"
  >
    Load GitHub proof issue
  </button>
  <button
    v-if="allowIssue && !unified"
    class="btn ghost small"
    type="button"
    :disabled="disabled || busy"
    @click="openBrowse"
  >
    Browse reported contradictions
  </button>
  <input
    ref="fileInput"
    type="file"
    accept=".json,application/json"
    hidden
    @change="selectFile"
  />
  <BaseDialog
    v-model="open"
    :title="
      unified
        ? 'Import contradiction'
        : browseMode
          ? 'Reported SUMO contradictions'
          : issueMode
            ? 'Load SUMO proof issue'
            : 'Import proof report'
    "
    width="min(640px, 94vw)"
    :dismissible="!busy && !fetching && !workflowBusy"
  >
    <div
      v-if="
        unified &&
        !issueMode &&
        !fileMode &&
        !workflowMode &&
        !report &&
        !result
      "
      class="source-choices"
    >
      <p class="hint">Choose where to load a contradiction from.</p>
      <button class="source-choice" type="button" @click="chooseFile">
        <strong>JSON report file</strong>
        <span>A proof report saved or shared by another user.</span>
      </button>
      <button class="source-choice" type="button" @click="openBrowse">
        <strong>GitHub issue</strong>
        <span>Browse reported contradictions or enter an issue URL.</span>
      </button>
      <button class="source-choice" type="button" @click="openWorkflow">
        <strong>Latest GitHub contradiction workflow</strong>
        <span>Replay the nightly SUMO master-branch audit findings.</span>
      </button>
    </div>
    <div v-if="fileMode && !report" class="file-lookup">
      <p class="hint">Choose a version 3 proof report JSON file.</p>
      <button class="btn" type="button" @click="fileInput?.click()">
        Choose JSON file
      </button>
    </div>
    <button
      v-if="unified && (issueMode || fileMode || workflowMode) && !workflowBusy"
      class="btn ghost small source-back"
      type="button"
      :disabled="busy || fetching"
      @click="backToSources"
    >
      All sources
    </button>
    <section
      v-if="browseMode && !report"
      class="issue-results"
      aria-label="Reported contradictions"
    >
      <p class="hint">
        SUMO issues whose titles start with [Contradiction]. Select one, then
        click Load issue to review its report.
      </p>
      <p v-if="searching && !issueResults.length" role="status">
        Searching GitHub issues...
      </p>
      <ul v-if="issueResults.length">
        <li v-for="issue in issueResults" :key="issue.number">
          <button
            class="issue-choice"
            :class="{ selected: issueInput.trim() === String(issue.number) }"
            type="button"
            :disabled="fetching"
            :aria-pressed="issueInput.trim() === String(issue.number)"
            @click="chooseIssue(issue.number)"
          >
            <span class="issue-title">{{ issue.title }}</span>
            <span class="hint">#{{ issue.number }} · {{ issue.state }}</span>
          </button>
        </li>
      </ul>
      <p v-else-if="!searching && !nextIssuePage && !error" class="hint">
        No matching contradiction issues found.
      </p>
      <p v-if="incompleteSearch" class="hint">
        GitHub reported incomplete search results. Try again later or enter an
        issue number below.
      </p>
      <button
        v-if="nextIssuePage"
        class="btn ghost small"
        type="button"
        :disabled="searching || fetching"
        @click="loadMoreIssues"
      >
        {{ searching ? "Loading more..." : "Load more issues" }}
      </button>
    </section>
    <form
      v-if="issueMode && !report"
      class="issue-lookup"
      @submit.prevent="loadIssue"
    >
      <label for="proof-issue-number">SUMO issue number or URL</label>
      <input
        id="proof-issue-number"
        v-model="issueInput"
        type="text"
        placeholder="https://github.com/ontologyportal/sumo/issues/123"
        :disabled="fetching"
      />
    </form>
    <button
      v-if="issueMode && !browseMode && !report"
      class="btn ghost small browse-link"
      type="button"
      @click="openBrowse"
    >
      Browse reported contradictions
    </button>
    <slot v-if="workflowMode" name="workflow" :close="() => (open = false)" />
    <div v-if="report && !result" class="import-review">
      <button
        v-if="browseMode"
        class="btn ghost small"
        type="button"
        @click="backToIssues"
      >
        Back to issues
      </button>
      <p class="intro">Review the report before replacing your workspace.</p>
      <dl class="report-summary">
        <div class="report-file">
          <dt>Report</dt>
          <dd>
            <a
              v-if="issueMode && issueNumber"
              :href="`https://github.com/ontologyportal/sumo/issues/${issueNumber}`"
              target="_blank"
              rel="noopener noreferrer"
              >{{ fileName }}</a
            >
            <template v-else>{{ fileName }}</template>
          </dd>
        </div>
        <div>
          <dt>Replay</dt>
          <dd>
            {{
              report.problem.command === "audit"
                ? "Consistency audit"
                : "Conjecture proof"
            }}
          </dd>
        </div>
        <div>
          <dt>Prover</dt>
          <dd>{{ proverLabel }}</dd>
        </div>
        <div>
          <dt>KB files</dt>
          <dd>{{ report.constituents.length }}</dd>
        </div>
      </dl>
      <section class="sources" aria-label="Source revisions">
        <h4>Source revisions</h4>
        <ul>
          <li
            v-for="source in sources"
            :key="`${source.repository}@${source.branch}:${source.commit}`"
          >
            <div class="source-name">
              <strong>{{ source.repository.replace("github:", "") }}</strong>
              <span class="hint">{{ source.branch }}</span>
            </div>
            <a
              :href="`https://github.com/${source.repository.replace('github:', '')}/commit/${source.commit}`"
              :title="source.commit"
              target="_blank"
              rel="noopener noreferrer"
              ><code>{{ source.commit.slice(0, 12) }}</code></a
            >
          </li>
        </ul>
        <p class="hint">
          Files will be downloaded and verified at these commits.
        </p>
      </section>
      <div class="report-details">
        <p v-if="compatible" class="compatibility">
          Engine and prover builds match.
        </p>
        <Disclosure summary="Problem and technical details">
          <p class="hint">
            Engine commit: <code>{{ report.engine.commit }}</code>
          </p>
          <pre>{{
            JSON.stringify(
              { problem: report.problem, config: report.config },
              null,
              2,
            )
          }}</pre>
        </Disclosure>
      </div>
      <div v-if="compatible" class="replacement">
        <p>
          <strong>This replaces your loaded KB and prover settings.</strong>
          Local edits to loaded and imported files will be discarded.
        </p>
        <label class="check">
          <input v-model="confirmed" type="checkbox" :disabled="busy" />
          <span>I have saved a separate copy of any work I want to keep.</span>
        </label>
      </div>
      <p v-if="report.problem.command === 'audit'" class="hint destination">
        Replay will open in the Audit tab.
      </p>
    </div>
    <p v-if="error" class="feedback bad" role="alert">{{ error }}</p>
    <p v-if="message" class="feedback" role="status">{{ message }}</p>
    <ProofView v-for="(steps, i) in proofs" :key="i" :steps="steps" />
    <template v-if="!workflowMode" #actions>
      <button
        class="btn ghost"
        type="button"
        :disabled="busy"
        @click="open = false"
      >
        Close
      </button>
      <button
        v-if="issueMode && !report"
        class="btn"
        type="button"
        :disabled="fetching || !issueInput.trim()"
        @click="loadIssue"
      >
        {{ fetching ? "Loading issue..." : "Load issue" }}
      </button>
      <button
        v-if="report && !result"
        class="btn"
        type="button"
        :disabled="busy || !compatible || !confirmed"
        @click="replay"
      >
        {{ busy ? "Loading and replaying..." : "Import and replay" }}
      </button>
    </template>
  </BaseDialog>
</template>

<style scoped>
.source-choices {
  display: grid;
  gap: 9px;
}
.source-choices p {
  margin-bottom: 4px;
}
.source-choice {
  display: grid;
  gap: 4px;
  width: 100%;
  padding: 14px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--bg);
  color: var(--fg);
  text-align: left;
  cursor: pointer;
}
.source-choice:hover,
.source-choice:focus-visible {
  border-color: var(--accent);
}
.source-choice span {
  color: var(--muted);
  font-size: 12px;
}
.file-lookup {
  display: grid;
  gap: 12px;
  justify-items: start;
}
.browse-link {
  margin-top: 12px;
}
.source-back {
  margin: 12px 0;
}
.issue-results {
  margin-bottom: 18px;
}
.issue-results ul {
  list-style: none;
  padding: 0;
  margin: 10px 0;
  max-height: 320px;
  overflow: auto;
}
.issue-results li + li {
  border-top: 1px solid var(--line);
}
.issue-choice {
  display: flex;
  flex-direction: column;
  gap: 3px;
  width: 100%;
  padding: 10px 4px;
  border: 0;
  background: transparent;
  color: var(--fg);
  text-align: left;
  cursor: pointer;
}
.issue-choice:hover,
.issue-choice:focus-visible {
  background: var(--bg);
}
.issue-choice.selected {
  background: var(--bg);
  box-shadow: inset 3px 0 var(--accent);
}
.issue-title {
  font-weight: 600;
  overflow-wrap: anywhere;
}
.import-review {
  display: grid;
  gap: 18px;
  font-size: 13px;
  line-height: 1.5;
}
p {
  margin: 0;
}
.intro,
dt {
  color: var(--muted);
}
.report-summary {
  display: grid;
  grid-template-columns: 2fr 1fr 1fr;
  gap: 12px 20px;
  margin: 0;
}
.report-file {
  grid-column: 1 / -1;
}
dt {
  font-size: 12px;
  margin-bottom: 3px;
}
dd {
  margin: 0;
  overflow-wrap: anywhere;
  font-weight: 600;
}
h4 {
  font-size: 12px;
  font-weight: 600;
  margin: 0 0 8px;
}
.sources {
  border-top: 1px solid var(--line);
  padding-top: 16px;
}
.sources ul {
  list-style: none;
  margin: 0 0 8px;
  padding: 0;
  max-height: 180px;
  overflow: auto;
}
.sources li {
  display: flex;
  justify-content: space-between;
  align-items: baseline;
  gap: 12px;
  padding: 4px 0;
}
.source-name {
  display: flex;
  flex-wrap: wrap;
  gap: 2px 10px;
  min-width: 0;
  overflow-wrap: anywhere;
}
.sources a {
  flex-shrink: 0;
}
.report-details :deep(.disclosure) {
  margin-top: 6px;
}
.compatibility {
  font-size: 12px;
  color: var(--muted);
}
.replacement {
  background: var(--bg);
  border: 1px solid var(--line);
  border-radius: 8px;
  padding: 14px;
}
.replacement .check {
  margin: 12px 0 0;
  align-items: flex-start;
  font-size: 13px;
  line-height: 1.5;
}
.replacement input {
  flex-shrink: 0;
  margin-top: 3px;
}
.destination {
  margin-top: -8px;
}
.feedback {
  margin: 16px 0;
  font-size: 13px;
  overflow-wrap: anywhere;
}
pre {
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  max-height: 240px;
  overflow: auto;
}
code {
  overflow-wrap: anywhere;
}
@media (max-width: 480px) {
  .report-summary {
    grid-template-columns: 1fr 1fr;
  }
  .sources li {
    flex-direction: column;
    gap: 2px;
    padding: 6px 0;
  }
}
</style>
