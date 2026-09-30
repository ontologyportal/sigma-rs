<script setup lang="ts">
import { computed, reactive, ref, shallowRef, watch } from "vue";
import { onBeforeRouteLeave } from "vue-router";
import type { AuditBatch, AuditResult } from "sigmakee/sdk";
import { call, isWasmAbort } from "../services/sigma";
import { errMsg } from "../utils/format";
import { useBootStore } from "../stores/boot";
import { useKBStore } from "../stores/kb";
import { useProverStore } from "../stores/prover";
import BusyButton from "../components/BusyButton.vue";
import { useElapsed } from "../composables/useElapsed";
import Card from "../components/Card.vue";
import BaseDialog from "../components/BaseDialog.vue";
import {
  latestAuditReport,
  loadAuditReplay,
  checkAuditReplay,
} from "../services/audit-replay";
import {
  replayPositions,
  replayAxiomKey,
  type AuditReplay,
} from "../utils/auditReplay";
import Disclosure from "../components/Disclosure.vue";
import ProofView from "../components/ProofView.vue";
import ProverSettings from "../components/ProverSettings.vue";
import SourceLoc from "../components/SourceLoc.vue";
import DiagnoseContradiction from "../components/audit/DiagnoseContradiction.vue";
import ReportContradiction from "../components/audit/ReportContradiction.vue";
import {
  contradictionKey,
  summarizeBatches,
} from "../utils/contradictionReport";

type Contradiction = AuditResult["contradictions"][number];
/** A subproblem that crashed the engine: logged by its sweep range, since it
 *  never returned a result. */
interface CrashedBatch {
  status: "Crashed";
  stop_reason: null;
  elapsed_ms: number;
  focus: [];
  range: [number, number];
}
type LogEntry = AuditBatch | CrashedBatch;

const prover = useProverStore();
const kb = useKBStore();
const boot = useBootStore();

/** The sweep and subproblem-size controls. Blank size fields fall back to
 *  the prover settings' selection budget. */
const sweep = reactive({
  scope: "",
  seed: 0,
  step: 0,
  count: 50,
  batch: 1,
  limit: 5,
  timeLimit: 10,
  budget: null as number | null,
  depth: null as number | null,
});

// Seed and position are remembered per knowledge base, so a later visit
// continues the same sweep. Per-viewer convenience only: the page works
// without storage.
const storageKey = computed(
  () =>
    "sumoAuditSweep:" +
    kb.constituents
      .map((c) => c.file)
      .sort()
      .join("|"),
);
function loadSweep() {
  try {
    const saved = JSON.parse(localStorage.getItem(storageKey.value) || "null");
    if (saved && typeof saved === "object") Object.assign(sweep, saved);
  } catch {
    /* storage unavailable or corrupt: keep defaults */
  }
}
function saveSweep() {
  try {
    localStorage.setItem(storageKey.value, JSON.stringify(sweep));
  } catch {
    /* storage unavailable */
  }
}
watch(storageKey, loadSweep, { immediate: true });

const auditing = ref(false);
const stopRequested = ref(false);
const { label: elapsedLabel, lastLabel: tookLabel } = useElapsed(auditing);
const error = ref("");
const reportOpen = ref(false);
const reportLoading = ref(false);
const report = shallowRef<Awaited<ReturnType<typeof latestAuditReport>> | null>(
  null,
);
const reportError = ref("");
const replayBusy = ref(false);
const replayMode = ref(false);
const replayDone = ref(0);
const replayMessage = ref("");

onBeforeRouteLeave(() => !replayBusy.value);

async function showMasterReport() {
  reportOpen.value = true;
  reportLoading.value = true;
  report.value = null;
  reportError.value = "";
  replayMessage.value = "";
  try {
    report.value = await latestAuditReport();
  } catch (e) {
    reportError.value = errMsg(e);
  } finally {
    reportLoading.value = false;
  }
}

async function confirmReplay() {
  if (
    !report.value ||
    report.value.unavailable ||
    replayBusy.value ||
    auditing.value
  )
    return;
  replayBusy.value = true;
  reportError.value = "";
  try {
    const replay = report.value.replay;
    await loadAuditReplay(replay, (message) => {
      replayMessage.value = message;
    });
    prover.reset();
    const { backend, ...settings } = replay.config;
    prover.backend = backend;
    Object.assign(prover.cfg, settings);
    Object.assign(sweep, {
      scope: "",
      batch: 1,
      count: 1,
      budget: null,
      depth: null,
      timeLimit: replay.config.timeLimitSecs,
      limit: replay.request.limit,
    });
    await runAudit(replay);
    await checkAuditReplay(replay);
  } catch (e) {
    reportError.value = errMsg(e);
    replayMessage.value = "Replay could not be completed or verified.";
  } finally {
    replayBusy.value = false;
  }
}

const backendLabel = ref("");
const contradictions = shallowRef<Contradiction[]>([]);
const batches = shallowRef<LogEntry[]>([]);
const rawLines = shallowRef<string[]>([]);
const run = reactive({ seed: 0, start: 0, next: 0, total: 0, planned: 0 });

const checked = computed(() =>
  replayMode.value ? replayDone.value : run.next - run.start,
);
const progress = computed(() =>
  run.planned > 0 ? checked.value / run.planned : null,
);
const summary = computed(() => summarizeBatches(batches.value));
const status = computed(() =>
  contradictions.value.length || summary.value.contradictory
    ? "Inconsistent"
    : "Unknown",
);
const verdict = computed(() => {
  const s = summary.value;
  const n = contradictions.value.length;
  const problems = `${s.total} subproblem${s.total === 1 ? "" : "s"}`;
  if (n)
    return `${n} distinct contradiction${n === 1 ? "" : "s"} found in ${problems}.`;
  if (s.contradictory)
    return "Contradiction found — see raw engine output for the derivation.";
  const parts = [`${s.clean} saturated clean`];
  if (s.timeLimit) parts.push(`${s.timeLimit} hit the time limit`);
  if (s.stepLimit) parts.push(`${s.stepLimit} hit the step limit`);
  if (s.crashed) parts.push(`${s.crashed} crashed the engine`);
  if (s.other) parts.push(`${s.other} stopped without a verdict`);
  return `No contradiction found in ${problems} (${parts.join(", ")}). This doesn't prove the KB consistent.`;
});
const positionText = computed(() => {
  if (replayMode.value)
    return `Replayed ${replayDone.value} of ${run.planned} reported subproblems (intermediate steps skipped)`;
  const steps = `Seed ${run.seed} · steps ${run.start}–${run.next}`;
  if (!run.total) return steps;
  return (
    `${steps} of ${run.total.toLocaleString()}` +
    (run.next < run.total ? ` · next step ${run.next}` : " · sweep complete")
  );
});
const busyLabel = computed(
  () =>
    `Auditing… ${checked.value} / ${run.planned || "?"} · ${elapsedLabel(0)}`,
);

const heading = (c: Contradiction, i: number) =>
  `Contradiction #${i + 1} — ${c.steps.length} step${c.steps.length === 1 ? "" : "s"}`;

const num = (v: unknown, dflt: number, min = 0) => {
  const n = Math.floor(Number(v));
  return Number.isFinite(n) && n >= min ? n : dflt;
};

/** Run the sweep one subproblem per worker call, so progress shows, Stop
 *  takes effect between subproblems, and the worker stays responsive to the
 *  editor in between. */
async function runAudit(replay?: AuditReplay) {
  // Audits get their own per-subproblem time limit: a browser subproblem can
  // need several GB, and the wasm heap stops at 4 GB.
  const config =
    replay?.config ??
    prover.config({ timeLimitSecs: num(sweep.timeLimit, 10) });
  const positions = replay ? replayPositions(replay) : [];
  replayMode.value = !!replay;
  replayDone.value = 0;
  const seed = num(sweep.seed, 0);
  const start = num(sweep.step, 0);
  const count = replay ? positions.length : num(sweep.count, 50, 1);
  const batch = num(sweep.batch, 1, 1);
  const limit = replay ? Number.MAX_SAFE_INTEGER : num(sweep.limit, 5, 1);
  const extra = {
    ...(sweep.budget ? { budget: num(sweep.budget, 1, 1) } : {}),
    ...(sweep.depth ? { depth: num(sweep.depth, 1, 1) } : {}),
    ...(sweep.scope ? { scope: sweep.scope } : {}),
  };

  backendLabel.value = prover.vampireSelected ? "Vampire" : "SUPr";
  contradictions.value = [];
  batches.value = [];
  rawLines.value = [];
  Object.assign(run, { seed, start, next: start, total: 0, planned: count });
  error.value = "";
  stopRequested.value = false;
  auditing.value = true;
  const seen = new Set<string>();
  let crashes = 0;
  try {
    while (
      !stopRequested.value &&
      checked.value < run.planned &&
      contradictions.value.length < limit
    ) {
      const take = replay ? 1 : Math.min(batch, run.planned - checked.value);
      const position = replay
        ? positions[replayDone.value]
        : { seed, step: run.next };
      if (replay)
        replayMessage.value = `Replaying ${replayDone.value + 1}/${positions.length}: seed ${position.seed}, step ${position.step}`;
      const started = performance.now();
      let result: AuditResult;
      try {
        ({ result } = await call("audit", {
          config,
          request: {
            seed: position.seed,
            step: position.step,
            count: take,
            batch,
            limit: replay
              ? replay.request.limit
              : limit - contradictions.value.length,
            ...extra,
          },
        }));
        crashes = 0;
      } catch (e) {
        if (replay) throw e;
        if (!isWasmAbort(errMsg(e)) || ++crashes >= 3) throw e;
        // The engine restarts itself on a trap; skip the subproblem that
        // caused it (it would only crash again) and carry on.
        batches.value = [
          ...batches.value,
          {
            status: "Crashed",
            stop_reason: null,
            elapsed_ms: Math.round(performance.now() - started),
            focus: [],
            range: [run.next, run.next + take],
          },
        ];
        await boot.recoverWorker();
        run.next += take;
        sweep.step = run.next;
        saveSweep();
        continue;
      }
      run.total = result.total;
      if (!replay)
        run.planned = Math.min(count, Math.max(0, result.total - start));
      if (replay && result.next_step !== position.step + 1)
        throw new Error(
          `Replay step ${position.step} was not checked by this engine.`,
        );
      const fresh = result.contradictions.filter((c) => {
        const key = contradictionKey(c.steps);
        if (seen.has(key)) return false;
        seen.add(key);
        return true;
      });
      if (fresh.length)
        contradictions.value = [...contradictions.value, ...fresh];
      batches.value = [...batches.value, ...result.batches];
      rawLines.value = [...rawLines.value, result.raw_output];
      if (replay) {
        replayDone.value++;
        run.next = result.next_step;
        continue;
      }
      if (result.next_step <= run.next) break;
      run.next = result.next_step;
      // Saved per subproblem, so leaving mid-run still resumes here.
      sweep.seed = seed;
      sweep.step = run.next;
      saveSweep();
    }
    if (replay) {
      const expected = new Set(
        replay.findings.map((f) => replayAxiomKey(f.axioms)),
      );
      const actual = new Set(
        contradictions.value.map((c) =>
          replayAxiomKey(
            c.steps
              .filter((s) => s.file != null)
              .map((s) => ({
                file: s.file ?? null,
                line: s.line ?? null,
                kif: s.kif,
              })),
          ),
        ),
      );
      const missing = [...expected].filter((key) => !actual.has(key)).length;
      const extra = [...actual].filter((key) => !expected.has(key)).length;
      replayMessage.value = stopRequested.value
        ? `Replay stopped after ${replayDone.value}/${positions.length} reported subproblems.`
        : missing || extra
          ? `Replay differs from the workflow: ${missing} reported contradiction(s) missing, ${extra} additional. See the subproblem log for time or step limits.`
          : `Reproduced all ${expected.size} reported contradiction(s) in ${positions.length} subproblems.`;
    }
  } catch (e) {
    error.value = isWasmAbort(errMsg(e))
      ? "Three subproblems in a row crashed the engine, most likely by running out of memory (a subproblem can need several GB; the browser allows 4 GB). Lower the time limit or the subproblem size and continue from the saved step."
      : errMsg(e);
    if (replay) replayMessage.value = `Replay failed: ${error.value}`;
  } finally {
    auditing.value = false;
  }
}

function randomSeed() {
  sweep.seed = Math.floor(Math.random() * 2 ** 31);
  sweep.step = 0;
}
</script>

<template>
  <div>
    <Card>
      <div class="inline">
        <button
          class="btn ghost"
          type="button"
          :disabled="auditing || replayBusy"
          @click="showMasterReport"
        >
          ⓘ Latest master contradiction report
        </button>
        <span class="hint"
          >Replay the reported steps against the workflow's verified Full SUMO
          inputs.</span
        >
      </div>
      <p v-if="replayMessage" role="status">{{ replayMessage }}</p>
    </Card>
    <BaseDialog
      v-model="reportOpen"
      title="Latest master contradiction report"
      width="min(800px, 94vw)"
      :dismissible="!replayBusy"
    >
      <p v-if="reportLoading" role="status">
        Fetching the latest completed master audit...
      </p>
      <p v-if="reportError" class="hint bad" role="alert">{{ reportError }}</p>
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
        <p v-else class="hint bad">
          Continuing will replace your loaded knowledge base and prover settings
          with the workflow's exact inputs. This may overwrite work you have
          saved, including local edits to loaded or audited constituents. Save a
          separate copy of any work you want to keep. Confirm to synchronize and
          run only the reported contradiction steps.
        </p>
        <Disclosure summary="Read contradictions.md">
          <pre class="report-text">{{
            report.markdown.split("## Replay metadata")[0]
          }}</pre>
        </Disclosure>
      </template>
      <p v-if="replayMessage" role="status">{{ replayMessage }}</p>
      <template #actions>
        <button
          class="btn ghost"
          type="button"
          :disabled="replayBusy"
          @click="reportOpen = false"
        >
          Close
        </button>
        <button
          v-if="replayBusy && auditing"
          class="btn ghost"
          type="button"
          :disabled="stopRequested"
          @click="stopRequested = true"
        >
          Stop after this subproblem
        </button>
        <button
          class="btn"
          type="button"
          :disabled="
            reportLoading ||
            replayBusy ||
            !report ||
            !!report.unavailable ||
            !report.replay.findings.length
          "
          @click="confirmReplay"
        >
          {{ replayBusy ? "Replaying..." : "Confirm: replace work and replay" }}
        </button>
      </template>
    </BaseDialog>
    <Card>
      <div class="hint">
        Samples the loaded KB: picks axioms in a seeded pseudorandom order and
        checks each one's neighbourhood (the axioms selected for it, as for a
        query) for a contradiction. Any contradiction found is real; finding
        none doesn't prove the KB consistent.
      </div>
      <div class="sweep-grid mt">
        <div>
          <label for="auditScope">scope</label>
          <select id="auditScope" v-model="sweep.scope" :disabled="auditing">
            <option value="">Whole KB</option>
            <option v-for="c in kb.constituents" :key="c.file" :value="c.file">
              {{ c.name }}
            </option>
          </select>
        </div>
        <div>
          <label for="auditSeed">seed</label>
          <div class="seed-row">
            <input
              id="auditSeed"
              v-model.number="sweep.seed"
              type="number"
              min="0"
              :disabled="auditing"
            />
            <button
              class="btn ghost small-btn"
              type="button"
              title="New random seed (restarts at step 0)"
              aria-label="New random seed"
              :disabled="auditing"
              @click="randomSeed"
            >
              ↻
            </button>
          </div>
        </div>
        <div>
          <label for="auditStep">start at step</label>
          <input
            id="auditStep"
            v-model.number="sweep.step"
            type="number"
            min="0"
            :disabled="auditing"
          />
        </div>
        <div>
          <label for="auditCount">axioms to check</label>
          <input
            id="auditCount"
            v-model.number="sweep.count"
            type="number"
            min="1"
            :disabled="auditing"
          />
        </div>
        <div>
          <label for="auditBatch">axioms per subproblem</label>
          <input
            id="auditBatch"
            v-model.number="sweep.batch"
            type="number"
            min="1"
            :disabled="auditing"
          />
        </div>
        <div>
          <label for="auditBudget">subproblem size (max axioms)</label>
          <input
            id="auditBudget"
            v-model.number="sweep.budget"
            type="number"
            min="1"
            placeholder="prover setting"
            :disabled="auditing"
          />
        </div>
        <div>
          <label for="auditDepth">selection depth</label>
          <input
            id="auditDepth"
            v-model.number="sweep.depth"
            type="number"
            min="1"
            placeholder="unlimited"
            :disabled="auditing"
          />
        </div>
        <div>
          <label for="auditTime">time limit per subproblem (s)</label>
          <input
            id="auditTime"
            v-model.number="sweep.timeLimit"
            type="number"
            min="0"
            :disabled="auditing"
          />
        </div>
        <div>
          <label for="auditLimit">stop after contradictions</label>
          <input
            id="auditLimit"
            v-model.number="sweep.limit"
            type="number"
            min="1"
            :disabled="auditing"
          />
        </div>
      </div>
      <div class="inline mt">
        <BusyButton
          :busy="auditing"
          label="Run audit"
          :busy-label="busyLabel"
          :progress="progress"
          @click="runAudit()"
        />
        <button
          v-if="auditing"
          class="btn ghost"
          type="button"
          :disabled="stopRequested"
          title="Stop after the current subproblem"
          @click="stopRequested = true"
        >
          {{ stopRequested ? "Stopping…" : "Stop" }}
        </button>
        <button
          class="cog"
          type="button"
          title="Prover settings (backend, step cap, selection budget)"
          aria-label="Prover settings"
          :aria-expanded="prover.settingsOpen"
          @click="prover.toggleSettings()"
        >
          ⚙
        </button>
      </div>
    </Card>
    <ProverSettings />
    <div>
      <Card v-if="error" class="hint bad">{{ error }}</Card>
      <template v-if="batches.length">
        <Card>
          <div class="inline">
            <span :class="`audit-status ${status}`">{{ status }}</span>
            <span class="hint">via {{ backendLabel }}</span>
            <span
              v-if="tookLabel && !auditing"
              class="hint"
              title="Wall-clock time"
              >{{ tookLabel }}</span
            >
          </div>
          <div class="hint verdict">{{ verdict }}</div>
          <div class="hint position">{{ positionText }}</div>
          <Disclosure :summary="`subproblem log (${batches.length})`">
            <ol class="batch-log">
              <li v-for="(b, i) in batches" :key="i">
                <span :class="`audit-status small ${b.status}`">{{
                  b.status === "Consistent" ? "clean" : b.status
                }}</span>
                <span class="hint">{{ b.elapsed_ms }} ms</span>
                <span v-if="b.status === 'Crashed'" class="hint"
                  >steps {{ b.range[0] }}–{{ b.range[1] }} (skipped)</span
                >
                <template v-for="(f, j) in b.focus" :key="j">
                  <SourceLoc
                    v-if="f.file"
                    :file="f.file"
                    :line="f.line"
                    variant="loc"
                  />
                  <span v-else class="hint">(no location)</span>
                </template>
              </li>
            </ol>
          </Disclosure>
          <Disclosure summary="raw engine output">
            <pre>{{ rawLines.join("\n") || "(none)" }}</pre>
          </Disclosure>
        </Card>
        <Card v-for="(c, i) in contradictions" :key="contradictionKey(c.steps)">
          <div class="contradiction-hd">{{ heading(c, i) }}</div>
          <ProofView
            :steps="c.steps"
            :prologue="c.proof_tptp_prologue"
            :prose="c.prose"
            :prose-missing="c.prose_missing"
            :graphviz="c.graphviz"
          />
          <div class="contradiction-actions">
            <DiagnoseContradiction :steps="c.steps" />
            <ReportContradiction
              :steps="c.steps"
              :prose="c.prose"
              :backend="backendLabel"
            />
          </div>
        </Card>
      </template>
    </div>
  </div>
</template>

<style scoped>
.report-text {
  max-height: 45vh;
  overflow: auto;
  white-space: pre-wrap;
}
.sweep-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(170px, 1fr));
  gap: 10px 14px;
}
.sweep-grid label {
  display: block;
  font-size: 12px;
  color: var(--muted);
  margin-bottom: 4px;
}
.sweep-grid select {
  width: 100%;
}
.seed-row {
  display: flex;
  gap: 6px;
}
button.small-btn {
  height: auto;
  padding: 0 10px;
}
.verdict {
  margin-top: 8px;
}
.position {
  margin-top: 4px;
  font-variant-numeric: tabular-nums;
}
.batch-log {
  margin: 6px 0;
  padding-left: 22px;
  max-height: 260px;
  overflow: auto;
}
.batch-log li {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  align-items: baseline;
  padding: 2px 0;
}
.audit-status.small {
  font-size: 11px;
  padding: 1px 7px;
}
.contradiction-actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  margin-top: 10px;
}
.contradiction-hd {
  font-weight: 600;
  margin-bottom: 6px;
}
</style>
