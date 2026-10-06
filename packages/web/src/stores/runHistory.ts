/**
 * Run histories, one feed per tab: Ask/Tell's proves (each query, its
 * assertions and outcome, so an earlier run can be put back in the panes)
 * and the Inference Tests tab's test runs. Newest first, kept in
 * localStorage as a per-viewer convenience (capped at {@link MAX_ENTRIES});
 * the page works without it.
 */

import { defineStore } from "pinia";
import { ASK_HISTORY_KEY, TEST_HISTORY_KEY } from "../constants";

export const MAX_ENTRIES = 50;

export interface ProofRun {
  /** Unique within the history. */
  id: string;
  /** When the run finished (epoch ms). */
  at: number;
  /** The pane dialect the run was made in. */
  lang: "kif" | "tptp";
  /** The panes as they were: assertions (or the TPTP problem) and query. */
  assertions: string;
  query: string;
  /** The query as proved, in KIF (a TPTP problem's conjecture, translated). */
  goal: string;
  /** Shown in place of `goal` -- a test run's test name. */
  title?: string;
  /** How many formulas were told alongside it. */
  told: number;
  /** The prover status, or "Error" when the run failed outright. */
  status: string;
  backend: string;
}

/** The number of top-level forms in KIF `text` (comments and strings
 *  skipped). */
export function countForms(text: string): number {
  let depth = 0;
  let forms = 0;
  let inString = false;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (inString) {
      if (c === "\\") i++;
      else if (c === '"') inString = false;
    } else if (c === '"') inString = true;
    else if (c === ";") {
      while (i < text.length && text[i] !== "\n") i++;
    } else if (c === "(") {
      if (depth === 0) forms++;
      depth++;
    } else if (c === ")" && depth > 0) depth--;
  }
  return forms;
}

function isRun(v: unknown): v is ProofRun {
  const r = v as ProofRun | null;
  return (
    !!r &&
    typeof r.id === "string" &&
    typeof r.at === "number" &&
    typeof r.query === "string" &&
    typeof r.status === "string"
  );
}

function loadRuns(key: string): ProofRun[] {
  try {
    const raw: unknown = JSON.parse(localStorage.getItem(key) || "[]");
    return Array.isArray(raw) ? raw.filter(isRun).slice(0, MAX_ENTRIES) : [];
  } catch {
    return [];
  }
}

/** One feed's store, persisted under `key`. */
const historyStore = (id: string, key: string) =>
  defineStore(id, {
    state: () => ({ runs: loadRuns(key) }),
    actions: {
      persist() {
        try {
          localStorage.setItem(key, JSON.stringify(this.runs));
        } catch {
          /* storage unavailable or full */
        }
      },
      /** Record a finished run at the top of the history. */
      record(run: Omit<ProofRun, "id" | "at">) {
        const at = Date.now();
        this.runs = [
          { ...run, at, id: `${at}-${Math.random().toString(36).slice(2, 8)}` },
          ...this.runs,
        ].slice(0, MAX_ENTRIES);
        this.persist();
      },
      clear() {
        this.runs = [];
        this.persist();
      },
    },
  });

export const useAskHistoryStore = historyStore("askHistory", ASK_HISTORY_KEY);
export const useTestHistoryStore = historyStore(
  "testHistory",
  TEST_HISTORY_KEY,
);

/** Which tab's history: Ask/Tell's proves or the Inference Tests runs. */
export type HistoryFeed = "ask" | "test";

export const useRunHistory = (feed: HistoryFeed) =>
  feed === "ask" ? useAskHistoryStore() : useTestHistoryStore();
