<script setup lang="ts">
import { computed, ref, shallowRef } from "vue";
import { onBeforeRouteLeave } from "vue-router";
import type { AskResult, AuditResult } from "sigmakee/sdk";
import BaseDialog from "./BaseDialog.vue";
import Disclosure from "./Disclosure.vue";
import ProofView from "./ProofView.vue";
import { AUDIT_ENGINE, PROVER_BUILDS } from "../constants";
import { importProofReport } from "../services/proof-report";
import {
  assertProofCompatible,
  parseProofReport,
  proofReplayOutcome,
  type SharedProofReport,
} from "../utils/proofReport";
import { errMsg } from "../utils/format";
import { currentTab, navigate } from "../router";
import { useShellStore } from "../stores/shell";

defineProps<{ disabled?: boolean }>();
const open = ref(false);
const busy = ref(false);
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
onBeforeRouteLeave(() => !busy.value);

async function selectFile(event: Event) {
  const file = (event.target as HTMLInputElement).files?.[0];
  if (!file) return;
  open.value = true;
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
    @click="fileInput?.click()"
  >
    Import proof report
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
    title="Import proof report"
    width="min(640px, 94vw)"
    :dismissible="!busy"
  >
    <div v-if="report && !result" class="import-review">
      <p class="intro">Review the report before replacing your workspace.</p>
      <dl class="report-summary">
        <div class="report-file">
          <dt>Report</dt>
          <dd>{{ fileName }}</dd>
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
    <template #actions>
      <button
        class="btn ghost"
        type="button"
        :disabled="busy"
        @click="open = false"
      >
        Close
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
