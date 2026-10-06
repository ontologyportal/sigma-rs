<script setup lang="ts">
/** Audit tab: sweeps the KB (or audits one sentence) for contradictions,
 *  and replays the nightly master-branch report. The form, the runs and the
 *  replay each live in a composable; this view wires them together and
 *  presents the results. */
import { computed, provide, ref, watch } from "vue";
import { call } from "../services/sigma";
import { errMsg, fmtNum } from "../utils/format";
import { useKBStore } from "../stores/kb";
import { useShellStore } from "../stores/shell";
import { useTabQuery } from "../composables/useTabQuery";
import { fmtSecs } from "../composables/useElapsed";
import { auditSweepKey, useAuditSweep } from "../composables/useAuditSweep";
import {
  useAuditRun,
  type AuditTarget,
  type Contradiction,
} from "../composables/useAuditRun";
import { useAuditReplay } from "../composables/useAuditReplay";
import { updateParams } from "../router";
import { batchBreakdown, contradictionKey } from "../utils/contradictionReport";
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
import MasterReportDialog from "../components/audit/MasterReportDialog.vue";

const kb = useKBStore();
const shell = useShellStore();

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

// -- form, runs, replay -------------------------------------------------------

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
  replayMode,
  lastRunRecheck,
  run,
  progress,
  summary,
  found,
  busyLabel,
} = runs;

const {
  reportOpen,
  reportLoading,
  report,
  reportError,
  replayBusy,
  replayDone,
  replayMessage,
  loadedReplay,
  recheckReason,
  showMasterReport,
  confirmReplay,
  recheckContradictions,
} = useAuditReplay(runs, form);

const optionsOpen = ref(false);
watch(
  () => sweep.mode,
  (mode) => {
    if (mode === "simple") optionsOpen.value = false;
  },
);
const isCompact = computed(() => shell.isCompact);
/** Advanced in the classic layout: the prover options sit in a column on
 *  the right, as on Ask/Tell; otherwise they toggle open in the form. */
const optionsBeside = computed(
  () => sweep.mode === "advanced" && !isCompact.value,
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
  if (found.value || replayMode.value) return "";
  if (summary.value.contradictory)
    return "The engine reported a contradiction without a citable derivation — see the raw engine output.";
  if (run.focused)
    return run.widest
      ? `Nothing within neighbourhoods of up to ${fmtNum(run.widest)} axioms. That doesn't prove this sentence consistent with the rest of the KB.`
      : "";
  return "Finding none doesn't prove the KB consistent: only the checked neighbourhoods were searched, and timed-out checks were cut short.";
});

const positionText = computed(() => {
  if (lastRunRecheck.value)
    return `Rechecked ${replayDone.value} of ${run.planned} reported targets against local edits`;
  if (replayMode.value)
    return `Replayed ${replayDone.value} of ${run.planned} reported subproblems (intermediate steps skipped)`;
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
    <Col :span="optionsBeside ? 8 : 12">
      <MasterReportDialog
        v-model="reportOpen"
        :report="report"
        :loading="reportLoading"
        :error="reportError"
        :busy="replayBusy"
        :auditing="auditing"
        :stop-requested="stopRequested"
        :message="replayMessage"
        @confirm="confirmReplay"
        @stop="stopRequested = true"
      />
      <Card
        title="Consistency audit"
        description="Checks axioms' neighbourhoods (the axioms selected for each, as for a query) for contradictions. Any contradiction found is real, but finding none doesn't prove the KB consistent."
      >
        <template #header>
          <button
            class="btn ghost small"
            type="button"
            title="The nightly master-branch audit: read it, replay it, recheck your edits against it"
            :disabled="auditing || replayBusy"
            @click="showMasterReport"
          >
            ⓘ Latest Contradiction Report
          </button>
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
            :disabled="replayBusy"
            :busy="auditing"
            :label="runLabel"
            :busy-label="busyLabel"
            :progress="progress"
            @click="start(false)"
          />
          <button
            v-if="canContinue && !auditing && !replayBusy"
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
          <button
            v-if="loadedReplay"
            class="btn ghost"
            type="button"
            :disabled="auditing || replayBusy || !!recheckReason"
            title="Recheck the report's contradictions against your edits"
            @click="recheckContradictions"
          >
            Recheck reported contradictions
          </button>
        </div>
        <p v-if="replayMessage" class="hint mt-sm" role="status">
          {{ replayMessage }}
        </p>
        <p v-if="loadedReplay && recheckReason" class="hint bad" role="alert">
          Recheck unavailable: {{ recheckReason }} Your edits have not been
          replaced.
        </p>
        <ProverOptions
          v-if="sweep.mode === 'advanced' && isCompact && optionsOpen"
          profile="audit"
          hide-time
          :disabled="auditing"
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
    <Col v-if="optionsBeside" :span="4" style="margin-left: 15px">
      <ProverOptionsCard profile="audit" hide-time :disabled="auditing" />
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
</style>
