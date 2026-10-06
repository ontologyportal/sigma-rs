/**
 * How the Audit tab spends a total time budget. The simple form only takes
 * a total; {@link planAudit} derives the per-check settings the advanced form
 * shows, and the run loop asks {@link nextCheckSecs} / {@link focusRoundSecs}
 * how long each check may take so the whole run ends near the deadline.
 *
 * Pure: no Vue, no imports (unit-tested in isolation).
 */

/** Longest time one sweep check gets by default: most neighbourhoods either
 *  saturate or refute well inside this, and a browser check can need GBs. */
export const MAX_CHECK_SECS = 10;
export const MIN_CHECK_SECS = 2;

/** Total-time choices the simple form offers. */
export const TOTAL_PRESETS = [
  { label: "1 min", secs: 60 },
  { label: "5 min", secs: 300 },
  { label: "15 min", secs: 900 },
  { label: "1 hour", secs: 3600 },
];

/** Neighbourhood budgets (max axioms) a focused audit widens through. */
export const FOCUS_BUDGETS = [250, 1000, 4000];

export interface AuditPlan {
  /** Deadline for the whole run in seconds; 0 = none. */
  totalSecs: number;
  /** Time limit per check. */
  perCheckSecs: number;
  /** Rounds (checks) to run; null = as many as the deadline allows. */
  rounds: number | null;
  /** Axioms added per round. */
  batch: number;
  /** Stop after this many distinct contradictions. */
  limit: number;
}

/** The sweep settings for a `totalSecs` budget: a third of it per check,
 *  between {@link MIN_CHECK_SECS} and {@link MAX_CHECK_SECS}, checking one
 *  axiom at a time until the deadline or five contradictions. */
export function planAudit(totalSecs: number): AuditPlan {
  const total = Math.max(0, Math.floor(Number(totalSecs) || 0));
  const perCheckSecs = total
    ? Math.min(MAX_CHECK_SECS, Math.max(MIN_CHECK_SECS, Math.round(total / 3)))
    : MAX_CHECK_SECS;
  return { totalSecs: total, perCheckSecs, rounds: null, batch: 1, limit: 5 };
}

/** Rounds a plan is guaranteed to reach even if every round runs to its
 *  limit (null without a deadline or round count). Rounds usually end sooner. */
export function minRounds(plan: AuditPlan): number | null {
  if (!plan.totalSecs || !plan.perCheckSecs) return plan.rounds;
  const n = Math.max(1, Math.floor(plan.totalSecs / plan.perCheckSecs));
  return plan.rounds == null ? n : Math.min(n, plan.rounds);
}

/** Time limit for the next sweep check: `perCheckSecs` (0 = none) cut down to
 *  the whole seconds left before the deadline (`remainingMs` null = no
 *  deadline). 0 means there is no time for another check. */
export function nextCheckSecs(
  perCheckSecs: number,
  remainingMs: number | null,
): number {
  if (remainingMs == null) return perCheckSecs;
  const left = Math.floor(remainingMs / 1000);
  if (left < 1) return 0;
  return perCheckSecs ? Math.min(perCheckSecs, left) : left;
}

/** Time limit for the next focused round: the remaining time split evenly
 *  over the rounds left, so a quick round leaves more for the wider ones.
 *  0 means there is no time for another round. */
export function focusRoundSecs(
  remainingMs: number,
  roundsLeft: number,
): number {
  if (roundsLeft < 1) return 0;
  const left = Math.floor(remainingMs / 1000);
  if (left < 1) return 0;
  return Math.max(1, Math.floor(left / roundsLeft));
}
