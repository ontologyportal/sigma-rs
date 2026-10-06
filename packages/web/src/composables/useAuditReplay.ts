/**
 * The nightly master-branch contradiction report on the Audit tab: fetch
 * it, replace the workspace with its exact inputs and replay its reported
 * subproblems, then recheck those targets against local edits. Results land
 * in the tab's run (see `useAuditRun`).
 */

import { onActivated, ref, shallowRef } from "vue";
import { onBeforeRouteLeave } from "vue-router";
import { call } from "../services/sigma";
import {
  checkAuditReplay,
  latestAuditReport,
  loadAuditReplay,
} from "../services/audit-replay";
import { useKBStore } from "../stores/kb";
import { useProverStore } from "../stores/prover";
import { errMsg } from "../utils/format";
import {
  replayAxiomKey,
  replayPositions,
  type AuditReplay,
} from "../utils/auditReplay";
import { summarizeBatches } from "../utils/contradictionReport";
import type { AuditRun } from "./useAuditRun";
import type { AuditSweep } from "./useAuditSweep";

export type AuditReport = Awaited<ReturnType<typeof latestAuditReport>>;

export function useAuditReplay(runs: AuditRun, form: AuditSweep) {
  const kb = useKBStore();
  const prover = useProverStore();

  const reportOpen = ref(false);
  const reportLoading = ref(false);
  const report = shallowRef<AuditReport | null>(null);
  const reportError = ref("");
  const replayBusy = ref(false);
  const replayDone = ref(0);
  const replayMessage = ref("");
  const loadedReplay = shallowRef<AuditReplay | null>(null);
  const recheckReason = ref("");
  const recheckRevision = ref(0);
  const rechecking = ref(false);

  async function refreshRecheck() {
    if (!loadedReplay.value || replayBusy.value || runs.auditing.value) return;
    try {
      const state = await call("auditRecheckStatus");
      recheckReason.value = state.reason;
      recheckRevision.value = state.revision;
    } catch (e) {
      recheckReason.value = errMsg(e);
    }
  }
  onActivated(refreshRecheck);
  // Leaving mid-replay would strand a half-replaced workspace.
  onBeforeRouteLeave(() => !replayBusy.value);

  async function recheckContradictions() {
    if (!loadedReplay.value || replayBusy.value || runs.auditing.value) return;
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
      runs.auditing.value
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
      form.sweep.scope = "";
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
    const { run } = runs;
    runs.clearResults();
    const positions = replayPositions(replay);
    runs.replayMode.value = true;
    runs.lastRunRecheck.value = rechecking.value;
    replayDone.value = 0;
    runs.backendLabel.value = "SUPr";
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
    runs.auditing.value = true;
    const seen = new Set<string>();
    try {
      while (!runs.stopRequested.value && replayDone.value < positions.length) {
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
        runs.record(result, seen);
        replayDone.value++;
        run.next = result.next_step;
      }
      replayMessage.value = rechecking.value
        ? recheckSummary(positions.length)
        : replaySummary(replay, positions.length);
    } catch (e) {
      runs.error.value = runs.crashMessage(e);
      replayMessage.value = `Replay failed: ${runs.error.value}`;
    } finally {
      runs.auditing.value = false;
    }
  }

  function recheckSummary(total: number): string {
    const s = summarizeBatches(runs.batches.value);
    const inconclusive = s.timeLimit + s.stepLimit + s.crashed + s.other;
    return runs.stopRequested.value
      ? `Recheck stopped after ${replayDone.value}/${total} targets; remaining targets were not checked.`
      : `Rechecked ${replayDone.value} reported targets against your edits: ${s.contradictory} still produced contradictions, ${s.clean} no longer reproduced a contradiction, ${inconclusive} inconclusive. This does not prove the entire knowledge base consistent.`;
  }

  function replaySummary(replay: AuditReplay, total: number): string {
    const expected = new Set(
      replay.findings.map((f) => replayAxiomKey(f.axioms)),
    );
    const actual = new Set(
      runs.contradictions.value.map((c) =>
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
    if (runs.stopRequested.value)
      return `Replay stopped after ${replayDone.value}/${total} reported subproblems.`;
    return missing || extra
      ? `Replay differs from the workflow: ${missing} reported contradiction(s) missing, ${extra} additional. See the check log for time or step limits.`
      : `Reproduced all ${expected.size} reported contradiction(s) in ${total} subproblems.`;
  }

  return {
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
  };
}
