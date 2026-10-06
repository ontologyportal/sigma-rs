/**
 * The Audit tab's runs and their results: a sweep over the KB (one worker
 * call per round, so progress shows, Stop takes effect between rounds, and
 * the worker stays responsive to the editor) or a focused audit of one
 * sentence. Replays of the nightly report (see `useAuditReplay`) record into
 * the same results.
 */

import { computed, reactive, ref, shallowRef } from "vue";
import type { AuditBatch, AuditFocus, AuditResult } from "sigmakee/sdk";
import { call, isWasmAbort } from "../services/sigma";
import { useBootStore } from "../stores/boot";
import { useProverStore } from "../stores/prover";
import { errMsg } from "../utils/format";
import {
  FOCUS_BUDGETS,
  focusRoundSecs,
  nextCheckSecs,
} from "../utils/auditPlan";
import {
  contradictionKey,
  summarizeBatches,
} from "../utils/contradictionReport";
import type { AuditSweep } from "./useAuditSweep";
import { useElapsed } from "./useElapsed";

/** A contradiction the engine reported, stamped (epoch ms) when it arrived. */
export type Contradiction = AuditResult["contradictions"][number] & {
  foundAt: number;
};
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
export type LogEntry = (AuditBatch | CrashedBatch) & { budget?: number };

/** One sentence being audited (the editor's "Audit this sentence"). */
export interface AuditTarget {
  file: string;
  offset: number;
  sentence: AuditFocus;
}

/** `onClear` runs whenever results are cleared for a new run. */
export function useAuditRun(form: AuditSweep, onClear: () => void = () => {}) {
  const prover = useProverStore();
  const boot = useBootStore();

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
  /** The results come from a replay of the nightly report ... */
  const replayMode = ref(false);
  /** ... or from rechecking its targets against local edits. */
  const lastRunRecheck = ref(false);
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
  const busyLabel = computed(
    () => `Auditing… ${checked.value} checked · ${elapsedLabel(run.totalSecs)}`,
  );

  /** Clear the last run's results before a new one. */
  function clearResults() {
    backendLabel.value = prover.backendLabel;
    contradictions.value = [];
    batches.value = [];
    rawLines.value = [];
    error.value = "";
    stopRequested.value = false;
    replayMode.value = false;
    lastRunRecheck.value = false;
    onClear();
  }

  function reset(focused: boolean) {
    clearResults();
    run.focused = focused;
    run.totalSecs = form.plan.value.totalSecs;
    run.perCheckSecs = form.plan.value.perCheckSecs;
    run.widest = 0;
  }

  /** Add `result`'s subproblems and new contradictions to the log. */
  function record(result: AuditResult, seen: Set<string>, budget?: number) {
    const foundAt = Date.now();
    const fresh = result.contradictions
      .filter((c) => {
        const key = contradictionKey(c.steps);
        if (seen.has(key)) return false;
        seen.add(key);
        return true;
      })
      .map((c) => ({ ...c, foundAt }));
    if (fresh.length)
      contradictions.value = [...contradictions.value, ...fresh];
    batches.value = [
      ...batches.value,
      ...result.batches.map((b) => (budget ? { ...b, budget } : b)),
    ];
    rawLines.value = [...rawLines.value, result.raw_output];
  }

  function crashMessage(e: unknown): string {
    return isWasmAbort(errMsg(e))
      ? "Three checks in a row crashed the engine, most likely by running out of memory (a check can need several GB; the browser allows 4 GB). Lower the time per round or the axioms per round and continue from the saved step."
      : errMsg(e);
  }

  /** Sweep the saved position onward, one round per worker call. */
  async function runSweep() {
    const { sweep } = form;
    reset(false);
    const p = form.plan.value;
    const seed = Math.max(0, Math.floor(sweep.seed) || 0);
    const begin = Math.max(0, Math.floor(sweep.step) || 0);
    Object.assign(run, {
      seed,
      start: begin,
      next: begin,
      total: 0,
      planned: p.rounds,
    });
    auditing.value = true;
    const deadline = p.totalSecs
      ? performance.now() + p.totalSecs * 1000
      : null;
    const seen = new Set<string>();
    let crashes = 0;
    try {
      await prover.loadDefaults();
      for (
        let round = 0;
        !stopRequested.value &&
        found.value < p.limit &&
        (p.rounds == null || round < p.rounds);
        round++
      ) {
        const secs = nextCheckSecs(
          p.perCheckSecs,
          deadline == null ? null : deadline - performance.now(),
        );
        if (deadline != null && secs === 0) break;
        const config = prover.config("audit", { timeLimitSecs: secs });
        const started = performance.now();
        let result: AuditResult;
        try {
          ({ result } = await call("audit", {
            config,
            request: {
              seed,
              step: run.next,
              count: p.batch,
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
              range: [run.next, run.next + p.batch],
            },
          ];
          await boot.recoverWorker();
          run.next += p.batch;
          sweep.step = run.next;
          form.saveSweep();
          continue;
        }
        run.total = result.total;
        record(result, seen);
        if (result.next_step <= run.next) break;
        run.next = result.next_step;
        // Saved per round, so leaving mid-run still resumes here.
        sweep.seed = seed;
        sweep.step = run.next;
        sweep.total = result.total;
        form.saveSweep();
      }
    } catch (e) {
      error.value = crashMessage(e);
    } finally {
      auditing.value = false;
    }
  }

  /** Audit one sentence: check its neighbourhood at each of FOCUS_BUDGETS in
   *  turn, sharing the total time, until a contradiction turns up. */
  async function runFocused(t: AuditTarget) {
    reset(true);
    const p = form.plan.value;
    auditing.value = true;
    const deadline = p.totalSecs
      ? performance.now() + p.totalSecs * 1000
      : null;
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
            at: { file: t.file, offset: t.offset },
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

  return {
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
    clearResults,
    record,
    crashMessage,
    runSweep,
    runFocused,
  };
}

export type AuditRun = ReturnType<typeof useAuditRun>;
