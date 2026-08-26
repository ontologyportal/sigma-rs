/**
 * Proof graph (Cytoscape.js, lazy npm-package load).
 *
 * Interactive rendering of a proof/contradiction's `{index, rule, premises,
 * kif}[]` steps, used by both Ask/Tell's proof and each Audit contradiction.
 * The taxonomy tree reuses the loader and the base style.
 */

import type cytoscape from 'cytoscape';
import type cytoscapeDagre from 'cytoscape-dagre';
import { formatKif } from 'sigmakee/sdk';
import { isDarkTheme } from './dom.ts';
import { highlightKif } from './kif-highlight.ts';

let cytoscapeLoadPromise = null;

/** A `<details>` element doubling as a Cytoscape instance cache — `_cy` is an
 *  ad hoc property stashed directly on it, not part of the DOM type. */
type ProofGraphDetails = HTMLDetailsElement & { _cy?: cytoscape.Core | null };

/** `cy.nodeHtmlLabel(...)`, registered onto `cytoscape.Core` at runtime by
 *  the `cytoscape-node-html-label` extension (see `loadCytoscape`) — it ships
 *  no proper Core type augmentation, so this stands in for one locally. */
type CyWithHtmlLabel = cytoscape.Core & {
  nodeHtmlLabel(specs: {
    query?: string; halign?: string; valign?: string;
    halignBox?: string; valignBox?: string; tpl: (data) => string;
  }[]): void;
};

/** Loads cytoscape.js, cytoscape-dagre, and cytoscape-node-html-label (node
 *  bodies are a DOM overlay, not canvas text — see `nodeLabelHtml`) and
 *  registers them. Dynamic `import()` so Vite code-splits all three into
 *  their own chunk, fetched only when this actually runs. */
export function loadCytoscape(): Promise<typeof cytoscape> {
  if (cytoscapeLoadPromise) return cytoscapeLoadPromise;
  cytoscapeLoadPromise = (async () => {
    const [{ default: cytoscape }, { default: dagre }, { default: nodeHtmlLabel }] = await Promise.all([
      import('cytoscape'),
      import('cytoscape-dagre'),
      import('cytoscape-node-html-label'),
    ]);
    cytoscape.use(dagre);
    cytoscape.use(nodeHtmlLabel);
    return cytoscape;
  })().catch((e) => {
    cytoscapeLoadPromise = null;   // a cached rejection would break the graph for the session
    throw e;
  });
  return cytoscapeLoadPromise;
}

/** A step's `rule` — as the wire already tags it (`axiom`/`hypothesis` for KB
 *  input, `conjecture`/`negated_conjecture` for the query, anything else an
 *  inference rule name) — collapsed to the three node categories the graph
 *  color-codes. */
function stepKind(rule: string): 'axiom' | 'conjecture' | 'lemma' {
  if (rule === 'axiom' || rule === 'hypothesis') return 'axiom';
  if (rule === 'conjecture') return 'conjecture';
  return 'lemma';
}

/** Category swatches, in the same order `stepKind` can return them — the
 *  `data-kind` values match `cytoscapeStyle`'s `node[kind="..."]` selectors,
 *  and `.pg-legend-swatch`'s border-color per kind (styles.css) mirrors those
 *  same node outline colors. */
const PROOF_GRAPH_LEGEND: { kind: 'axiom' | 'conjecture' | 'lemma'; label: string }[] = [
  { kind: 'axiom', label: 'axiom' },
  { kind: 'conjecture', label: 'conjecture' },
  { kind: 'lemma', label: 'derived lemma' },
];

/** Legend row naming each node category's outline color, used by each Audit
 *  contradiction (tabs/audit.ts, templated per card). The Ask/Tell proof
 *  graph's copy is static markup in index.html (there's no per-render
 *  templating step there to call this from) — keep the two in sync by hand
 *  if `PROOF_GRAPH_LEGEND` changes. */
export function proofGraphLegendHtml(): string {
  const items = PROOF_GRAPH_LEGEND
    .map(({ kind, label }) => `<span class="pg-legend-item" data-kind="${kind}"><span class="pg-legend-swatch"></span>${label}</span>`)
    .join('');
  return `<div class="pg-legend">${items}</div>`;
}

/** A node's body: pretty-printed, syntax-highlighted KIF — `data.kif` is
 *  already `formatKif`-ed by `stepsToElements`, so this only tokenizes it.
 *  Reused both by `cy.nodeHtmlLabel`'s live template and, once, up front by
 *  `measureLabel` to size the underlying canvas node to match. */
function nodeLabelHtml(data): string {
  return `<div class="pg-node-label">${highlightKif(data.kif)}</div>`;
}

let labelProbe: HTMLDivElement | null = null;

/** Render `html` off-screen once to measure the box the live `.pg-node-label`
 *  overlay will occupy, so the canvas-drawn node (border/background — the
 *  category color) can be sized to actually wrap it. `cy.nodeHtmlLabel`
 *  positions its DOM overlay independently of the node's style box, so
 *  without this the two would drift out of sync. */
function measureLabel(html: string): { w: number; h: number } {
  if (!labelProbe) {
    labelProbe = document.createElement('div');
    labelProbe.style.position = 'fixed';
    labelProbe.style.visibility = 'hidden';
    labelProbe.style.left = '-9999px';
    labelProbe.style.top = '0';
    document.body.appendChild(labelProbe);
  }
  labelProbe.innerHTML = html;
  return { w: labelProbe.offsetWidth, h: labelProbe.offsetHeight };
}

/** Proof/contradiction steps → Cytoscape elements: one node per step, one edge per premise. */
function stepsToElements(steps): cytoscape.ElementDefinition[] {
  const nodes = steps.map((s) => {
    const kif = formatKif(s.kif);
    const { w, h } = measureLabel(nodeLabelHtml({ index: s.index, kif }));
    return { data: { id: `n${s.index}`, index: s.index, kif, kind: stepKind(s.rule), w, h } };
  });
  const edges = steps.flatMap((s) => s.premises.map((p) => ({
    data: { id: `n${p}-n${s.index}`, source: `n${p}`, target: `n${s.index}`, label: `${s.rule}` },
  })));
  return [...nodes, ...edges];
}

export function cytoscapeStyle(dark: boolean): cytoscape.StylesheetJson {
  return [
    {
      selector: 'node', style: {
        'background-color': dark ? '#1e2024' : '#f7f7f8',
        'border-color': dark ? '#6ea8ff' : '#2d6cdf',
        'border-width': 1.5,
        shape: 'round-rectangle',
        label: 'data(label)',
        color: dark ? '#e6e6e6' : '#1a1a1a',
        'font-family': 'ui-monospace, SFMono-Regular, Menlo, monospace',
        'font-size': 12,
        'text-valign': 'center', 'text-halign': 'center',
        'text-wrap': 'wrap',
        padding: '3px', width: 'label', height: 'label',
      }
    },
    {
      selector: 'edge', style: {
        width: 1.5,
        'line-color': dark ? '#9aa0a6' : '#666666',
        'target-arrow-color': dark ? '#9aa0a6' : '#666666',
        'target-arrow-shape': 'triangle',
        'curve-style': 'bezier',
        label: 'data(label)',
        'font-size': 12,
        color: dark ? '#9aa0a6' : '#666666',
        'text-background-color': dark ? '#1a1c1f' : '#ffffff',
        'text-background-opacity': 0.85,
        'text-background-padding': '2px',
        'text-background-shape': 'roundrectangle',
        'text-margin-y': -6,
      }
    },
    // Category outline colors — axiom (KB input), conjecture (the query,
    // negated for refutation), lemma (everything derived by an inference
    // rule) — mirror the --accent/--warn/--ok CSS tokens (styles.css), same
    // as the relation-color convention in tabs/taxonomy.ts's `relColor`.
    {
      selector: 'node[kind="axiom"]', style: {
        'border-color': dark ? '#6ea8ff' : '#2d6cdf',
      }
    },
    {
      selector: 'node[kind="conjecture"]', style: {
        'border-color': dark ? '#e3b341' : '#9a6700',
      }
    },
    {
      selector: 'node[kind="lemma"]', style: {
        'border-color': dark ? '#4ac26b' : '#1a7f37',
      }
    },
    {
      selector: 'node:selected', style: {
        'border-color': dark ? '#d2a8ff' : '#8250df',
        'border-width': 3,
      }
    },
  ];
}

/** Layered after `cytoscapeStyle` for the proof graph only (not the taxonomy
 *  tree, which shares that base style but keeps plain canvas labels): node
 *  bodies are an HTML overlay (`cy.nodeHtmlLabel`), not canvas text, so size
 *  from the measured HTML (`stepsToElements`' `data.w`/`data.h`) instead of
 *  canvas label metrics. */
function proofNodeOverrides(): cytoscape.StylesheetJson {
  return [
    { selector: 'node', style: { label: '', width: 'data(w)', height: 'data(h)' } },
  ];
}

/** Create a Cytoscape instance inside `container` from `steps`, top-down (dagre) layout. */
async function renderProofGraph(container, steps): Promise<cytoscape.Core> {
  const cytoscape = await loadCytoscape();
  container.textContent = '';
  const dark = isDarkTheme();
  const cy = cytoscape({
    container,
    elements: stepsToElements(steps),
    style: [...cytoscapeStyle(dark), ...proofNodeOverrides()],
    layout: { name: 'dagre', rankDir: 'TB', nodeSep: 20, rankSep: 40 } as cytoscapeDagre.DagreLayoutOptions,
    wheelSensitivity: 0.5,
  });
  (cy as CyWithHtmlLabel).nodeHtmlLabel([
    {
      query: 'node',
      halign: 'center', valign: 'center', halignBox: 'center', valignBox: 'center',
      tpl: (data) => nodeLabelHtml(data),
    },
  ]);
  return cy;
}

const EXPAND_ICON = `<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M8 3H5a2 2 0 0 0-2 2v3"/><path d="M21 8V5a2 2 0 0 0-2-2h-3"/><path d="M3 16v3a2 2 0 0 0 2 2h3"/><path d="M16 21h3a2 2 0 0 0 2-2v-3"/></svg>`;
const COMPRESS_ICON = `<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M8 3v3a2 2 0 0 1-2 2H3"/><path d="M21 8h-3a2 2 0 0 1-2-2V3"/><path d="M3 16h3a2 2 0 0 1 2 2v3"/><path d="M16 21v-3a2 2 0 0 1 2-2h3"/></svg>`;

/** Bottom-left fullscreen toggle over `container` (the native Fullscreen API,
 *  not a CSS-only modal, so Esc/browser chrome exit it for free). Cytoscape
 *  doesn't observe its container resizing on its own, so `resize`+`fit` on
 *  `fullscreenchange` is what actually makes the graph fill the new size. */
function wireFullscreenButton(container: HTMLElement, details: ProofGraphDetails) {
  const btn = document.createElement('button');
  btn.type = 'button';
  btn.className = 'pg-fullscreen-btn';
  btn.innerHTML = EXPAND_ICON;
  btn.title = 'Fullscreen';
  btn.setAttribute('aria-label', 'Toggle fullscreen');
  btn.addEventListener('click', () => {
    if (document.fullscreenElement === container) document.exitFullscreen();
    else container.requestFullscreen();
  });
  container.addEventListener('fullscreenchange', () => {
    const isFull = document.fullscreenElement === container;
    btn.innerHTML = isFull ? COMPRESS_ICON : EXPAND_ICON;
    btn.title = isFull ? 'Exit fullscreen' : 'Fullscreen';
    details._cy?.resize();
    details._cy?.fit();
  });
  container.appendChild(btn);
}

/** Wire a `<details>` element to lazily render its proof graph the first time it's opened, and re-fit on later opens. */
export function wireProofGraph(details: ProofGraphDetails, container: HTMLElement, getSteps) {
  // Permanent children of `container`, built once: `render` below only ever
  // touches `mount`/`status`, so it can't wipe out the fullscreen button the
  // way clearing `container.textContent` wholesale used to.
  const mount = document.createElement('div');
  mount.className = 'pg-mount';
  const status = document.createElement('div');
  status.className = 'pg-status';
  container.append(mount, status);
  wireFullscreenButton(container, details);

  const render = async () => {
    if (details._cy) { details._cy.destroy(); details._cy = null; }
    status.textContent = 'Loading graph…';
    try {
      details._cy = await renderProofGraph(mount, getSteps());
      status.textContent = '';
    } catch (err) {
      status.textContent = 'Failed to load graph: ' + (err && err.message || err);
    }
  };
  details.addEventListener('toggle', () => {
    if (!details.open) return;
    if (details._cy) { details._cy.resize(); details._cy.fit(); }
    else render();
  });
  return () => { if (details.open) render(); else if (details._cy) { details._cy.destroy(); details._cy = null; } };
}
