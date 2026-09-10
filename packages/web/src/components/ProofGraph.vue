<script setup lang="ts">
import { onBeforeUnmount, ref, watch } from "vue";
import type cytoscape from "cytoscape";
import { PROOF_GRAPH_LEGEND, renderProofGraph } from "../services/proof-graph";
import { useShellStore } from "../stores/shell";
import { errMsg } from "../utils/format";

const props = defineProps<{
  /** The `{index, rule, premises, kif}[]` transcript to draw. */
  steps: any[];
  /** The engine's graphviz source for the same proof, shown under a toggle. */
  dot?: string;
}>();

const shell = useShellStore();
const container = ref<HTMLElement | null>(null);
const mount = ref<HTMLElement | null>(null);
const isOpen = ref(false);
const isFull = ref(false);
const status = ref("");
let cy: cytoscape.Core | null = null;

function destroy() {
  cy?.destroy();
  cy = null;
}

async function render() {
  destroy();
  status.value = "Loading graph…";
  try {
    cy = await renderProofGraph(mount.value, props.steps, shell.isDark);
    status.value = "";
  } catch (err) {
    status.value = "Failed to load graph: " + errMsg(err);
  }
}

/** Lazily render the first time the details opens; re-fit on later opens. */
function onToggle(e: Event) {
  isOpen.value = (e.target as HTMLDetailsElement).open;
  if (!isOpen.value) return;
  if (cy) {
    cy.resize();
    cy.fit();
  } else render();
}

watch(
  () => props.steps,
  () => {
    if (isOpen.value) render();
    else destroy();
  },
);

watch(
  () => shell.isDark,
  () => {
    if (isOpen.value) render();
  },
);

/** The native Fullscreen API, not a CSS-only modal, so Esc/browser chrome
 *  exit it for free. */
function toggleFullscreen() {
  if (document.fullscreenElement === container.value) document.exitFullscreen();
  else container.value?.requestFullscreen();
}

/** Cytoscape doesn't observe its container resizing on its own, so
 *  `resize`+`fit` here is what actually makes the graph fill the new size. */
function onFullscreenChange() {
  isFull.value = document.fullscreenElement === container.value;
  cy?.resize();
  cy?.fit();
}

onBeforeUnmount(destroy);
</script>

<template>
  <details class="proof-graph-details" @toggle="onToggle">
    <summary class="hint">proof graph</summary>
    <div
      ref="container"
      class="graph-container"
      @fullscreenchange="onFullscreenChange"
    >
      <div ref="mount" class="pg-mount"></div>
      <div class="pg-status">{{ status }}</div>
      <button
        type="button"
        class="pg-fullscreen-btn"
        :title="isFull ? 'Exit fullscreen' : 'Fullscreen'"
        aria-label="Toggle fullscreen"
        @click="toggleFullscreen"
      >
        <svg
          v-if="isFull"
          viewBox="0 0 24 24"
          width="14"
          height="14"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M8 3v3a2 2 0 0 1-2 2H3" />
          <path d="M21 8h-3a2 2 0 0 1-2-2V3" />
          <path d="M3 16h3a2 2 0 0 1 2 2v3" />
          <path d="M16 21v-3a2 2 0 0 1 2-2h3" />
        </svg>
        <svg
          v-else
          viewBox="0 0 24 24"
          width="14"
          height="14"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M8 3H5a2 2 0 0 0-2 2v3" />
          <path d="M21 8V5a2 2 0 0 0-2-2h-3" />
          <path d="M3 16v3a2 2 0 0 0 2 2h3" />
          <path d="M16 21h3a2 2 0 0 0 2-2v-3" />
        </svg>
      </button>
    </div>
    <div class="pg-legend">
      <span
        v-for="k in PROOF_GRAPH_LEGEND"
        :key="k.kind"
        class="pg-legend-item"
        :data-kind="k.kind"
      >
        <span class="pg-legend-swatch"></span>{{ k.label }}
      </span>
    </div>
    <details class="graph-dot-toggle">
      <summary>graphviz (DOT) source</summary>
      <pre>{{ dot || "(none)" }}</pre>
    </details>
  </details>
</template>

<style scoped>
details pre {
  font-family: var(--mono);
  font-size: 12px;
  white-space: pre-wrap;
  background: var(--bg);
  border: 1px solid var(--line);
  border-radius: 7px;
  padding: 10px;
  overflow-x: auto;
}
.graph-container {
  position: relative;
  height: 340px;
  border: 1px solid var(--line);
  border-radius: 7px;
  background: var(--bg);
  margin-top: 8px;
}
/* Native Fullscreen API target -- the UA stretches it to the viewport; drop
   the card chrome so the graph reads as its own view, not a floating box. */
.graph-container:fullscreen {
  height: 100%;
  border: none;
  border-radius: 0;
}
.graph-tip {
  min-height: 18px;
  margin-top: 6px;
  font-family: var(--mono);
  white-space: pre-wrap;
  word-break: break-word;
}
.graph-dot-toggle {
  margin-top: 8px;
}
.graph-dot-toggle summary {
  font-size: 12px;
  color: var(--muted);
  cursor: pointer;
}
.graph-dot-toggle pre {
  font-size: 11px;
  max-height: 200px;
  overflow-y: auto;
}
/* `pg-mount` (the Cytoscape container) and `pg-status` (loading/error text)
   are stacked full-bleed layers so the fullscreen button beside them
   survives a re-render. */
.pg-mount {
  position: absolute;
  inset: 0;
}
.pg-status {
  position: absolute;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  text-align: center;
  padding: 12px;
  font-family: var(--mono);
  color: var(--muted);
  white-space: pre-wrap;
  word-break: break-word;
  pointer-events: none;
}
.pg-status:empty {
  display: none;
}
.pg-fullscreen-btn {
  position: absolute;
  left: 8px;
  bottom: 8px;
  z-index: 2;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 26px;
  height: 26px;
  padding: 0;
  background: var(--card);
  color: var(--muted);
  border: 1px solid var(--line);
  border-radius: 6px;
  cursor: pointer;
  opacity: 0.85;
}
.pg-fullscreen-btn:hover {
  color: var(--fg);
  border-color: var(--accent);
  opacity: 1;
}
.pg-fullscreen-btn svg {
  display: block;
}
/* Proof graph node content: an HTML overlay (cytoscape-node-html-label) over
   the canvas-drawn, kind-colored node box -- shrink-wraps up to max-width, so
   the underlying node is sized to match (see proof-graph.ts's measureLabel). */
:deep(.pg-node-label) {
  display: inline-block;
  padding: 6px 8px;
  font-family: var(--mono);
  font-size: 10px;
  line-height: 1.35;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  color: var(--fg);
  pointer-events: none;
}
:deep(.pg-node-label .pg-idx) {
  color: var(--muted);
  margin-right: 4px;
}
/* Proof graph legend: one hollow swatch per node category, border-colored to
   match cytoscapeStyle's node[kind="..."] outline colors (proof-graph.ts). */
.pg-legend {
  display: flex;
  flex-wrap: wrap;
  gap: 6px 16px;
  margin-top: 8px;
  font-size: 12px;
  color: var(--muted);
}
.pg-legend-item {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}
.pg-legend-swatch {
  display: inline-block;
  width: 11px;
  height: 11px;
  border-radius: 3px;
  border: 2px solid;
  background: var(--card);
}
.pg-legend-item[data-kind="axiom"] .pg-legend-swatch {
  border-color: var(--accent);
}
.pg-legend-item[data-kind="conjecture"] .pg-legend-swatch {
  border-color: var(--warn);
}
.pg-legend-item[data-kind="lemma"] .pg-legend-swatch {
  border-color: var(--ok);
}
</style>
