import type { Session } from "sigmakee/sdk";

/** Direct taxonomy response shared with the SDK. */
export type Taxonomy = ReturnType<Session["taxonomy"]>;
/** A directed assertion, from child to parent. */
export interface TaxonomyEdge {
  child: string;
  parent: string;
  relation: string;
}
/** A term position and its allocated share of the branching cone. */
export interface TaxonomyNode {
  name: string;
  position: number[];
  depth: number;
  weight: number;
  share: number;
}
export const RELATION_COLORS: Record<string, string> = {
  subclass: "#79b5ff",
  instance: "#6ee7ae",
  subrelation: "#c5a3ff",
  subAttribute: "#ffd17b",
};

/** Fetch descendants in bounded batches; the caller owns cancellation and KB lifetime. */
export async function loadTaxonomy(
  fetchTaxonomy: (symbol: string) => Promise<Taxonomy>,
  depth: number,
  cancelled: () => boolean,
  progress: (count: number) => void,
  limit = 6000,
) {
  const pages = new Map<string, Taxonomy>();
  const seen = new Set(["Entity"]);
  let frontier = ["Entity"];
  let truncated = false;
  for (let level = 0; frontier.length && level <= depth; level++) {
    const next: string[] = [];
    for (let start = 0; start < frontier.length; start += 24) {
      if (cancelled()) return null;
      const names = frontier.slice(start, start + 24);
      const results = await Promise.all(names.map(fetchTaxonomy));
      if (cancelled()) return null;
      results.forEach((tax, i) => {
        pages.set(names[i], tax);
        for (const edge of tax.children) {
          if (seen.has(edge.parent)) continue;
          if (level === depth || seen.size >= limit) {
            truncated = true;
            continue;
          }
          seen.add(edge.parent);
          next.push(edge.parent);
        }
      });
      progress(pages.size);
    }
    frontier = next;
  }
  return { pages, truncated };
}

/** Lay out a cycle-safe spanning tree while retaining all visible assertions.
 * Subtree mass allocates solid angle; each child continues its branch direction.
 */
export function layoutTaxonomy(
  pages: Map<string, Taxonomy>,
  relations: string[],
) {
  const allowed = new Set(relations);
  const edgeMap = new Map<string, TaxonomyEdge>();
  for (const [name, tax] of pages) {
    for (const [rows, downward] of [
      [tax.children, true],
      [tax.parents, false],
    ] as const) {
      for (const row of rows) {
        const child = downward ? row.parent : name;
        const parent = downward ? name : row.parent;
        if (
          !allowed.has(row.relation) ||
          !pages.has(child) ||
          !pages.has(parent)
        )
          continue;
        const edge = { child, parent, relation: row.relation };
        edgeMap.set(JSON.stringify([child, parent, row.relation]), edge);
      }
    }
  }
  const outgoing = new Map<string, Set<string>>();
  for (const edge of edgeMap.values()) {
    if (!outgoing.has(edge.parent)) outgoing.set(edge.parent, new Set());
    outgoing.get(edge.parent)!.add(edge.child);
  }
  if (!pages.has("Entity"))
    return { nodes: [] as TaxonomyNode[], edges: [] as TaxonomyEdge[] };
  const nodes: TaxonomyNode[] = [
    { name: "Entity", position: [0, 0, 0], depth: 0, weight: 1, share: 1 },
  ];
  const found = new Map([[nodes[0].name, nodes[0]]]);
  const children = new Map<string, TaxonomyNode[]>();
  for (let i = 0; i < nodes.length; i++) {
    const node = nodes[i];
    const kids: TaxonomyNode[] = [];
    for (const name of [...(outgoing.get(node.name) ?? [])].sort()) {
      if (found.has(name)) continue;
      const child = {
        name,
        position: [0, 0, 0],
        depth: node.depth + 1,
        weight: 1,
        share: 0,
      };
      kids.push(child);
      nodes.push(child);
      found.set(name, child);
    }
    children.set(node.name, kids);
  }
  for (const node of [...nodes].reverse()) {
    node.weight += (children.get(node.name) ?? []).reduce(
      (sum, child) => sum + child.weight,
      0,
    );
  }
  const directions = new Map<string, number[]>([["Entity", [0, 1, 0]]]);
  for (const node of nodes) {
    const kids = children.get(node.name) ?? [];
    const total = kids.reduce((sum, child) => sum + child.weight, 0);
    const axis = directions.get(node.name)!;
    const reference = Math.abs(axis[1]) < 0.9 ? [0, 1, 0] : [1, 0, 0];
    const u = normalize(cross(axis, reference));
    const v = cross(axis, u);
    let offset = 0;
    kids.forEach((child, i) => {
      child.share = child.weight / total;
      const middle = offset + child.share / 2;
      offset += child.share;
      // Equal-area latitude bands allocate sphere area in proportion to branch mass.
      const cosine = node.depth === 0 ? 1 - 2 * middle : 1 - 0.85 * middle;
      const sine = Math.sqrt(1 - cosine * cosine);
      const phi =
        node.depth === 0
          ? i * Math.PI * (3 - Math.sqrt(5))
          : 2 * Math.PI * middle;
      const direction =
        kids.length === 1 && node.depth > 0
          ? axis
          : axis.map(
              (value, j) =>
                value * cosine +
                sine * (u[j] * Math.cos(phi) + v[j] * Math.sin(phi)),
            );
      directions.set(child.name, direction);
      const length = 75 + 26 * Math.log2(1 + child.weight);
      child.position = node.position.map(
        (value, j) => value + length * direction[j],
      );
    });
  }
  return {
    nodes,
    edges: [...edgeMap.values()].filter(
      (e) => found.has(e.child) && found.has(e.parent),
    ),
  };
}

function cross(a: number[], b: number[]) {
  return [
    a[1] * b[2] - a[2] * b[1],
    a[2] * b[0] - a[0] * b[2],
    a[0] * b[1] - a[1] * b[0],
  ];
}
function normalize(v: number[]) {
  const length = Math.hypot(...v);
  return v.map((x) => x / length);
}
