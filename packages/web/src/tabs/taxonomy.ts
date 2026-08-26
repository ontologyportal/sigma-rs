/**
 * The man page's taxonomy tree.
 *
 * Replaces the flat Parents/Children link lists: the ancestor chain renders
 * above the current symbol (walked lazily upward after first paint), and the
 * descendants below expand on demand via the lightweight `taxonomy` call.
 */

import type cytoscape from 'cytoscape';
import type cytoscapeDagre from 'cytoscape-dagre';
import { call } from '../rpc.ts';
import { esc, escAttr, isDarkTheme } from '../dom.ts';
import { loadCytoscape, cytoscapeStyle } from '../proof-graph.ts';
import { navigate } from '../router.ts';

/** The edge kinds the tree walks, in legend order. */
const TAX_RELATIONS = ['subclass', 'instance', 'subrelation', 'subAttribute'];

/** How many direct children the diagram shows before eliding the rest. */
const TAX_MAX_CHILDREN = 60;

/** Color-coded pill for an edge's relation kind (colors keyed off data-rel). */
function relChip(relation) {
  return `<span class="tax-rel" data-rel="${escAttr(relation)}">${esc(relation)}</span>`;
}

/** Legend row naming each edge kind present in the rendered tree. */
function taxLegend(rels) {
  const present = [...TAX_RELATIONS.filter((r) => rels.has(r)),
                   ...[...rels].filter((r) => !TAX_RELATIONS.includes(r))];
  return present.length
    ? `<div class="tax-legend"><span class="hint">edges:</span> ${present.map(relChip).join(' ')}</div>`
    : '';
}

/** The initial widget: legend + an empty graph container that
 *  `fillAncestors` populates with the Cytoscape diagram. */
export function taxonomyWidget(p) {
  const rels = new Set([...p.parents, ...p.children].map((e) => e.relation));
  return `<div class="taxtree">
    ${taxLegend(rels)}
    <div id="taxGraph" class="graph-container tax-graph"><span class="hint">tracing taxonomy…</span></div>
    <div id="taxTip" class="hint graph-tip">tap a node to open its man page · scroll to zoom</div>
  </div>`;
}

/** Walk the ENTIRE ancestor graph upward from `p` (all parents, transitively,
 *  to the roots — Entity, ultimately), add the direct children, and render it
 *  all as a Cytoscape diagram: roots at the top, the current symbol
 *  highlighted, edges color-coded by relation kind. Tapping any other node
 *  opens its man page. Bounded and cycle-safe. */
export async function fillAncestors(p) {
  const container = document.getElementById('taxGraph');
  if (!container) return;

  // BFS upward, collecting every symbol's parent edges. A whole frontier is
  // fetched at once: a FIFO queue is level order anyway, and the levels are
  // deep enough that one round-trip per symbol dominates the walk.
  const parentEdges = new Map([[p.name, p.parents]]);       // sym → [{relation, parent}]
  const seen = new Set([p.name]);
  let frontier = [];
  for (const e of p.parents) if (!seen.has(e.parent)) { seen.add(e.parent); frontier.push(e.parent); }
  for (let budget = 80; frontier.length && budget > 0;) {
    const level = frontier.slice(0, budget);
    budget -= level.length;
    const fetched = await Promise.all(level.map((sym) =>
      call('taxonomy', { symbol: sym }).then((r) => r.tax?.parents ?? []).catch(() => null)));
    frontier = [];
    level.forEach((sym, i) => {
      const ps = fetched[i];
      if (!ps) return;
      parentEdges.set(sym, ps);
      for (const e of ps) if (!seen.has(e.parent)) { seen.add(e.parent); frontier.push(e.parent); }
    });
  }
  if (!container.isConnected) return;  // view re-rendered while walking — stale target

  // Elements: ancestor nodes + current + (capped) direct children; one edge
  // per taxonomy assertion, tagged with its relation for the color styling.
  const rels = new Set();
  const nodes = new Map();                                  // sym → node element
  const addNode = (sym, kind) => {
    if (!nodes.has(sym)) nodes.set(sym, { data: { id: sym, label: sym, kind } });
  };
  const edges = [];
  const addEdge = (child, relation, parent) => {
    rels.add(relation);
    edges.push({ data: { id: `${child}→${parent}#${relation}`, source: child, target: parent, rel: relation } });
  };
  addNode(p.name, 'current');
  for (const [child, ps] of parentEdges) {
    if (child !== p.name) addNode(child, 'ancestor');
    for (const e of ps) { addNode(e.parent, 'ancestor'); addEdge(child, e.relation, e.parent); }
  }
  const shownKids = p.children.slice(0, TAX_MAX_CHILDREN);
  for (const e of shownKids) { addNode(e.parent, 'child'); addEdge(e.parent, e.relation, p.name); }
  const elided = p.children.length - shownKids.length;

  try {
    const cytoscape = await loadCytoscape();
    if (!container.isConnected) return;
    container.textContent = '';
    const cy = cytoscape({
      container,
      elements: [...nodes.values(), ...edges],
      style: taxonomyGraphStyle(isDarkTheme()),
      // Edges point child → parent, so rank bottom-to-top puts Entity on top.
      layout: { name: 'dagre', rankDir: 'BT', nodeSep: 14, rankSep: 46 } as cytoscapeDagre.DagreLayoutOptions,
      wheelSensitivity: 0.2,
    });
    const tip = document.getElementById('taxTip');
    cy.on('tap', 'node', (e) => {
      const sym = e.target.id();
      if (sym !== p.name) navigate('browse', { sym });
    });
    if (tip) {
      cy.on('mouseover', 'edge', (e) => { tip.textContent = `(${e.target.data('rel')} ${e.target.source().id()} ${e.target.target().id()})`; });
      if (elided) tip.textContent += ` · showing ${shownKids.length} of ${p.children.length} children`;
    }
  } catch (err) {
    container.textContent = 'Failed to load taxonomy graph: ' + (err && err.message || err);
    return;
  }

  const legend = container.parentElement.querySelector('.tax-legend');
  const fresh = taxLegend(rels);
  if (legend) legend.outerHTML = fresh; else container.insertAdjacentHTML('beforebegin', fresh);
}

/** Cytoscape style for the taxonomy diagram: node emphasis by role (current /
 *  ancestor / child), edge color by relation kind — matching the legend pills. */
function taxonomyGraphStyle(dark: boolean): cytoscape.StylesheetJson {
  const relColor = {
    subclass:     dark ? '#6ea8ff' : '#2d6cdf',   // --accent
    instance:     dark ? '#4ac26b' : '#1a7f37',   // --ok
    subrelation:  dark ? '#d2a8ff' : '#8250df',   // --op
    subAttribute: dark ? '#e3b341' : '#9a6700',   // --warn
  };
  const base = cytoscapeStyle(dark);
  return [
    ...base,
    { selector: 'node', style: { 'font-size': 11, 'text-max-width': '140px' } },
    { selector: 'node[kind="current"]', style: {
        'border-width': 3,
        'font-weight': 'bold',
      } },
    { selector: 'node[kind="child"]', style: {
        'border-style': 'dashed',
      } },
    ...Object.entries(relColor).map(([rel, color]) => ({
      selector: `edge[rel="${rel}"]`,
      style: { 'line-color': color, 'target-arrow-color': color },
    })),
  ];
}
