<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, watch } from "vue";
import type cytoscape from "cytoscape";
import type cytoscapeDagre from "cytoscape-dagre";
import { navigate } from "../../router";
import { call } from "../../services/sigma";
import { cytoscapeStyle, loadCytoscape } from "../../services/proof-graph";
import { useShellStore } from "../../stores/shell";
import { errMsg } from "../../utils/format";

const props = defineProps<{
  /** The man page whose ancestor chain and direct children to draw. */
  page: any;
}>();

/** The edge kinds the tree walks, in legend order. */
const TAX_RELATIONS = ["subclass", "instance", "subrelation", "subAttribute"];

/** How many direct children the diagram shows before eliding the rest. */
const TAX_MAX_CHILDREN = 60;

const DEFAULT_TIP = "tap a node to open its man page · scroll to zoom";

const shell = useShellStore();
const container = ref<HTMLElement | null>(null);
const mount = ref<HTMLElement | null>(null);
const status = ref("tracing taxonomy…");
const tip = ref(DEFAULT_TIP);
const rels = ref<string[]>([]);
let cy: cytoscape.Core | null = null;
let seq = 0;

/** Legend order: the known kinds first, then anything else the KB uses. */
function legendOrder(present: Set<string>): string[] {
  return [
    ...TAX_RELATIONS.filter((r) => present.has(r)),
    ...[...present].filter((r) => !TAX_RELATIONS.includes(r)),
  ];
}

function destroy() {
  cy?.destroy();
  cy = null;
}

/** Walk the ENTIRE ancestor graph upward from `p` (all parents, transitively,
 *  to the roots -- Entity, ultimately), add the direct children, and render it
 *  all as a Cytoscape diagram: roots at the top, the current symbol
 *  highlighted, edges color-coded by relation kind. Tapping any other node
 *  opens its man page. Bounded and cycle-safe. */
async function render() {
  const p = props.page;
  const mine = ++seq;
  destroy();
  status.value = "tracing taxonomy…";
  tip.value = DEFAULT_TIP;
  rels.value = legendOrder(
    new Set([...p.parents, ...p.children].map((e: any) => e.relation)),
  );

  // BFS upward, collecting every symbol's parent edges. A whole frontier is
  // fetched at once: a FIFO queue is level order anyway, and the levels are
  // deep enough that one round-trip per symbol dominates the walk.
  const parentEdges = new Map<string, any[]>([[p.name, p.parents]]);
  const seen = new Set<string>([p.name]);
  let frontier: string[] = [];
  for (const e of p.parents) {
    if (!seen.has(e.parent)) {
      seen.add(e.parent);
      frontier.push(e.parent);
    }
  }
  for (let budget = 80; frontier.length && budget > 0;) {
    const level = frontier.slice(0, budget);
    budget -= level.length;
    const fetched = await Promise.all(
      level.map((sym) =>
        call("taxonomy", { symbol: sym })
          .then((r) => r.tax?.parents ?? [])
          .catch(() => null),
      ),
    );
    if (mine !== seq) return;
    frontier = [];
    level.forEach((sym, i) => {
      const ps = fetched[i];
      if (!ps) return;
      parentEdges.set(sym, ps);
      for (const e of ps) {
        if (!seen.has(e.parent)) {
          seen.add(e.parent);
          frontier.push(e.parent);
        }
      }
    });
  }

  // Elements: ancestor nodes + current + (capped) direct children; one edge
  // per taxonomy assertion, tagged with its relation for the color styling.
  const present = new Set<string>();
  const nodes = new Map<string, cytoscape.ElementDefinition>();
  const addNode = (sym: string, kind: string) => {
    if (!nodes.has(sym))
      nodes.set(sym, { data: { id: sym, label: sym, kind } });
  };
  const edges: cytoscape.ElementDefinition[] = [];
  const addEdge = (child: string, relation: string, parent: string) => {
    present.add(relation);
    edges.push({
      data: {
        id: `${child}->${parent}#${relation}`,
        source: child,
        target: parent,
        rel: relation,
      },
    });
  };
  addNode(p.name, "current");
  for (const [child, ps] of parentEdges) {
    if (child !== p.name) addNode(child, "ancestor");
    for (const e of ps) {
      addNode(e.parent, "ancestor");
      addEdge(child, e.relation, e.parent);
    }
  }
  const shownKids = p.children.slice(0, TAX_MAX_CHILDREN);
  for (const e of shownKids) {
    addNode(e.parent, "child");
    addEdge(e.parent, e.relation, p.name);
  }
  const elided = p.children.length - shownKids.length;

  try {
    const cytoscape = await loadCytoscape();
    if (mine !== seq || !mount.value) return;
    cy = cytoscape({
      container: mount.value,
      elements: [...nodes.values(), ...edges],
      style: taxonomyGraphStyle(shell.isDark),
      // Edges point child -> parent, so rank bottom-to-top puts Entity on top.
      layout: {
        name: "dagre",
        rankDir: "BT",
        nodeSep: 14,
        rankSep: 46,
      } as cytoscapeDagre.DagreLayoutOptions,
      wheelSensitivity: 0.2,
    });
    status.value = "";
    cy.on("tap", "node", (e) => {
      const sym = e.target.id();
      if (sym !== p.name) navigate("browse", { sym });
    });
    cy.on("mouseover", "edge", (e) => {
      tip.value = `(${e.target.data("rel")} ${e.target.source().id()} ${e.target.target().id()})`;
    });
    if (elided)
      tip.value += ` · showing ${shownKids.length} of ${p.children.length} children`;
  } catch (err) {
    status.value = "Failed to load taxonomy graph: " + errMsg(err);
    return;
  }
  rels.value = legendOrder(present);
}

/** Cytoscape style for the taxonomy diagram: node emphasis by role (current /
 *  ancestor / child), edge color by relation kind -- matching the legend pills. */
function taxonomyGraphStyle(dark: boolean): cytoscape.StylesheetJson {
  const relColor: Record<string, string> = {
    subclass: dark ? "#6ea8ff" : "#2d6cdf", // --accent
    instance: dark ? "#4ac26b" : "#1a7f37", // --ok
    subrelation: dark ? "#d2a8ff" : "#8250df", // --op
    subAttribute: dark ? "#e3b341" : "#9a6700", // --warn
  };
  return [
    ...cytoscapeStyle(dark),
    { selector: "node", style: { "font-size": 11, "text-max-width": "140px" } },
    {
      selector: 'node[kind="current"]',
      style: { "border-width": 3, "font-weight": "bold" },
    },
    { selector: 'node[kind="child"]', style: { "border-style": "dashed" } },
    ...Object.entries(relColor).map(([rel, color]) => ({
      selector: `edge[rel="${rel}"]`,
      style: { "line-color": color, "target-arrow-color": color },
    })),
  ];
}

onMounted(render);
watch(() => props.page, render);
watch(
  () => shell.isDark,
  (dark) => {
    cy?.style(taxonomyGraphStyle(dark));
  },
);
onBeforeUnmount(() => {
  seq += 1;
  destroy();
});
</script>

<template>
  <div class="taxtree">
    <div v-if="rels.length" class="tax-legend">
      <span class="hint">edges:</span>
      <span v-for="r in rels" :key="r" class="tax-rel" :data-rel="r">{{
        r
      }}</span>
    </div>
    <div ref="container" class="graph-container tax-graph">
      <div ref="mount" class="tax-mount"></div>
      <span v-if="status" class="hint">{{ status }}</span>
    </div>
    <div class="hint graph-tip">{{ tip }}</div>
  </div>
</template>

<style scoped>
/* Taxonomy tree: ancestor chain above the current symbol, direct children below. */
.taxtree {
  font-size: 14px;
}
.graph-container {
  position: relative;
  height: 340px;
  border: 1px solid var(--line);
  border-radius: 7px;
  background: var(--bg);
  margin-top: 8px;
}
.tax-graph {
  height: 380px;
}
.tax-mount {
  position: absolute;
  inset: 0;
}
.tax-graph .hint {
  position: absolute;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  height: 100%;
  pointer-events: none;
}
.tax-legend {
  margin-bottom: 4px;
}
/* Edge-kind pills, color-coded per relation. */
.tax-rel {
  display: inline-block;
  font-size: 10px;
  font-weight: 600;
  line-height: 1.5;
  padding: 0 6px;
  border-radius: 999px;
  margin-right: 5px;
  vertical-align: 1px;
  background: color-mix(in srgb, var(--muted) 16%, transparent);
  color: var(--muted);
}
.tax-rel[data-rel="subclass"] {
  background: color-mix(in srgb, var(--accent) 15%, transparent);
  color: var(--accent);
}
.tax-rel[data-rel="instance"] {
  background: color-mix(in srgb, var(--ok) 15%, transparent);
  color: var(--ok);
}
.tax-rel[data-rel="subrelation"] {
  background: color-mix(in srgb, var(--op) 15%, transparent);
  color: var(--op);
}
.tax-rel[data-rel="subAttribute"] {
  background: color-mix(in srgb, var(--warn) 20%, transparent);
  color: var(--warn);
}
.graph-tip {
  min-height: 18px;
  margin-top: 6px;
  font-family: var(--mono);
  white-space: pre-wrap;
  word-break: break-word;
}
</style>
