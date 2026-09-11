import { call } from "./sigma";

/** One taxonomy assertion `(relation child parent)` as the worker reports
 *  it on a man page's `parents` / `children`. */
export interface TaxEdge {
  relation: string;
  parent: string;
}

export interface WalkOptions {
  /** Upper bound on symbols fetched from the worker (default 80). */
  budget?: number;
  /** Polled between frontier fetches; returning true abandons the walk. */
  cancelled?: () => boolean;
}

/** Walk the ENTIRE ancestor graph upward from `page` (all parents,
 *  transitively, to the roots -- Entity, ultimately), returning every
 *  visited symbol's parent edges keyed by symbol (the page itself included).
 *
 *  BFS with a whole frontier fetched at once: a FIFO queue is level order
 *  anyway, and the levels are deep enough that one round-trip per symbol
 *  dominates the walk. Bounded by `budget` and cycle-safe. */
export async function walkAncestors(
  page: { name: string; parents: TaxEdge[] },
  opts: WalkOptions = {},
): Promise<Map<string, TaxEdge[]>> {
  const cancelled = opts.cancelled ?? (() => false);
  const parentEdges = new Map<string, TaxEdge[]>([[page.name, page.parents]]);
  const seen = new Set<string>([page.name]);
  let frontier: string[] = [];
  const push = (edges: TaxEdge[]) => {
    for (const e of edges) {
      if (!seen.has(e.parent)) {
        seen.add(e.parent);
        frontier.push(e.parent);
      }
    }
  };
  push(page.parents);
  for (let budget = opts.budget ?? 80; frontier.length && budget > 0;) {
    const level = frontier.slice(0, budget);
    budget -= level.length;
    const fetched = await Promise.all(
      level.map((sym) =>
        call("taxonomy", { symbol: sym })
          .then((r): TaxEdge[] | null => r.tax?.parents ?? [])
          .catch(() => null),
      ),
    );
    if (cancelled()) break;
    frontier = [];
    level.forEach((sym, i) => {
      const ps = fetched[i];
      if (!ps) return;
      parentEdges.set(sym, ps);
      push(ps);
    });
  }
  return parentEdges;
}

/** Ancestors by distance: level 1 (index 0) is the direct parents, level 2
 *  their parents, and so on; each symbol is listed once, at its shortest
 *  level. */
export function ancestorLevels(
  page: { name: string },
  parentEdges: Map<string, TaxEdge[]>,
): string[][] {
  const levels: string[][] = [];
  const seen = new Set<string>([page.name]);
  let frontier = [page.name];
  while (frontier.length) {
    const next: string[] = [];
    for (const sym of frontier) {
      for (const e of parentEdges.get(sym) ?? []) {
        if (!seen.has(e.parent)) {
          seen.add(e.parent);
          next.push(e.parent);
        }
      }
    }
    if (next.length) levels.push(next);
    frontier = next;
  }
  return levels;
}
