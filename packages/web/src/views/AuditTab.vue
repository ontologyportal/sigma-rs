<script setup lang="ts">
/** Audit tab: sweeps the KB (or audits one sentence) for contradictions,
 *  and lists the contradictions recorded for this KB -- found here or
 *  imported from the configured sources -- each replayable. The form and
 *  the runs live in composables; this view wires them together and
 *  presents the results. */
import { computed, onActivated, provide, ref, watch } from "vue";
import type { AuditFinding } from "sigmakee/sdk";
import { call } from "../services/sigma";
import { errMsg, fmtNum } from "../utils/format";
import { useKBStore } from "../stores/kb";
import { useShellStore } from "../stores/shell";
import { useContradictionsStore } from "../stores/contradictions";
import { useTabQuery } from "../composables/useTabQuery";
import { fmtSecs } from "../composables/useElapsed";
import { auditSweepKey, useAuditSweep } from "../composables/useAuditSweep";
import {
  useAuditRun,
  type AuditTarget,
  type Contradiction,
} from "../composables/useAuditRun";
import { updateParams } from "../router";
import { batchBreakdown, contradictionKey } from "../utils/contradictionReport";
import type { SharedProofReport } from "../utils/proofReport";
import BusyButton from "../components/BusyButton.vue";
import Card from "../components/Card.vue";
import Col from "../components/Col.vue";
import Disclosure from "../components/Disclosure.vue";
import ProverChips from "../components/ProverChips.vue";
import ProverOptions from "../components/ProverOptions.vue";
import ProverOptionsCard from "../components/ProverOptionsCard.vue";
import Row from "../components/Row.vue";
import Segmented from "../components/Segmented.vue";
import SourceLoc from "../components/SourceLoc.vue";
import AuditAdvancedForm from "../components/audit/AuditAdvancedForm.vue";
import CheckLog from "../components/audit/CheckLog.vue";
import ContradictionCard from "../components/audit/ContradictionCard.vue";
import RecordedContradictions from "../components/audit/RecordedContradictions.vue";
import HistoryToggle from "../components/HistoryToggle.vue";

const kb = useKBStore();
const shell = useShellStore();
const recorded = useContradictionsStore();

// The engine drops findings whose axioms changed: re-read on every visit.
onActivated(() => {
  recorded.refresh().catch(() => {});
});

/** Compact layout: the recorded contradictions toggle open under the form,
 *  as Ask/Tell's run history does. Opened on arrival at a specific finding. */
const recordedOpen = ref(false);
watch(
  () => recorded.focusKey,
  (key) => {
    if (key) recordedOpen.value = true;
  },
  { immediate: true },
);

// -- focus (right-click "Audit this sentence" in the editor) ------------------

const focus = ref<AuditTarget | null>(null);
const focusError = ref("");

const { onQuery, str } = useTabQuery(["audit"]);
onQuery(async (q) => {
  const file = str(q.focus);
  const offset = Number(str(q.at));
  if (!file || !Number.isInteger(offset) || offset < 0) {
    focus.value = null;
    focusError.value = "";
    return;
  }
  if (focus.value?.file === file && focus.value.offset === offset) return;
  focusError.value = "";
  try {
    const { focus: sentence } = await call("sentenceAt", { file, offset });
    if (sentence) focus.value = { file, offset, sentence };
    else {
      focus.value = null;
      focusError.value = `No sentence at that position in ${file} any more — right-click it in the editor again.`;
    }
  } catch (e) {
    focus.value = null;
    focusError.value = errMsg(e);
  }
});

function clearFocus() {
  focus.value = null;
  focusError.value = "";
  updateParams({});
}

// -- form and runs ------------------------------------------------------------

const form = useAuditSweep(computed(() => !!focus.value));
provide(auditSweepKey, form);
const {
  sweep,
  planText,
  totalMinutes,
  presetTotal,
  totalOptions,
  onScopeChange,
  randomSeed,
  sweepDone,
} = form;

/** Contradictions whose proof is folded away (by `contradictionKey`). */
const collapsed = ref(new Set<string>());

const runs = useAuditRun(form, () => {
  collapsed.value = new Set();
});
const {
  auditing,
  stopRequested,
  error,
  lastSecs,
  backendLabel,
  contradictions,
  batches,
  rawLines,
  run,
  progress,
  summary,
  found,
  busyLabel,
} = runs;

/** Replay a recorded contradiction: audit its cited axioms together. */
function replay(finding: AuditFinding) {
  clearFocus();
  return runs.runCulprits(finding);
}

/** A shared audit report confirmed in the Ask/Tell import dialog: record its
 *  contradiction (when every cited axiom is in this KB) and replay it. */
async function importShared(shared: SharedProofReport) {
  if (shared.problem.command !== "audit" || auditing.value) return;
  clearFocus();
  try {
    const outcome = await recorded.importJson(JSON.stringify(shared), "report");
    const finding = outcome.imported[0];
    if (finding) {
      await runs.runCulprits(finding);
      return;
    }
    const missing = outcome.rejected.flatMap((r) => r.missing);
    runs.clearResults();
    error.value = missing.length
      ? `The report's contradiction was not recorded: ${missing.length} cited axiom${missing.length === 1 ? " is" : "s are"} not in this knowledge base (${missing
          .map((a) => (a.file ? `${a.file}:${a.line ?? "?"}` : a.kif))
          .join(", ")}).`
      : (outcome.rejected[0]?.reason ??
        "The report cites no contradiction this knowledge base holds.");
  } catch (e) {
    runs.clearResults();
    error.value = errMsg(e);
  }
}

watch(
  () => shell.auditReportRequest,
  (shared) => {
    if (!shared) return;
    shell.auditReportRequest = null;
    void importShared(shared);
  },
  { immediate: true },
);

const optionsOpen = ref(false);
watch(
  () => sweep.mode,
  (mode) => {
    if (mode === "simple") optionsOpen.value = false;
  },
);
const isCompact = computed(() => shell.isCompact);
/** The classic layout keeps a column on the right: the recorded
 *  contradictions, and (in Advanced) the prover options above them, as on
 *  Ask/Tell. Compact stacks everything under the form. */
const sideColumn = computed(() => !isCompact.value);
/** Advanced in the classic layout: the prover options sit in the side
 *  column; otherwise they toggle open in the form. */
const optionsBeside = computed(
  () => sweep.mode === "advanced" && sideColumn.value,
);

const canContinue = computed(
  () => !focus.value && sweep.step > 0 && !sweepDone.value,
);
const runLabel = computed(() => {
  if (focus.value) return "Audit sentence";
  if (!canContinue.value) return "Run audit";
  return sweep.total
    ? `Continue (step ${fmtNum(sweep.step)} of ${fmtNum(sweep.total)})`
    : `Continue from step ${fmtNum(sweep.step)}`;
});

function start(fresh: boolean) {
  // A finished sweep has nothing left to resume: run a new one.
  if (fresh || (!focus.value && sweepDone.value)) randomSeed();
  return focus.value ? runs.runFocused(focus.value) : runs.runSweep();
}

// -- results -------------------------------------------------------------------

const headline = computed(() => {
  const n = found.value;
  if (n) return `${n} contradiction${n === 1 ? "" : "s"} found`;
  if (summary.value.contradictory) return "Contradiction found";
  return "No contradictions found";
});
const headlineClass = computed(() =>
  found.value || summary.value.contradictory ? "Inconsistent" : "Consistent",
);
const breakdown = computed(() => batchBreakdown(summary.value));

const caveat = computed(() => {
  if (found.value) return "";
  if (summary.value.contradictory)
    return "The engine reported a contradiction without a citable derivation — see the raw engine output.";
  if (run.replayOf)
    return "The recorded contradiction was not re-derived within this check's limits. It stays recorded: a refutation is undone only by changing one of its axioms.";
  if (run.focused)
    return run.widest
      ? `Nothing within neighbourhoods of up to ${fmtNum(run.widest)} axioms. That doesn't prove this sentence consistent with the rest of the KB.`
      : "";
  return "Finding none doesn't prove the KB consistent: only the checked neighbourhoods were searched, and timed-out checks were cut short.";
});

const positionText = computed(() => {
  if (run.replayOf)
    return `Replayed a recorded contradiction (${run.replayOf.culprits.length} cited axiom${run.replayOf.culprits.length === 1 ? "" : "s"})`;
  if (run.focused) return "";
  const steps = `Seed ${run.seed} · steps ${fmtNum(run.start)}–${fmtNum(run.next)}`;
  if (!run.total) return steps;
  return `${steps} of ${fmtNum(run.total)}${run.next >= run.total ? " · sweep complete" : ""}`;
});

const isCollapsed = (c: Contradiction) =>
  collapsed.value.has(contradictionKey(c.steps));
/** Fold or unfold one proof. Idempotent: setting the disclosure's `open`
 *  programmatically (Collapse all) fires its toggle event too. */
function setCollapsed(c: Contradiction, fold: boolean) {
  const key = contradictionKey(c.steps);
  if (collapsed.value.has(key) === fold) return;
  const next = new Set(collapsed.value);
  if (fold) next.add(key);
  else next.delete(key);
  collapsed.value = next;
}
const allCollapsed = computed(
  () =>
    contradictions.value.length > 0 &&
    contradictions.value.every((c) => isCollapsed(c)),
);
function setAllCollapsed(fold: boolean) {
  collapsed.value = fold
    ? new Set(contradictions.value.map((c) => contradictionKey(c.steps)))
    : new Set();
}
</script>

<template>
  <Row>
    <Col :span="sideColumn ? 8 : 12">
      <Card
        title="Consistency audit"
        description="Checks axioms' neighbourhoods (the axioms selected for each, as for a query) for contradictions. Any contradiction found is real, but finding none doesn't prove the KB consistent."
      >
        <template #header>
          <Segmented
            v-model="sweep.mode"
            :options="[
              { value: 'simple', label: 'Simple' },
              { value: 'advanced', label: 'Advanced' },
            ]"
            label="Audit form"
          />
        </template>

        <div v-if="focus" class="focus mt">
          <div class="focus-hd">
            <span class="focus-tag">Auditing one sentence</span>
            <SourceLoc
              v-if="focus.sentence.file"
              :file="focus.sentence.file"
              :line="focus.sentence.line"
              variant="loc"
            />
            <button
              class="btn ghost small"
              type="button"
              :disabled="auditing"
              title="Audit the whole KB or a file instead"
              @click="clearFocus"
            >
              Back to sweep
            </button>
          </div>
          <pre class="focus-kif">{{ focus.sentence.kif }}</pre>
        </div>
        <div v-if="focusError" class="hint bad mt">{{ focusError }}</div>

        <!-- Simple: what to audit and for how long. -->
        <div class="form-row mt">
          <div v-if="!focus">
            <label for="auditScope">audit</label>
            <select
              id="auditScope"
              v-model="sweep.scope"
              :disabled="auditing"
              @change="onScopeChange"
            >
              <option value="">The whole KB</option>
              <option
                v-for="c in kb.constituents"
                :key="c.file"
                :value="c.file"
              >
                {{ c.name }}
              </option>
            </select>
          </div>
          <div v-if="sweep.mode === 'simple'">
            <label>for</label>
            <div class="total">
              <Segmented
                v-model="presetTotal"
                :options="totalOptions"
                label="Total time"
                :disabled="auditing"
              />
              <span v-if="presetTotal === 'custom'" class="custom-total">
                <input
                  v-model.number="totalMinutes"
                  type="number"
                  min="0"
                  step="1"
                  aria-label="Total minutes"
                  :disabled="auditing"
                />
                min
              </span>
            </div>
          </div>
        </div>
        <div class="hint mt-sm plan">{{ planText }}</div>

        <AuditAdvancedForm
          v-if="sweep.mode === 'advanced'"
          :focused="!!focus"
          :disabled="auditing"
        />

        <div class="actions mt">
          <BusyButton
            :busy="auditing"
            :label="runLabel"
            :busy-label="busyLabel"
            :progress="progress"
            @click="start(false)"
          />
          <button
            v-if="canContinue && !auditing"
            class="btn ghost"
            type="button"
            title="Start a new sweep with a fresh random order"
            @click="start(true)"
          >
            Start over
          </button>
          <button
            v-if="auditing"
            class="btn ghost"
            type="button"
            :disabled="stopRequested"
            title="Stop after the current check"
            @click="stopRequested = true"
          >
            {{ stopRequested ? "Stopping…" : "Stop" }}
          </button>
          <ProverChips
            v-model:open="optionsOpen"
            profile="audit"
            :toggle="sweep.mode === 'advanced' && isCompact"
            :disabled="auditing"
          />
          <HistoryToggle
            v-if="isCompact"
            v-model:open="recordedOpen"
            title="Recorded contradictions"
          />
        </div>
        <ProverOptions
          v-if="sweep.mode === 'advanced' && isCompact && optionsOpen"
          profile="audit"
          hide-time
          :disabled="auditing"
        />
        <RecordedContradictions
          v-if="isCompact && recordedOpen"
          class="history-panel"
          embedded
          :busy="auditing"
          @replay="replay"
        />
      </Card>

      <Card v-if="error" class="hint bad">{{ error }}</Card>
      <template v-if="batches.length">
        <Card>
          <div class="result-hd">
            <span :class="`audit-status ${headlineClass}`">{{ headline }}</span>
            <span class="hint">{{ breakdown }}</span>
            <button
              v-if="contradictions.length > 1"
              class="btn ghost small fold-all"
              type="button"
              @click="setAllCollapsed(!allCollapsed)"
            >
              {{ allCollapsed ? "Expand all" : "Collapse all" }}
            </button>
          </div>
          <div class="hint meta">
            via {{ backendLabel }}
            <template v-if="lastSecs != null && !auditing">
              · took {{ fmtSecs(lastSecs) }}</template
            >
            <template v-if="positionText"> · {{ positionText }}</template>
          </div>
          <div v-if="caveat" class="hint mt-sm">{{ caveat }}</div>
          <CheckLog :entries="batches" />
          <Disclosure summary="raw engine output">
            <pre>{{ rawLines.join("\n") || "(none)" }}</pre>
          </Disclosure>
        </Card>
        <ContradictionCard
          v-for="(c, i) in contradictions"
          :key="contradictionKey(c.steps)"
          :contradiction="c"
          :number="i + 1"
          :backend="backendLabel"
          :time-limit-secs="run.perCheckSecs"
          :collapsed="isCollapsed(c)"
          @update:collapsed="setCollapsed(c, $event)"
        />
      </template>
    </Col>
    <Col v-if="sideColumn" :span="4" style="margin-left: 15px">
      <ProverOptionsCard
        v-if="optionsBeside"
        profile="audit"
        hide-time
        :disabled="auditing"
      />
      <RecordedContradictions :busy="auditing" @replay="replay" />
    </Col>
  </Row>
</template>

<style scoped>
.form-row {
  display: flex;
  flex-wrap: wrap;
  gap: 12px 18px;
  align-items: end;
}
.form-row select {
  min-width: 220px;
}
.total {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
}
.custom-total {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 13px;
}
.custom-total input {
  width: 90px;
}
.plan {
  max-width: 70ch;
}
.actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 10px;
}
.focus {
  border: 1px solid color-mix(in srgb, var(--accent) 40%, var(--line));
  border-radius: 8px;
  padding: 10px 12px;
  background: var(--bg);
}
.focus-hd {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 10px;
}
.focus-hd button {
  margin-left: auto;
}
.focus-tag {
  font-size: 12px;
  font-weight: 600;
  color: var(--accent);
}
.focus-kif {
  margin: 8px 0 0;
  font-family: var(--mono);
  font-size: 12px;
  white-space: pre-wrap;
  max-height: 160px;
  overflow: auto;
}
.result-hd {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 10px;
}
.meta {
  margin-top: 6px;
  font-variant-numeric: tabular-nums;
}
.fold-all {
  margin-left: auto;
}
.history-panel {
  margin-top: 12px;
}
</style>
