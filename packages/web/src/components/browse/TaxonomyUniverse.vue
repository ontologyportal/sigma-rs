<script setup lang="ts">
import {
  computed,
  onBeforeUnmount,
  onMounted,
  ref,
  shallowRef,
  watch,
} from "vue";
import { call } from "../../services/sigma";
import { useKBStore } from "../../stores/kb";
import { errMsg } from "../../utils/format";
import {
  layoutTaxonomy,
  loadTaxonomy,
  RELATION_COLORS,
  type Taxonomy,
  type TaxonomyNode,
} from "../../services/taxonomy-3d";

const emit = defineEmits<{ open: [symbol: string] }>();
const kb = useKBStore();
const canvas = ref<HTMLCanvasElement | null>(null);
const host = ref<HTMLElement | null>(null);
const depth = ref(4);
const enabled = ref(Object.keys(RELATION_COLORS));
const pages = shallowRef(new Map<string, Taxonomy>());
const selected = ref("Entity");
const focused = ref("Entity");
const search = ref("");
const status = ref("");
const busy = ref(false);
const partial = ref(false);
const labels = ref(true);
const graph = computed(() => layoutTaxonomy(pages.value, enabled.value));
const visibleNames = computed(
  () => new Set(graph.value.nodes.map((node) => node.name)),
);
const current = computed(() => pages.value.get(selected.value));
const matches = computed(() =>
  graph.value.nodes
    .filter((n) => n.name.toLowerCase().includes(search.value.toLowerCase()))
    .slice(0, 30),
);
const connections = computed(() => [
  ...(current.value?.parents ?? []).map((e) => ({ ...e, direction: "Parent" })),
  ...(current.value?.children ?? []).map((e) => ({ ...e, direction: "Child" })),
]);
let generation = 0;
let observer: ResizeObserver | undefined;
let frame = 0;
let yaw = 0.35,
  pitch = -0.2,
  zoom = 1;
let panX = 0,
  panY = 0;
let dragging: {
  id: number;
  x: number;
  y: number;
  startX: number;
  startY: number;
  moved: boolean;
  pan: boolean;
} | null = null;
let projected: {
  node: TaxonomyNode;
  x: number;
  y: number;
  z: number;
  radius: number;
}[] = [];

async function load() {
  const mine = ++generation;
  pages.value = new Map();
  busy.value = true;
  partial.value = false;
  status.value = kb.promoting
    ? "Waiting for the knowledge base..."
    : "Loading hierarchy...";
  if (kb.promoting) return;
  try {
    const result = await loadTaxonomy(
      async (symbol) => {
        const response = await call<{ tax: Taxonomy }>("taxonomy", { symbol });
        return response.tax ?? { parents: [], children: [] };
      },
      depth.value,
      () => mine !== generation,
      (count) => {
        status.value = `Loading hierarchy: ${count.toLocaleString()} terms...`;
      },
    );
    if (!result || mine !== generation) return;
    pages.value = result.pages;
    partial.value = result.truncated;
    selected.value = "Entity";
    status.value = result.pages.get("Entity")?.children.length
      ? ""
      : "Entity has no hierarchy in the loaded knowledge base.";
    reset();
  } catch (error) {
    if (mine === generation)
      status.value = "Could not load hierarchy: " + errMsg(error);
  } finally {
    if (mine === generation) busy.value = false;
  }
}

function reset() {
  focused.value = "Entity";
  yaw = 0.35;
  pitch = -0.2;
  zoom = 1;
  panX = 0;
  panY = 0;
  schedule();
}
function choose(name: string) {
  if (!visibleNames.value.has(name)) return;
  selected.value = name;
  focused.value = name;
  panX = 0;
  panY = 0;
  schedule();
}
function schedule() {
  if (!frame)
    frame = requestAnimationFrame(() => {
      frame = 0;
      draw();
    });
}
function draw() {
  const element = canvas.value;
  const context = element?.getContext("2d");
  if (!element || !context) return;
  const width = element.clientWidth,
    height = element.clientHeight;
  if (!width || !height) return;
  const ratio = Math.min(devicePixelRatio || 1, 2);
  element.width = Math.round(width * ratio);
  element.height = Math.round(height * ratio);
  context.setTransform(ratio, 0, 0, ratio, 0, 0);
  context.clearRect(0, 0, width, height);
  const nodes = graph.value.nodes;
  const origin = nodes.find((node) => node.name === focused.value)
    ?.position ?? [0, 0, 0];
  const extent = Math.max(200, ...nodes.map((n) => Math.hypot(...n.position)));
  const scale = ((Math.min(width, height) * 0.44) / extent) * zoom;
  const selectedEdges = new Set<string>();
  const colors = new Map<string, string>();
  for (const edge of graph.value.edges) {
    if (!colors.has(edge.child))
      colors.set(edge.child, RELATION_COLORS[edge.relation] ?? "#94a3b8");
    if (edge.child === selected.value) selectedEdges.add(edge.parent);
    if (edge.parent === selected.value) selectedEdges.add(edge.child);
  }
  projected = nodes
    .map((node) => {
      const [nx, ny, nz] = node.position.map(
        (value, axis) => value - origin[axis],
      );
      const x = nx * Math.cos(yaw) + nz * Math.sin(yaw);
      const z0 = -nx * Math.sin(yaw) + nz * Math.cos(yaw);
      const y = ny * Math.cos(pitch) - z0 * Math.sin(pitch);
      const z = ny * Math.sin(pitch) + z0 * Math.cos(pitch);
      const perspective = 1 / (1 + z / (extent * 3));
      return {
        node,
        x: width / 2 + panX + x * scale * perspective,
        y: height / 2 + panY + y * scale * perspective,
        z,
        radius: Math.max(
          0.8,
          Math.min(
            18,
            (node.depth === 0 ? 11 : 3 + Math.log2(node.weight + 1) * 0.7) *
              Math.sqrt(zoom) *
              (node.depth === 0
                ? 1
                : Math.min(1, 25 / Math.sqrt(nodes.length))) *
              perspective,
          ),
        ),
      };
    })
    .sort((a, b) => b.z - a.z);
  const byName = new Map(projected.map((p) => [p.node.name, p]));
  for (const edge of graph.value.edges) {
    const a = byName.get(edge.child),
      b = byName.get(edge.parent);
    if (!a || !b) continue;
    const highlight =
      edge.child === selected.value || edge.parent === selected.value;
    context.globalAlpha = highlight ? 1 : 0.55;
    context.strokeStyle = RELATION_COLORS[edge.relation] ?? "#94a3b8";
    context.lineWidth = highlight ? 2.2 : 1.2;
    context.beginPath();
    context.moveTo(a.x, a.y);
    context.lineTo(b.x, b.y);
    context.stroke();
    if (highlight) {
      const angle = Math.atan2(b.y - a.y, b.x - a.x);
      const x = b.x - Math.cos(angle) * (b.radius + 3),
        y = b.y - Math.sin(angle) * (b.radius + 3);
      context.beginPath();
      context.moveTo(
        x - Math.cos(angle - 0.45) * 7,
        y - Math.sin(angle - 0.45) * 7,
      );
      context.lineTo(x, y);
      context.lineTo(
        x - Math.cos(angle + 0.45) * 7,
        y - Math.sin(angle + 0.45) * 7,
      );
      context.stroke();
    }
  }
  context.globalAlpha = 1;
  context.font = "12px system-ui";
  for (const p of projected) {
    if (p.x < -100 || p.x > width + 100 || p.y < -30 || p.y > height + 30)
      continue;
    const active = p.node.name === selected.value;
    const color =
      p.node.depth === 0 ? "#ffe2a3" : (colors.get(p.node.name) ?? "#79b5ff");
    const gradient = context.createRadialGradient(
      p.x - p.radius * 0.3,
      p.y - p.radius * 0.35,
      0,
      p.x,
      p.y,
      p.radius,
    );
    gradient.addColorStop(0, "#ffffff");
    gradient.addColorStop(0.3, color);
    gradient.addColorStop(1, "#172a46");
    context.fillStyle = gradient;
    context.beginPath();
    context.arc(p.x, p.y, p.radius, 0, Math.PI * 2);
    context.fill();
    if (active) {
      context.strokeStyle = "#ffffff";
      context.lineWidth = 1.5;
      context.beginPath();
      context.arc(p.x, p.y, p.radius + 4, 0, Math.PI * 2);
      context.stroke();
    }
    if (
      active ||
      (labels.value &&
        (p.node.depth <= 1 || selectedEdges.has(p.node.name) || zoom > 2.5))
    ) {
      context.lineWidth = 4;
      context.strokeStyle = "#091221";
      context.strokeText(p.node.name, p.x + p.radius + 7, p.y + 4);
      context.fillStyle = active ? "#ffffff" : "#bdcee6";
      context.fillText(p.node.name, p.x + p.radius + 7, p.y + 4);
    }
  }
}
function down(event: PointerEvent) {
  if (event.button !== 0) return;
  canvas.value?.setPointerCapture(event.pointerId);
  dragging = {
    id: event.pointerId,
    x: event.clientX,
    y: event.clientY,
    startX: event.clientX,
    startY: event.clientY,
    moved: false,
    pan: event.shiftKey,
  };
}
function move(event: PointerEvent) {
  if (!dragging || dragging.id !== event.pointerId) return;
  const dx = event.clientX - dragging.x,
    dy = event.clientY - dragging.y;
  if (
    Math.hypot(
      event.clientX - dragging.startX,
      event.clientY - dragging.startY,
    ) > 4
  )
    dragging.moved = true;
  if (dragging.pan) {
    panX += dx;
    panY += dy;
  } else {
    yaw += dx * 0.008;
    pitch += dy * 0.008;
  }
  dragging.x = event.clientX;
  dragging.y = event.clientY;
  schedule();
}
function up(event: PointerEvent) {
  if (!dragging || dragging.id !== event.pointerId) return;
  if (!dragging.moved) {
    const rect = canvas.value!.getBoundingClientRect();
    const x = event.clientX - rect.left,
      y = event.clientY - rect.top;
    const hit = [...projected]
      .reverse()
      .find((p) => Math.hypot(p.x - x, p.y - y) <= p.radius + 5);
    if (hit) choose(hit.node.name);
  }
  dragging = null;
}
function magnify(factor: number) {
  zoom = Math.max(0.2, Math.min(12, zoom * factor));
  schedule();
}
function key(event: KeyboardEvent) {
  const actions: Record<string, () => void> = {
    ArrowLeft: () => {
      yaw -= 0.12;
    },
    ArrowRight: () => {
      yaw += 0.12;
    },
    ArrowUp: () => {
      pitch -= 0.12;
    },
    ArrowDown: () => {
      pitch += 0.12;
    },
    "+": () => magnify(1.2),
    "=": () => magnify(1.2),
    "-": () => magnify(1 / 1.2),
    Home: reset,
  };
  if (actions[event.key]) {
    event.preventDefault();
    event.stopPropagation();
    actions[event.key]();
    schedule();
  }
}
watch(graph, () => {
  if (!visibleNames.value.has(focused.value)) {
    focused.value = "Entity";
    panX = 0;
    panY = 0;
  }
  if (!visibleNames.value.has(selected.value)) selected.value = "Entity";
});
watch([graph, selected, labels], schedule);
watch([depth, () => kb.promoting], load);
onMounted(() => {
  observer = new ResizeObserver(schedule);
  if (host.value) observer.observe(host.value);
  load();
});
onBeforeUnmount(() => {
  generation++;
  observer?.disconnect();
  cancelAnimationFrame(frame);
});
</script>

<template>
  <section class="universe" aria-label="3D SUMO hierarchy">
    <header class="universe-toolbar">
      <div>
        <span class="eyebrow">ONTOLOGY EXPLORER</span>
        <h2>SUMO in three dimensions</h2>
      </div>
      <div class="tools">
        <label
          >Levels
          <select v-model.number="depth">
            <option :value="2">2</option>
            <option :value="4">4</option>
            <option :value="6">6</option>
            <option :value="Number.POSITIVE_INFINITY">All</option>
          </select></label
        >
        <button type="button" @click="load">Reload</button>
        <button type="button" @click="reset">Reset view</button>
      </div>
    </header>
    <div
      class="universe-filters"
      role="group"
      aria-label="Edge colors and relationship filters"
    >
      <span class="eyebrow">EDGE COLORS</span>
      <label
        v-for="(color, relation) in RELATION_COLORS"
        :key="relation"
        :style="{ color }"
      >
        <input v-model="enabled" type="checkbox" :value="relation" />
        <span class="edge-swatch" aria-hidden="true"></span>{{ relation }}
      </label>
      <label><input v-model="labels" type="checkbox" /> Labels</label>
    </div>
    <div class="universe-body">
      <div ref="host" class="space">
        <canvas
          ref="canvas"
          tabindex="0"
          aria-label="3D hierarchy. Drag or use arrow keys to orbit, scroll or use plus and minus to zoom, shift-drag to pan. Select terms using the adjacent list."
          @pointerdown="down"
          @pointermove="move"
          @pointerup="up"
          @pointercancel="dragging = null"
          @lostpointercapture="dragging = null"
          @wheel.prevent="magnify(Math.exp(-$event.deltaY * 0.001))"
          @keydown="key"
        />
        <div class="space-caption">
          <span class="live-dot"></span> {{ focused }} AT CENTER
          <p>
            {{ graph.nodes.length.toLocaleString() }} terms /
            {{ graph.edges.length.toLocaleString() }} connections
          </p>
        </div>
        <p v-if="status" class="space-status" role="status">{{ status }}</p>
        <div class="zoom-tools">
          <button type="button" aria-label="Zoom in" @click="magnify(1.25)">
            +</button
          ><button type="button" aria-label="Zoom out" @click="magnify(0.8)">
            -
          </button>
        </div>
        <p class="space-help">
          Click a ball to center · Drag to orbit · Scroll to zoom · Shift-drag
          to pan
        </p>
      </div>
      <aside class="inspector">
        <label for="hierarchy-find">Find a visible term</label>
        <input
          id="hierarchy-find"
          v-model="search"
          type="search"
          placeholder="Entity, Physical, Process..."
        />
        <div class="term-results" aria-label="Visible terms">
          <button
            v-for="node in matches"
            :key="node.name"
            type="button"
            :aria-pressed="selected === node.name"
            @click="choose(node.name)"
          >
            {{ node.name }}
          </button>
          <p v-if="!matches.length">
            No visible matches. Try more levels or enable relationships.
          </p>
        </div>
        <span class="eyebrow">SELECTED TERM</span>
        <h3>{{ selected }}</h3>
        <button
          type="button"
          :disabled="busy || !current"
          @click="emit('open', selected)"
        >
          Open term page
        </button>
        <p class="connection-count">
          {{ current?.parents.length ?? 0 }} parents /
          {{ current?.children.length ?? 0 }} children
        </p>
        <div class="connections">
          <button
            v-for="(edge, index) in connections"
            :key="index"
            type="button"
            :disabled="!visibleNames.has(edge.parent)"
            @click="choose(edge.parent)"
          >
            <span>{{ edge.parent }}</span
            ><small :style="{ color: RELATION_COLORS[edge.relation] }"
              >{{ edge.direction }} · {{ edge.relation
              }}{{
                visibleNames.has(edge.parent)
                  ? ""
                  : pages.has(edge.parent)
                    ? " · filtered"
                    : " · not loaded"
              }}</small
            >
          </button>
        </div>
      </aside>
    </div>
    <footer>
      <span v-if="busy">Loading the requested hierarchy...</span>
      <span v-else-if="status">{{ status }}</span>
      <span v-else-if="partial"
        >Partial hierarchy: increase levels to explore further. Maximum 6,000
        terms per view.</span
      >
      <span v-else
        >Showing the reachable hierarchy in the loaded knowledge base.</span
      >
      Edge colors identify the defining relationship. Branch space follows
      visible subtree size. Arrows point to parents.
    </footer>
  </section>
</template>

<style scoped>
.universe {
  border: 1px solid #293951;
  border-radius: 12px;
  overflow: hidden;
  background: #0d1727;
  color: #dce7f7;
  margin: 16px 0;
}
.universe-toolbar {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 20px 24px;
}
.eyebrow {
  font: 10px var(--mono, monospace);
  letter-spacing: 0.16em;
  color: #90a6c4;
}
h2 {
  font-size: 21px;
  margin: 5px 0 0;
  color: #f2f6ff;
  font-weight: 500;
}
.tools,
.universe-filters {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 14px;
}
.universe label {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  margin: 0;
}
.universe input[type="checkbox"] {
  width: auto;
  margin: 0;
  accent-color: #79b5ff;
}
.universe button,
.universe select,
.universe input[type="search"] {
  background: #15243a;
  color: #dce7f7;
  border: 1px solid #32455e;
  border-radius: 6px;
  font: inherit;
  padding: 7px 10px;
}
.universe button {
  cursor: pointer;
  font-size: 12px;
}
.universe button:hover {
  background: #233a57;
}
.universe button:disabled {
  opacity: 0.5;
  cursor: default;
}
.universe-filters {
  padding: 12px 24px;
  border-top: 1px solid #233148;
  border-bottom: 1px solid #233148;
}
.edge-swatch {
  width: 22px;
  border-top: 3px solid currentColor;
}
.universe-body {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 255px;
}
.space {
  position: relative;
  min-width: 0;
  height: min(70vh, 720px);
  min-height: 440px;
  background: radial-gradient(ellipse at center, #142940 0, #091221 70%);
}
canvas {
  width: 100%;
  height: 100%;
  display: block;
  touch-action: none;
  cursor: grab;
}
canvas:active {
  cursor: grabbing;
}
canvas:focus-visible {
  outline: 2px solid #79b5ff;
  outline-offset: -3px;
}
.space-caption {
  position: absolute;
  top: 22px;
  left: 22px;
  pointer-events: none;
  font: 10px var(--mono, monospace);
  letter-spacing: 0.1em;
  color: #b7cae3;
}
.space-caption p {
  letter-spacing: 0;
  margin: 8px 0;
  color: #8299b7;
}
.live-dot {
  display: inline-block;
  width: 6px;
  height: 6px;
  background: #ffe2a3;
  border-radius: 50%;
  margin-right: 6px;
}
.space-help {
  position: absolute;
  bottom: 8px;
  left: 20px;
  color: #8299b7;
  font-size: 11px;
  pointer-events: none;
}
.space-status {
  position: absolute;
  left: 20px;
  top: 70px;
  right: 20px;
  color: #ffe2a3;
  pointer-events: none;
}
.zoom-tools {
  position: absolute;
  bottom: 40px;
  right: 18px;
  display: flex;
  gap: 5px;
}
.inspector {
  border-left: 1px solid #233148;
  padding: 18px;
  min-width: 0;
}
.inspector input {
  width: 100%;
  box-sizing: border-box;
  margin: 8px 0;
  font-size: 12px !important;
}
.term-results {
  max-height: 130px;
  overflow: auto;
  margin-bottom: 20px;
}
.term-results button {
  display: block;
  width: 100%;
  text-align: left;
  margin: 3px 0;
  overflow-wrap: anywhere;
}
.term-results button[aria-pressed="true"] {
  border-color: #79b5ff;
  background: #213b5b;
}
h3 {
  font-size: 20px;
  margin: 8px 0 14px;
  overflow-wrap: anywhere;
  color: #fff;
}
.connection-count {
  font-size: 12px;
  color: #90a6c4;
  margin: 18px 0 8px;
}
.connections {
  max-height: 190px;
  overflow: auto;
}
.connections button {
  display: block;
  text-align: left;
  width: 100%;
  margin-bottom: 4px;
  overflow-wrap: anywhere;
}
.connections small {
  display: block;
  font-size: 10px;
  margin-top: 4px;
}
footer {
  border-top: 1px solid #233148;
  padding: 12px 24px;
  color: #90a6c4;
  font-size: 11px;
  line-height: 1.8;
}
footer span {
  display: block;
}
@media (max-width: 760px) {
  .universe-body {
    grid-template-columns: 1fr;
  }
  .inspector {
    border-left: 0;
    border-top: 1px solid #233148;
  }
  .space {
    min-height: 380px;
  }
}
</style>
