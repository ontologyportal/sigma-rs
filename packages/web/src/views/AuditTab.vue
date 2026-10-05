<script setup lang="ts">
import { computed, onActivated, reactive, ref, shallowRef, watch } from "vue";
import { onBeforeRouteLeave } from "vue-router";
import type { AuditBatch, AuditFocus, AuditResult } from "sigmakee/sdk";
import { call, isWasmAbort } from "../services/sigma";
import { errMsg, fmtNum } from "../utils/format";
import { useBootStore } from "../stores/boot";
import { useKBStore } from "../stores/kb";
import { useProverStore } from "../stores/prover";
import { useTabQuery } from "../composables/useTabQuery";
import { fmtSecs, useElapsed } from "../composables/useElapsed";
import { updateParams } from "../router";
import BusyButton from "../components/BusyButton.vue";
import BaseDialog from "../components/BaseDialog.vue";
import Card from "../components/Card.vue";
import {
  checkAuditReplay,
  latestAuditReport,
  loadAuditReplay,
} from "../services/audit-replay";
import {
  replayAxiomKey,
  replayPositions,
  type AuditReplay,
} from "../utils/auditReplay";
import Disclosure from "../components/Disclosure.vue";
import ProofView from "../components/ProofView.vue";
import ProverChips from "../components/ProverChips.vue";
import ProverOptions from "../components/ProverOptions.vue";
import Segmented from "../components/Segmented.vue";
import SourceLoc from "../components/SourceLoc.vue";
import DiagnoseContradiction from "../components/audit/DiagnoseContradiction.vue";
import ReportContradiction from "../components/audit/ReportContradiction.vue";
import {
  contradictionKey,
  summarizeBatches,
} from "../utils/contradictionReport";
import {
  FOCUS_BUDGETS,
  TOTAL_PRESETS,
  focusRoundSecs,
  minChecks,
  nextCheckSecs,
  planAudit,
  type AuditPlan,
} from "../utils/auditPlan";

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
/** A logged subproblem; `budget` is set on a focused audit's rounds. */
type LogEntry = (AuditBatch | CrashedBatch) & { budget?: number };

const prover = useProverStore();
const kb = useKBStore();
const boot = useBootStore();

// -- settings ------------------------------------------------------------------

/** The audit form. While `linked`, the per-check fields follow the plan
 *  derived from `totalSecs`; editing one of them in Advanced unlinks it. */
const sweep = reactive({
  mode: "simple" as "simple" | "advanced",
  scope: "",
  totalSecs: 300,
  linked: true,
  perCheckSecs: 10,
  /** Axioms to check; null = until the time is up. */
  count: null as number | null,
  batch: 1,
  limit: 5,
  seed: 0,
  step: 0,
  /** Sweep size seen on the last run, for the Continue label. */
  total: 0,
});

// Remembered per knowledge base, so a later visit continues the same sweep.
// Per-viewer convenience only: the page works without storage.
const storageKey = computed(
  () =>
    "sumoAuditSweep:" +
    kb.constituents
      .map((c) => c.file)
      .sort()
      .join("|"),
);
function loadSweep() {
  // The position belongs to one KB's sweep order; a KB with nothing saved
  // starts its own sweep rather than inheriting another's.
  Object.assign(sweep, { scope: "", seed: 0, step: 0, total: 0 });
  try {
    const saved = JSON.parse(localStorage.getItem(storageKey.value) || "null");
    if (saved && typeof saved === "object")
      for (const k of Object.keys(sweep) as (keyof typeof sweep)[])
        if (k in saved && typeof saved[k] === typeof sweep[k])
          (sweep as Record<string, unknown>)[k] = saved[k];
    // `count` is nullable, so typeof can't vet it above.
    if (saved && (saved.count === null || typeof saved.count === "number"))
      sweep.count = saved.count;
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
watch(() => [sweep.mode, sweep.totalSecs, sweep.linked], saveSweep);

const derived = computed(() => planAudit(sweep.totalSecs));
/** The plan a run uses: derived from the total while linked, else the
 *  Advanced fields. */
const plan = computed<AuditPlan>(() =>
  sweep.linked
    ? derived.value
    : {
        totalSecs: Math.max(0, Math.floor(sweep.totalSecs) || 0),
        perCheckSecs: Math.max(0, Math.floor(sweep.perCheckSecs) || 0),
        count: sweep.count && sweep.count > 0 ? Math.floor(sweep.count) : null,
        batch: Math.max(1, Math.floor(sweep.batch) || 1),
        limit: Math.max(1, Math.floor(sweep.limit) || 1),
      },
);

type PlanField = "perCheckSecs" | "count" | "batch" | "limit";
/** A v-model for one Advanced plan field: shows the derived value while
 *  linked; the first edit copies the plan in and unlinks. */
function planField(key: PlanField) {
  return computed({
    get: () => plan.value[key] ?? "",
    set: (v: number | string) => {
      if (sweep.linked) {
        Object.assign(sweep, derived.value);
        sweep.linked = false;
      }
      (sweep as Record<string, unknown>)[key] =
        v === "" || v === null ? (key === "count" ? null : 0) : Number(v);
      saveSweep();
    },
  });
}
const perCheckField = planField("perCheckSecs");
const countField = planField("count");
const batchField = planField("batch");
const limitField = planField("limit");

function relink() {
  sweep.linked = true;
}

const totalMinutes = computed({
  get: () => Math.round((sweep.totalSecs / 60) * 10) / 10,
  set: (m: number) => {
    sweep.totalSecs = Math.max(0, Math.round(Number(m) * 60) || 0);
  },
});
const presetTotal = computed({
  get: () =>
    TOTAL_PRESETS.some((p) => p.secs === sweep.totalSecs)
      ? String(sweep.totalSecs)
      : "custom",
  set: (v: string) => {
    if (v !== "custom") sweep.totalSecs = Number(v);
    else if (TOTAL_PRESETS.some((p) => p.secs === sweep.totalSecs))
      sweep.totalSecs += 60;
  },
});
const totalOptions = [
  ...TOTAL_PRESETS.map((p) => ({ value: String(p.secs), label: p.label })),
  { value: "custom", label: "Custom" },
];

const optionsOpen = ref(false);

function onScopeChange() {
  sweep.step = 0;
  sweep.total = 0;
  saveSweep();
}

function randomSeed() {
  sweep.seed = Math.floor(Math.random() * 2 ** 31);
  sweep.step = 0;
  sweep.total = 0;
  saveSweep();
}

// -- focus (right-click "Audit this sentence" in the editor) ------------------

interface Focus {
  file: string;
  offset: number;
  sentence: AuditFocus;
}
const focus = ref<Focus | null>(null);
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

// -- run -------------------------------------------------------------------------

const auditing = ref(false);
const stopRequested = ref(false);
const {
  label: elapsedLabel,
  fraction: elapsedFraction,
  lastSecs,
} = useElapsed(auditing);
const error = ref("");

const backendLabel = ref("");
const contradictions = shallowRef<Contradiction[]>([]);
const batches = shallowRef<LogEntry[]>([]);
const rawLines = shallowRef<string[]>([]);
const run = reactive({
  focused: false,
  seed: 0,
  start: 0,
  next: 0,
  total: 0,
  planned: 0 as number | null,
  totalSecs: 0,
  perCheckSecs: 0,
  widest: 0,
});

const checked = computed(() => batches.value.length);
const progress = computed(() => {
  if (run.totalSecs) return elapsedFraction(run.totalSecs);
  return run.planned ? Math.min(1, checked.value / run.planned) : null;
});
const summary = computed(() => summarizeBatches(batches.value));
const found = computed(() => contradictions.value.length);

const busyLabel = computed(() => {
  const n = `${checked.value} checked`;
  return `Auditing… ${n} · ${elapsedLabel(run.totalSecs)}`;
});

/** The saved position has reached the end of its sweep. */
const sweepDone = computed(() => sweep.total > 0 && sweep.step >= sweep.total);
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

const planText = computed(() => {
  const p = plan.value;
  const each = p.perCheckSecs
    ? `up to ${fmtSecs(p.perCheckSecs)} each`
    : "no limit each";
  const until = p.totalSecs
    ? `for ${fmtSecs(p.totalSecs)}`
    : "with no overall limit";
  const stop = `or until ${p.limit} contradiction${p.limit === 1 ? "" : "s"}`;
  if (focus.value)
    return p.totalSecs
      ? `Checks this sentence's neighbourhood at up to ${FOCUS_BUDGETS.join(", ")} axioms, sharing ${fmtSecs(p.totalSecs)}, and stops at the first contradiction.`
      : `Checks this sentence's neighbourhood at up to ${FOCUS_BUDGETS.join(", ")} axioms, ${each}, and stops at the first contradiction.`;
  const n = minChecks(p);
  const reach =
    n == null
      ? ""
      : p.count != null && p.count <= n
        ? ` Covers ${fmtNum(p.count)} axiom${p.count === 1 ? "" : "s"}.`
        : ` Covers at least ${fmtNum(n)} axioms; most checks finish well under their limit.`;
  return `Checks one axiom's neighbourhood at a time, ${each}, ${until} ${stop}.${reach}`;
});

/** Clear the last run's results before a new one. */
function clearResults() {
  backendLabel.value = prover.backendLabel;
  collapsed.value = new Set();
  contradictions.value = [];
  batches.value = [];
  rawLines.value = [];
  error.value = "";
  stopRequested.value = false;
  replayMode.value = false;
  lastRunRecheck.value = false;
}

function reset(focused: boolean) {
  clearResults();
  run.focused = focused;
  run.totalSecs = plan.value.totalSecs;
  run.perCheckSecs = plan.value.perCheckSecs;
  run.widest = 0;
}

/** Add `result`'s subproblems and new contradictions to the log. */
function record(result: AuditResult, seen: Set<string>, budget?: number) {
  const fresh = result.contradictions.filter((c) => {
    const key = contradictionKey(c.steps);
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
  if (fresh.length) contradictions.value = [...contradictions.value, ...fresh];
  batches.value = [
    ...batches.value,
    ...result.batches.map((b) => (budget ? { ...b, budget } : b)),
  ];
  rawLines.value = [...rawLines.value, result.raw_output];
}

function crashMessage(e: unknown): string {
  return isWasmAbort(errMsg(e))
    ? "Three checks in a row crashed the engine, most likely by running out of memory (a check can need several GB; the browser allows 4 GB). Lower the time per check or the axiom selection and continue from the saved step."
    : errMsg(e);
}

function start(fresh: boolean) {
  // A finished sweep has nothing left to resume: run a new one.
  if (fresh || (!focus.value && sweepDone.value)) randomSeed();
  return focus.value ? runFocused(focus.value) : runSweep();
}

/** Sweep one check per worker call, so progress shows, Stop takes effect
 *  between checks, and the worker stays responsive to the editor. */
async function runSweep() {
  reset(false);
  const p = plan.value;
  const seed = Math.max(0, Math.floor(sweep.seed) || 0);
  const begin = Math.max(0, Math.floor(sweep.step) || 0);
  Object.assign(run, {
    seed,
    start: begin,
    next: begin,
    total: 0,
    planned: p.count,
  });
  auditing.value = true;
  const deadline = p.totalSecs ? performance.now() + p.totalSecs * 1000 : null;
  const seen = new Set<string>();
  let crashes = 0;
  try {
    await prover.loadDefaults();
    while (
      !stopRequested.value &&
      found.value < p.limit &&
      (p.count == null || run.next - begin < p.count)
    ) {
      const secs = nextCheckSecs(
        p.perCheckSecs,
        deadline == null ? null : deadline - performance.now(),
      );
      if (deadline != null && secs === 0) break;
      const config = prover.config("audit", { timeLimitSecs: secs });
      const take =
        p.count == null
          ? p.batch
          : Math.min(p.batch, p.count - (run.next - begin));
      const started = performance.now();
      let result: AuditResult;
      try {
        ({ result } = await call("audit", {
          config,
          request: {
            seed,
            step: run.next,
            count: take,
            batch: p.batch,
            limit: p.limit - found.value,
            ...(sweep.scope ? { scope: sweep.scope } : {}),
          },
        }));
        crashes = 0;
      } catch (e) {
        if (!isWasmAbort(errMsg(e)) || ++crashes >= 3) throw e;
        // The engine restarts itself on a trap; skip the check that caused
        // it (it would only crash again) and carry on.
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
      record(result, seen);
      if (result.next_step <= run.next) break;
      run.next = result.next_step;
      // Saved per check, so leaving mid-run still resumes here.
      sweep.seed = seed;
      sweep.step = run.next;
      sweep.total = result.total;
      saveSweep();
    }
  } catch (e) {
    error.value = crashMessage(e);
  } finally {
    auditing.value = false;
  }
}

/** Audit one sentence: check its neighbourhood at each of FOCUS_BUDGETS in
 *  turn, sharing the total time, until a contradiction turns up. */
async function runFocused(f: Focus) {
  reset(true);
  const p = plan.value;
  auditing.value = true;
  const deadline = p.totalSecs ? performance.now() + p.totalSecs * 1000 : null;
  const seen = new Set<string>();
  try {
    await prover.loadDefaults();
    for (const [i, budget] of FOCUS_BUDGETS.entries()) {
      if (stopRequested.value || found.value) break;
      const secs =
        deadline == null
          ? p.perCheckSecs
          : focusRoundSecs(
              deadline - performance.now(),
              FOCUS_BUDGETS.length - i,
            );
      if (deadline != null && secs === 0) break;
      const { result } = await call("audit", {
        config: prover.config("audit", { timeLimitSecs: secs }),
        request: {
          at: { file: f.file, offset: f.offset },
          count: 1,
          batch: 1,
          limit: p.limit,
          budget,
        },
      });
      record(result, seen, budget);
      run.widest = budget;
    }
  } catch (e) {
    if (isWasmAbort(errMsg(e))) await boot.recoverWorker();
    error.value = crashMessage(e);
  } finally {
    auditing.value = false;
  }
}

// -- nightly report replay + recheck ----------------------------------------

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
const loadedReplay = shallowRef<AuditReplay | null>(null);
const recheckReason = ref("");
const recheckRevision = ref(0);
const rechecking = ref(false);
const lastRunRecheck = ref(false);

async function refreshRecheck() {
  if (!loadedReplay.value || replayBusy.value || auditing.value) return;
  try {
    const state = await call("auditRecheckStatus");
    recheckReason.value = state.reason;
    recheckRevision.value = state.revision;
  } catch (e) {
    recheckReason.value = errMsg(e);
  }
}
onActivated(refreshRecheck);

async function recheckContradictions() {
  if (!loadedReplay.value || replayBusy.value || auditing.value) return;
  replayBusy.value = true;
  try {
    const state = await call("auditRecheckStatus");
    recheckReason.value = state.reason;
    recheckRevision.value = state.revision;
    if (state.reason) return;
    rechecking.value = true;
    await runReplay(loadedReplay.value);
  } catch (e) {
    recheckReason.value = errMsg(e);
  } finally {
    rechecking.value = false;
    replayBusy.value = false;
    await refreshRecheck();
  }
}

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
    loadedReplay.value = null;
    await loadAuditReplay(replay, (message) => {
      replayMessage.value = message;
    });
    loadedReplay.value = replay;
    try {
      const state = await call("prepareAuditRecheck", {
        files: kb.constituents.map((c) => ({ name: c.file, text: c.text })),
        positions: replayPositions(replay),
        config: replay.config,
        limit: replay.request.limit,
      });
      recheckReason.value = state.reason;
      recheckRevision.value = state.revision;
    } catch (e) {
      recheckReason.value = errMsg(e);
    }
    prover.adoptConfig("audit", replay.config);
    sweep.scope = "";
    await runReplay(replay);
    await checkAuditReplay(replay);
  } catch (e) {
    reportError.value = errMsg(e);
    replayMessage.value = "Replay could not be completed or verified.";
  } finally {
    replayBusy.value = false;
  }
}

/** Replay a report's subproblems with its exact settings -- or, while
 *  `rechecking`, recheck its tracked targets against local edits -- one
 *  worker call each, verifying each landed on the reported position. */
async function runReplay(replay: AuditReplay) {
  clearResults();
  const positions = replayPositions(replay);
  replayMode.value = true;
  lastRunRecheck.value = rechecking.value;
  replayDone.value = 0;
  backendLabel.value = "SUPr";
  Object.assign(run, {
    focused: false,
    seed: 0,
    start: 0,
    next: 0,
    total: 0,
    planned: positions.length,
    totalSecs: 0,
    perCheckSecs: replay.config.timeLimitSecs ?? 0,
    widest: 0,
  });
  auditing.value = true;
  const seen = new Set<string>();
  try {
    while (!stopRequested.value && replayDone.value < positions.length) {
      const position = positions[replayDone.value];
      replayMessage.value = `${rechecking.value ? "Rechecking target" : "Replaying"} ${replayDone.value + 1}/${positions.length}: original seed ${position.seed}, step ${position.step}`;
      const { result } = rechecking.value
        ? await call("recheckAudit", {
            index: replayDone.value,
            revision: recheckRevision.value,
          })
        : await call("audit", {
            config: replay.config,
            request: {
              seed: position.seed,
              step: position.step,
              count: 1,
              batch: 1,
              limit: replay.request.limit,
            },
          });
      if (result.next_step !== (rechecking.value ? 1 : position.step + 1))
        throw new Error(
          `Replay step ${position.step} was not checked by this engine.`,
        );
      run.total = result.total;
      record(result, seen);
      replayDone.value++;
      run.next = result.next_step;
    }
    replayMessage.value = rechecking.value
      ? recheckSummary(positions.length)
      : replaySummary(replay, positions.length);
  } catch (e) {
    error.value = crashMessage(e);
    replayMessage.value = `Replay failed: ${error.value}`;
  } finally {
    auditing.value = false;
  }
}

function recheckSummary(total: number): string {
  const s = summarizeBatches(batches.value);
  const inconclusive = s.timeLimit + s.stepLimit + s.crashed + s.other;
  return stopRequested.value
    ? `Recheck stopped after ${replayDone.value}/${total} targets; remaining targets were not checked.`
    : `Rechecked ${replayDone.value} reported targets against your edits: ${s.contradictory} still produced contradictions, ${s.clean} no longer reproduced a contradiction, ${inconclusive} inconclusive. This does not prove the entire knowledge base consistent.`;
}

function replaySummary(replay: AuditReplay, total: number): string {
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
  if (stopRequested.value)
    return `Replay stopped after ${replayDone.value}/${total} reported subproblems.`;
  return missing || extra
    ? `Replay differs from the workflow: ${missing} reported contradiction(s) missing, ${extra} additional. See the check log for time or step limits.`
    : `Reproduced all ${expected.size} reported contradiction(s) in ${total} subproblems.`;
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

const breakdown = computed(() => {
  const s = summary.value;
  const parts = [`${s.total} check${s.total === 1 ? "" : "s"}`];
  if (s.clean) parts.push(`${s.clean} clean`);
  if (s.contradictory) parts.push(`${s.contradictory} contradictory`);
  if (s.timeLimit) parts.push(`${s.timeLimit} timed out`);
  if (s.stepLimit) parts.push(`${s.stepLimit} hit the step cap`);
  if (s.crashed) parts.push(`${s.crashed} crashed`);
  if (s.other) parts.push(`${s.other} gave up`);
  return parts.join(" · ");
});

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

function entryLabel(b: LogEntry): string {
  if (b.status === "Consistent") return "clean";
  if (b.status === "Inconsistent") return "contradiction";
  if (b.status === "Crashed") return "crashed";
  switch (b.stop_reason) {
    case "TimeLimit":
      return "timed out";
    case "StepLimit":
      return "step cap";
    case "IncompleteLoad":
      return "incomplete load";
    default:
      return "gave up";
  }
}
function entryClass(b: LogEntry): string {
  if (b.status === "Consistent") return "ok";
  if (b.status === "Inconsistent" || b.status === "Crashed") return "bad";
  return "warn";
}

const heading = (c: Contradiction, i: number) =>
  `Contradiction ${i + 1} · ${c.steps.length} step${c.steps.length === 1 ? "" : "s"}`;

/** Contradictions whose proof is folded away (by `contradictionKey`). */
const collapsed = ref(new Set<string>());
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

/** The step list's fold summary: the proof's size and what it cites. */
function proofSummary(c: Contradiction): string {
  const cited = citedSummary(c);
  return `proof (${c.steps.length} step${c.steps.length === 1 ? "" : "s"})${cited ? ` · cites ${cited}` : ""}`;
}

/** The source axioms a contradiction cites, for its steps' summary. */
function citedSummary(c: Contradiction): string {
  const locs = [
    ...new Set(
      c.steps.filter((s) => s.file).map((s) => `${s.file}:${s.line ?? "?"}`),
    ),
  ];
  if (!locs.length) return "";
  const shown = locs.slice(0, 3).join(", ");
  return locs.length > 3 ? `${shown} (+${locs.length - 3} more)` : shown;
}
</script>

<template>
  <div>
    <BaseDialog
      v-model="reportOpen"
      title="Latest Contradiction Report"
      width="min(800px, 94vw)"
      :dismissible="!replayBusy"
    >
      <p>
        SUMO runs a two-hour contradiction audit each night. This report records
        the contradictions found and the steps that produced them. Continue to
        synchronize your workspace with the report's verified master-branch
        inputs and replay only those steps. Reports cannot be loaded if master
        has changed since the audit ran.
      </p>
      <p>
        After replaying, edit existing formulas and use Recheck reported
        contradictions to test your changes without replacing your work. Adding
        or removing formulas, or changes that make a target ambiguous,
        invalidate rechecking. Keep this app session open while editing.
      </p>
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
        <template v-else>
          <p class="hint bad">
            WARNING: Continuing will replace your loaded knowledge base and
            prover settings with the workflow's exact inputs. This may overwrite
            work you have saved, including local edits to loaded or audited
            constituents. Save a separate copy of any work you want to keep.
          </p>
          <p>
            Confirm to synchronize with contradiction report and replay the
            reported contradiction steps.
          </p>
        </template>
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
            <option v-for="c in kb.constituents" :key="c.file" :value="c.file">
              {{ c.name }}
            </option>
          </select>
        </div>
        <div>
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
      <div v-if="sweep.mode === 'simple' && !sweep.linked" class="hint mt-sm">
        Using the custom per-check settings from Advanced.
        <button class="linkish" type="button" @click="relink">
          Derive them from the time instead
        </button>
      </div>

      <!-- Advanced: every knob, prefilled from the plan above. -->
      <template v-if="sweep.mode === 'advanced'">
        <div class="section-hd">
          <span>Checks</span>
          <span v-if="sweep.linked" class="hint"
            >derived from the total time</span
          >
          <button v-else class="linkish" type="button" @click="relink">
            Re-derive from the total time
          </button>
        </div>
        <div class="grid">
          <div>
            <label for="auditPerCheck">time per check (s)</label>
            <input
              id="auditPerCheck"
              v-model.number="perCheckField"
              type="number"
              min="0"
              :disabled="auditing"
            />
          </div>
          <div v-if="!focus">
            <label for="auditCount">axioms to check</label>
            <input
              id="auditCount"
              v-model.number="countField"
              type="number"
              min="1"
              placeholder="until time's up"
              :disabled="auditing"
            />
          </div>
          <div v-if="!focus">
            <label for="auditBatch">axioms per check</label>
            <input
              id="auditBatch"
              v-model.number="batchField"
              type="number"
              min="1"
              :disabled="auditing"
            />
          </div>
          <div>
            <label for="auditLimit">stop after contradictions</label>
            <input
              id="auditLimit"
              v-model.number="limitField"
              type="number"
              min="1"
              :disabled="auditing"
            />
          </div>
          <div>
            <label for="auditTotal">total time (s)</label>
            <input
              id="auditTotal"
              v-model.number="sweep.totalSecs"
              type="number"
              min="0"
              placeholder="0 = no limit"
              :disabled="auditing"
            />
          </div>
        </div>
        <template v-if="!focus">
          <div class="section-hd"><span>Sweep position</span></div>
          <div class="grid">
            <div>
              <label for="auditSeed">seed</label>
              <div class="seed-row">
                <input
                  id="auditSeed"
                  v-model.number="sweep.seed"
                  type="number"
                  min="0"
                  :disabled="auditing"
                  @change="saveSweep"
                />
                <button
                  class="btn ghost small"
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
                @change="saveSweep"
              />
            </div>
          </div>
          <div class="hint sub">
            The seed fixes a pseudorandom order of the axioms; the step is the
            position in it. Both are saved per KB, so a later run continues
            where the last one stopped.
          </div>
        </template>
      </template>

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
          v-if="sweep.mode === 'advanced'"
          v-model:open="optionsOpen"
          profile="audit"
          :disabled="auditing"
        />
        <span v-else class="hint">via {{ prover.backendLabel }}</span>
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
        v-if="sweep.mode === 'advanced' && optionsOpen"
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
        <Disclosure :summary="`check log (${batches.length})`">
          <ol class="batch-log">
            <li v-for="(b, i) in batches" :key="i">
              <span class="dot" :class="entryClass(b)" aria-hidden="true" />
              <span class="entry-status">{{ entryLabel(b) }}</span>
              <span class="hint num">{{ fmtSecs(b.elapsed_ms / 1000) }}</span>
              <span v-if="b.budget" class="hint"
                >≤ {{ fmtNum(b.budget) }} axioms</span
              >
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
          :steps-summary="proofSummary(c)"
          :steps-open="!isCollapsed(c)"
          @steps-toggle="setCollapsed(c, !$event)"
        />
        <div class="contradiction-actions">
          <DiagnoseContradiction :steps="c.steps" />
          <ReportContradiction
            :steps="c.steps"
            :prose="c.prose"
            :backend="backendLabel"
            :time-limit-secs="run.perCheckSecs"
          />
        </div>
      </Card>
    </template>
  </div>
</template>

<style scoped>
.report-text {
  max-height: 45vh;
  overflow: auto;
  white-space: pre-wrap;
}
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
.section-hd {
  display: flex;
  align-items: baseline;
  gap: 10px;
  margin-top: 16px;
  font-size: 12px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: var(--muted);
}
.section-hd .hint {
  text-transform: none;
  letter-spacing: 0;
  font-weight: 400;
}
.grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(170px, 1fr));
  gap: 10px 14px;
  margin-top: 8px;
}
.sub {
  font-size: 12px;
  margin-top: 6px;
}
.seed-row {
  display: flex;
  gap: 6px;
}
button.small {
  height: auto;
  padding: 4px 10px;
}
.linkish {
  font: inherit;
  font-size: 13px;
  background: none;
  border: none;
  padding: 0;
  color: var(--accent);
  cursor: pointer;
  text-transform: none;
  letter-spacing: 0;
  font-weight: 400;
}
.linkish:hover {
  text-decoration: underline;
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
.batch-log {
  margin: 6px 0;
  padding-left: 22px;
  max-height: 300px;
  overflow: auto;
}
.batch-log li {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  align-items: baseline;
  padding: 2px 0;
}
.dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  align-self: center;
  background: var(--muted);
}
.dot.ok {
  background: var(--ok);
}
.dot.warn {
  background: var(--warn);
}
.dot.bad {
  background: var(--bad);
}
.entry-status {
  font-size: 13px;
  min-width: 90px;
}
.num {
  font-variant-numeric: tabular-nums;
  min-width: 48px;
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
}
.fold-all {
  margin-left: auto;
}
</style>
