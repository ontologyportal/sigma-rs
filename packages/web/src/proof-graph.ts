/**
 * Proof graph (Cytoscape.js, lazy CDN load).
 *
 * Interactive rendering of a proof/contradiction's `{index, rule, premises,
 * kif}[]` steps, used by both Ask/Tell's proof and each Audit contradiction.
 * The taxonomy tree reuses the loader and the base style.
 */

import { isDarkTheme } from './dom.ts';
import { monacoLoading } from './editor/monaco.ts';

const CYTOSCAPE_VERSION = '3.34.0';
const CYTOSCAPE_DAGRE_VERSION = '4.0.0';

let cytoscapeLoadPromise = null;

function loadScript(src) {
  return new Promise((resolve, reject) => {
    const s = document.createElement('script');
    s.src = src;
    s.onload = resolve;
    s.onerror = () => reject(new Error(`failed to load ${src}`));
    document.head.appendChild(s);
  });
}

/**
 * Load a UMD bundle so it lands on `window`, not in a module registry.
 *
 * Monaco's loader installs a global `define` with `.amd`, and a UMD wrapper
 * that sees one registers itself as an anonymous AMD module and never sets its
 * browser global — so `window.cytoscape` stays undefined and instantiating it
 * throws "cytoscape is not a function". It only bites when the Edit tab loaded
 * Monaco before the first proof graph, which is what made it look intermittent.
 *
 * Hiding `define`/`exports`/`module` for the duration forces the wrapper down
 * its browser-global branch. They are restored in a `finally`, so an aborted
 * load cannot leave Monaco's loader detached.
 */
async function loadUmdGlobal(src) {
  const saved = { define: window.define, exports: window.exports, module: window.module };
  window.define = undefined; window.exports = undefined; window.module = undefined;
  try {
    await loadScript(src);
  } finally {
    window.define = saved.define; window.exports = saved.exports; window.module = saved.module;
  }
}

/** Loads cytoscape.js then cytoscape-dagre (which self-registers onto `window.cytoscape`). */
export function loadCytoscape() {
  if (cytoscapeLoadPromise) return cytoscapeLoadPromise;
  cytoscapeLoadPromise = (async () => {
    // Never pull `define` out from under an in-flight Monaco load.
    const monacoInFlight = monacoLoading();
    if (monacoInFlight) { try { await monacoInFlight; } catch { /* its own problem */ } }
    await loadUmdGlobal(`https://cdn.jsdelivr.net/npm/cytoscape@${CYTOSCAPE_VERSION}/dist/cytoscape.min.js`);
    await loadUmdGlobal(`https://cdn.jsdelivr.net/npm/cytoscape-dagre@${CYTOSCAPE_DAGRE_VERSION}/dist/cytoscape-dagre.min.js`);
    if (typeof window.cytoscape !== 'function') {
      throw new Error('cytoscape did not register as a browser global');
    }
    return window.cytoscape;
  })().catch((e) => {
    cytoscapeLoadPromise = null;   // a cached rejection would break the graph for the session
    throw e;
  });
  return cytoscapeLoadPromise;
}

/** Proof/contradiction steps → Cytoscape elements: one node per step, one edge per premise. */
function stepsToElements(steps) {
  const nodes = steps.map((s) => ({
    data: { id: `n${s.index}`, label: `${s.index + 1}. ${s.rule}`, kif: s.kif },
  }));
  const edges = steps.flatMap((s) => s.premises.map((p) => ({
    data: { id: `n${p}-n${s.index}`, source: `n${p}`, target: `n${s.index}` },
  })));
  return [...nodes, ...edges];
}

export function cytoscapeStyle(dark) {
  return [
    { selector: 'node', style: {
        'background-color': dark ? '#1e2024' : '#f7f7f8',
        'border-color':     dark ? '#6ea8ff' : '#2d6cdf',
        'border-width': 1.5,
        shape: 'round-rectangle',
        label: 'data(label)',
        color: dark ? '#e6e6e6' : '#1a1a1a',
        'font-family': 'ui-monospace, SFMono-Regular, Menlo, monospace',
        'font-size': 10,
        'text-valign': 'center', 'text-halign': 'center',
        'text-wrap': 'wrap', 'text-max-width': '160px',
        padding: '8px', width: 'label', height: 'label',
      } },
    { selector: 'edge', style: {
        width: 1.5,
        'line-color':          dark ? '#9aa0a6' : '#666666',
        'target-arrow-color':  dark ? '#9aa0a6' : '#666666',
        'target-arrow-shape': 'triangle',
        'curve-style': 'bezier',
      } },
    { selector: 'node:selected', style: {
        'border-color': dark ? '#d2a8ff' : '#8250df',
        'border-width': 3,
      } },
  ];
}

/** Create a Cytoscape instance inside `container` from `steps`, top-down (dagre) layout. */
async function renderProofGraph(container, steps, tipEl) {
  const cytoscape = await loadCytoscape();
  container.textContent = '';
  const dark = isDarkTheme();
  const cy = cytoscape({
    container,
    elements: stepsToElements(steps),
    style: cytoscapeStyle(dark),
    layout: { name: 'dagre', rankDir: 'TB', nodeSep: 20, rankSep: 40 },
    wheelSensitivity: 0.2,
  });
  if (tipEl) {
    cy.on('tap mouseover', 'node', (e) => { tipEl.textContent = e.target.data('kif'); });
  }
  return cy;
}

/** Wire a `<details>` element to lazily render its proof graph the first time it's opened, and re-fit on later opens. */
export function wireProofGraph(details, container, tipEl, getSteps) {
  const render = async () => {
    if (details._cy) { details._cy.destroy(); details._cy = null; }
    container.textContent = 'Loading graph…';
    try {
      details._cy = await renderProofGraph(container, getSteps(), tipEl);
    } catch (err) {
      container.textContent = 'Failed to load graph: ' + (err && err.message || err);
    }
  };
  details.addEventListener('toggle', () => {
    if (!details.open) return;
    if (details._cy) { details._cy.resize(); details._cy.fit(); }
    else render();
  });
  return () => { if (details.open) render(); else if (details._cy) { details._cy.destroy(); details._cy = null; } };
}
