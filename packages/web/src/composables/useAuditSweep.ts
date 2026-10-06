/**
 * The Audit tab's form: what to audit and for how long (Simple), every
 * per-round knob (Advanced), and the sweep position -- remembered per
 * knowledge base, so a later visit continues the same sweep. Storage is a
 * per-viewer convenience only: the form works without it.
 */

import {
  computed,
  inject,
  reactive,
  watch,
  type InjectionKey,
  type Ref,
} from "vue";
import { useKBStore } from "../stores/kb";
import { useProverStore } from "../stores/prover";
import { fmtNum } from "../utils/format";
import {
  FOCUS_BUDGETS,
  TOTAL_PRESETS,
  minRounds,
  planAudit,
  type AuditPlan,
} from "../utils/auditPlan";
import { fmtSecs } from "./useElapsed";

type PlanField = "perCheckSecs" | "rounds" | "batch" | "limit";

/** `focused`: a single sentence is being audited (the plan text says so). */
export function useAuditSweep(focused: Ref<boolean>) {
  const kb = useKBStore();
  const prover = useProverStore();

  /** The audit form. While `linked`, the per-round fields follow the plan
   *  derived from `totalSecs`; editing one of them in Advanced unlinks it. */
  const sweep = reactive({
    mode: "simple" as "simple" | "advanced",
    scope: "",
    totalSecs: 300,
    linked: true,
    perCheckSecs: 10,
    /** Rounds to run; null = until the time is up. */
    rounds: null as number | null,
    batch: 1,
    limit: 5,
    seed: 0,
    step: 0,
    /** Sweep size seen on the last run, for the Continue label. */
    total: 0,
  });

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
      const saved = JSON.parse(
        localStorage.getItem(storageKey.value) || "null",
      );
      if (saved && typeof saved === "object")
        for (const k of Object.keys(sweep) as (keyof typeof sweep)[])
          if (k in saved && typeof saved[k] === typeof sweep[k])
            (sweep as Record<string, unknown>)[k] = saved[k];
      // `rounds` is nullable, so typeof can't vet it above.
      if (saved && (saved.rounds === null || typeof saved.rounds === "number"))
        sweep.rounds = saved.rounds;
    } catch {
      /* storage unavailable or corrupt: keep defaults */
    }
    if (sweep.mode === "simple") resetPlan();
  }
  /** Put the Advanced per-round fields back to the plan derived from the
   *  total time.  The sweep position (seed / step) is kept. */
  function resetPlan() {
    Object.assign(sweep, planAudit(sweep.totalSecs), { linked: true });
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
  // Simple uses the defaults: leaving Advanced drops its overrides, the audit
  // prover profile's included.
  watch(
    () => sweep.mode,
    (mode) => {
      if (mode !== "simple") return;
      resetPlan();
      prover.reset("audit");
      saveSweep();
    },
  );

  const derived = computed(() => planAudit(sweep.totalSecs));
  /** The plan a run uses: derived from the total while linked, else the
   *  Advanced fields. */
  const plan = computed<AuditPlan>(() =>
    sweep.linked
      ? derived.value
      : {
          totalSecs: Math.max(0, Math.floor(sweep.totalSecs) || 0),
          perCheckSecs: Math.max(0, Math.floor(sweep.perCheckSecs) || 0),
          rounds:
            sweep.rounds && sweep.rounds > 0 ? Math.floor(sweep.rounds) : null,
          batch: Math.max(1, Math.floor(sweep.batch) || 1),
          limit: Math.max(1, Math.floor(sweep.limit) || 1),
        },
  );

  /** A v-model for one Advanced plan field: shows the derived value while
   *  linked; the first edit copies the plan in and unlinks. While a round
   *  count is set, the total follows it as rounds x time per round. */
  function planField(key: PlanField) {
    return computed({
      get: () => plan.value[key] ?? "",
      set: (v: number | string) => {
        if (sweep.linked) {
          Object.assign(sweep, derived.value);
          sweep.linked = false;
        }
        (sweep as Record<string, unknown>)[key] =
          v === "" || v === null ? (key === "rounds" ? null : 0) : Number(v);
        const p = plan.value;
        if ((key === "rounds" || key === "perCheckSecs") && p.rounds != null)
          sweep.totalSecs = p.rounds * p.perCheckSecs;
        saveSweep();
      },
    });
  }

  /** Sets the total time; a hand-set total means "check until it runs out". */
  function setTotal(secs: number) {
    sweep.totalSecs = secs;
    sweep.rounds = null;
  }
  const totalField = computed({
    get: () => sweep.totalSecs,
    set: (v: number | string) => setTotal(Math.max(0, Number(v) || 0)),
  });
  const totalMinutes = computed({
    get: () => Math.round((sweep.totalSecs / 60) * 10) / 10,
    set: (m: number) => {
      setTotal(Math.max(0, Math.round(Number(m) * 60) || 0));
    },
  });
  const presetTotal = computed({
    get: () =>
      TOTAL_PRESETS.some((p) => p.secs === sweep.totalSecs)
        ? String(sweep.totalSecs)
        : "custom",
    set: (v: string) => {
      if (v !== "custom") setTotal(Number(v));
      else if (TOTAL_PRESETS.some((p) => p.secs === sweep.totalSecs))
        setTotal(sweep.totalSecs + 60);
    },
  });
  const totalOptions = [
    ...TOTAL_PRESETS.map((p) => ({ value: String(p.secs), label: p.label })),
    { value: "custom", label: "Custom" },
  ];

  function relink() {
    sweep.linked = true;
  }
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

  /** The saved position has reached the end of its sweep. */
  const sweepDone = computed(
    () => sweep.total > 0 && sweep.step >= sweep.total,
  );

  /** What a run will do, in words. */
  const planText = computed(() => {
    const p = plan.value;
    const each = p.perCheckSecs
      ? `up to ${fmtSecs(p.perCheckSecs)} each`
      : "no limit each";
    const until = p.totalSecs
      ? `for ${fmtSecs(p.totalSecs)}`
      : "with no overall limit";
    const stop = `or until ${p.limit} contradiction${p.limit === 1 ? "" : "s"}`;
    if (focused.value)
      return p.totalSecs
        ? `Checks this sentence's neighbourhood at up to ${FOCUS_BUDGETS.join(", ")} axioms, sharing ${fmtSecs(p.totalSecs)}, and stops at the first contradiction.`
        : `Checks this sentence's neighbourhood at up to ${FOCUS_BUDGETS.join(", ")} axioms, ${each}, and stops at the first contradiction.`;
    const n = minRounds(p);
    const axioms = n == null ? 0 : n * p.batch;
    const reach =
      n == null
        ? ""
        : p.rounds != null && p.rounds <= n
          ? ` Covers ${fmtNum(axioms)} axiom${axioms === 1 ? "" : "s"}.`
          : ` Covers at least ${fmtNum(axioms)} axioms; most rounds finish well under their limit.`;
    const per =
      p.batch === 1
        ? "one axiom's neighbourhood"
        : `${fmtNum(p.batch)} axioms' neighbourhoods`;
    return `Checks ${per} per round, ${each}, ${until} ${stop}.${reach}`;
  });

  return {
    sweep,
    plan,
    planText,
    loadSweep,
    saveSweep,
    perCheckField: planField("perCheckSecs"),
    roundsField: planField("rounds"),
    batchField: planField("batch"),
    limitField: planField("limit"),
    totalField,
    totalMinutes,
    presetTotal,
    totalOptions,
    relink,
    onScopeChange,
    randomSeed,
    sweepDone,
  };
}

export type AuditSweep = ReturnType<typeof useAuditSweep>;

/** Hands the Audit tab's form to its Advanced-fields component. */
export const auditSweepKey: InjectionKey<AuditSweep> = Symbol("auditSweep");

/** The form the Audit tab provided (see {@link auditSweepKey}). */
export function useProvidedAuditSweep(): AuditSweep {
  const sweep = inject(auditSweepKey);
  if (!sweep) throw new Error("useProvidedAuditSweep: no Audit form provided");
  return sweep;
}
