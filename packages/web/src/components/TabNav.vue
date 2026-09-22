<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useRoute, type LocationQuery } from "vue-router";
import { PROMOTE_TABS } from "../constants";
import { navigate, type TabName } from "../router";
import { useKBStore } from "../stores/kb";
import DropMenu from "./DropMenu.vue";

/** The tab navigation, in three tiers: a grouped tab strip when it fits,
 *  one button per group (opening a menu of its tabs) when it does not, and a
 *  grouped `<select>` on narrow screens -- no horizontal scrolling at any
 *  width. Returns to the query each tab was last on, greys promote-gated tabs
 *  while the KB is post-processing, and badges Diagnostics with its count. */

const kb = useKBStore();
const route = useRoute();

type TabGroup = { label: string; tabs: [TabName, string][] };

/** The nav, grouped by activity: `[routeName, label]` pairs. */
const TAB_GROUPS: TabGroup[] = [
  { label: "Explore", tabs: [["browse", "Browse"]] },
  {
    label: "Reason",
    tabs: [
      ["prover", "Ask/Tell"],
      ["audit", "Audit"],
    ],
  },
  {
    label: "Develop",
    tabs: [
      ["edit", "Edit"],
      ["diagnostics", "Diagnostics"],
    ],
  },
  {
    label: "Manage",
    tabs: [
      ["kb", "Knowledge base"],
      ["problems", "Inference Tests"],
    ],
  },
];

const errorCount = computed(
  () => kb.diagnostics.filter((d) => d.severity === "error").length,
);

const currentTab = computed(() => (route.name as TabName) ?? "browse");

function gated(name: TabName): boolean {
  return kb.promoting && PROMOTE_TABS.includes(name);
}

/** The select's option text: the label plus the diagnostics count. */
function optionLabel(name: TabName, label: string): string {
  return name === "diagnostics" && kb.diagnostics.length
    ? `${label} (${kb.diagnostics.length})`
    : label;
}

// Collapsed groups still have to say where you are, so the group button
// carries the name of its active tab and any badge its tabs would show.
function activeIn(group: TabGroup): string | null {
  return group.tabs.find(([name]) => name === currentTab.value)?.[1] ?? null;
}

function groupGated(group: TabGroup): boolean {
  return group.tabs.every(([name]) => gated(name));
}

function groupBadged(group: TabGroup): boolean {
  return (
    kb.diagnostics.length > 0 &&
    group.tabs.some(([name]) => name === "diagnostics")
  );
}

const groupBtns = ref<Record<string, HTMLElement | null>>({});
const openGroup = ref<string | null>(null);

function setGroupBtn(label: string, el: unknown) {
  groupBtns.value[label] = (el as HTMLElement | null) ?? null;
}

function toggleGroup(group: TabGroup) {
  if (groupGated(group)) return;
  openGroup.value = openGroup.value === group.label ? null : group.label;
}

// The tab bar returns to where each tab was left (its last query), so a
// visit elsewhere never resets a man page, a filter, or an open file.
const lastQuery = new Map<TabName, LocationQuery>();
watch(
  () => route.fullPath,
  () => lastQuery.set(currentTab.value, { ...route.query }),
  { immediate: true },
);

function go(name: TabName) {
  openGroup.value = null;
  if (gated(name) || name === currentTab.value) return;
  navigate(name, lastQuery.get(name));
}

function onSelect(e: Event) {
  go((e.target as HTMLSelectElement).value as TabName);
}

// Which tier to show is a fit question, not a viewport question: the strip's
// width depends on the tab set and on the system font, so it is measured
// rather than guessed at a breakpoint. The measurement runs on a detached
// clone laid out at an unconstrained width, so it reports what the strip
// wants rather than what it was squeezed into -- and, being out of flow, it
// cannot feed back into the size it is measuring against.
const shell = ref<HTMLElement | null>(null);
const strip = ref<HTMLElement | null>(null);
const avail = ref(0);
const natural = ref(0);
const narrow = ref(false);

const tier = computed(() =>
  narrow.value
    ? "select"
    : natural.value > 0 && avail.value < natural.value
      ? "rail"
      : "strip",
);

function measure() {
  const el = strip.value;
  if (!el) return;
  const probe = el.cloneNode(true) as HTMLElement;
  probe.style.cssText =
    "position:fixed;top:-9999px;left:0;width:3000px;display:flex;" +
    "flex-wrap:nowrap;visibility:hidden;pointer-events:none";
  document.body.appendChild(probe);
  const groups = [...probe.children] as HTMLElement[];
  groups.forEach((g) => (g.style.flex = "0 0 auto"));
  const left = probe.getBoundingClientRect().left;
  const right = groups.at(-1)?.getBoundingClientRect().right ?? left;
  probe.remove();
  natural.value = Math.ceil(right - left);
}

const NARROW = "(max-width: 760px)";
let mql: MediaQueryList | null = null;
let ro: ResizeObserver | null = null;
const onNarrow = (e: MediaQueryListEvent | MediaQueryList) =>
  (narrow.value = e.matches);

onMounted(() => {
  mql = window.matchMedia(NARROW);
  onNarrow(mql);
  mql.addEventListener("change", onNarrow);
  ro = new ResizeObserver((entries) => {
    avail.value = entries[0].contentRect.width;
    measure();
  });
  if (shell.value) ro.observe(shell.value);
  measure();
  // A fallback face measures differently from the real one.
  void document.fonts?.ready.then(measure);
});

onBeforeUnmount(() => {
  mql?.removeEventListener("change", onNarrow);
  ro?.disconnect();
});

// The Diagnostics badge appearing or changing digits changes the strip's
// width, which is exactly what the tier decision turns on. Post-flush: the
// badge has to be in the DOM to be measured.
watch(() => kb.diagnostics.length, measure, { flush: "post" });
</script>

<template>
  <div ref="shell" class="tabnav">
    <nav v-show="tier === 'strip'" ref="strip" class="tabs" role="tablist">
      <div
        v-for="group in TAB_GROUPS"
        :key="group.label"
        class="tab-group"
        :data-label="group.label"
      >
        <button
          v-for="[name, label] in group.tabs"
          :key="name"
          role="tab"
          type="button"
          :aria-selected="currentTab === name"
          :class="{ disabled: gated(name) }"
          :aria-disabled="gated(name)"
          @click="go(name)"
        >
          {{ label }}
          <span
            v-if="name === 'diagnostics' && kb.diagnostics.length"
            class="tab-badge"
            :class="{ err: errorCount > 0 }"
            >{{ kb.diagnostics.length }}</span
          >
        </button>
      </div>
    </nav>

    <nav v-show="tier === 'rail'" class="tab-rail">
      <template v-for="group in TAB_GROUPS" :key="group.label">
        <button
          v-if="group.tabs.length === 1"
          type="button"
          :class="{ active: !!activeIn(group), disabled: groupGated(group) }"
          :aria-current="activeIn(group) ? 'page' : undefined"
          :aria-disabled="groupGated(group)"
          @click="go(group.tabs[0][0])"
        >
          {{ group.tabs[0][1] }}
        </button>
        <button
          v-else
          :ref="(el) => setGroupBtn(group.label, el)"
          type="button"
          aria-haspopup="menu"
          :aria-expanded="openGroup === group.label"
          :class="{ active: !!activeIn(group), disabled: groupGated(group) }"
          :aria-disabled="groupGated(group)"
          @click="toggleGroup(group)"
        >
          <span class="rail-group">{{ group.label }}</span>
          <span v-if="activeIn(group)" class="rail-current">{{
            activeIn(group)
          }}</span>
          <span
            v-if="groupBadged(group)"
            class="tab-badge"
            :class="{ err: errorCount > 0 }"
            >{{ kb.diagnostics.length }}</span
          >
          <span class="rail-caret" aria-hidden="true">&#9662;</span>
        </button>
        <DropMenu
          v-if="group.tabs.length > 1"
          :model-value="openGroup === group.label"
          :anchor="groupBtns[group.label] ?? null"
          @update:model-value="
            (open) => (openGroup = open ? group.label : null)
          "
        >
          <button
            v-for="[name, label] in group.tabs"
            :key="name"
            type="button"
            role="menuitem"
            :disabled="gated(name)"
            :aria-current="currentTab === name ? 'page' : undefined"
            :class="{ current: currentTab === name }"
            @click="go(name)"
          >
            {{ optionLabel(name, label) }}
          </button>
        </DropMenu>
      </template>
    </nav>

    <label v-show="tier === 'select'" class="tab-select">
      <span class="hint">Section</span>
      <select :value="currentTab" aria-label="Section" @change="onSelect">
        <optgroup
          v-for="group in TAB_GROUPS"
          :key="group.label"
          :label="group.label"
        >
          <option
            v-for="[name, label] in group.tabs"
            :key="name"
            :value="name"
            :disabled="gated(name)"
          >
            {{ optionLabel(name, label) }}
          </option>
        </optgroup>
      </select>
    </label>
  </div>
</template>

<style scoped>
/* Widest tier: the tab strip, grouped by activity. Shown only while it
   fits (see `tier`); flex-wrap is the fallback if it is ever displayed
   before its first measurement. */
nav.tabs {
  display: flex;
  flex-wrap: wrap;
  gap: 0;
  margin: 18px 0 16px;
  border-bottom: 1px solid var(--line);
}
/* A quiet label above each cluster. */
.tab-group {
  display: flex;
  gap: 2px;
  position: relative;
  padding-top: 14px;
  flex: 0 0 auto;
}
.tab-group::before {
  content: attr(data-label);
  position: absolute;
  top: 0;
  left: 12px;
  font-size: 9px;
  text-transform: uppercase;
  letter-spacing: 0.09em;
  color: var(--muted);
  opacity: 0.8;
}
.tab-group + .tab-group {
  margin-left: 10px;
  padding-left: 10px;
  border-left: 1px solid var(--line);
}
nav.tabs button,
nav.tab-rail button {
  font: inherit;
  background: none;
  border: none;
  color: var(--muted);
  cursor: pointer;
  padding: 9px 12px;
  border-bottom: 2px solid transparent;
  margin-bottom: -1px;
  flex: 0 0 auto;
  white-space: nowrap;
}
nav.tabs button[aria-selected="true"],
nav.tab-rail button.active {
  color: var(--fg);
  border-bottom-color: var(--accent);
  font-weight: 600;
}
nav.tabs button.disabled,
nav.tab-rail button.disabled {
  opacity: 0.4;
  cursor: not-allowed;
}
/* Diagnostics count on its tab button; colored by worst severity. */
.tab-badge {
  display: inline-block;
  min-width: 17px;
  margin-left: 5px;
  padding: 0 5px;
  border-radius: 999px;
  font-size: 11px;
  font-weight: 700;
  line-height: 17px;
  text-align: center;
  background: color-mix(in srgb, var(--warn) 22%, transparent);
  color: var(--warn);
}
.tab-badge.err {
  background: color-mix(in srgb, var(--muted) 22%, transparent);
  color: var(--muted);
}

/* Middle tier: one button per group, each opening a menu of its tabs.
   Click, not hover -- this band is where tablets land, and a hover-only
   menu has neither a touch nor a keyboard story. */
nav.tab-rail {
  display: flex;
  gap: 4px;
  margin: 18px 0 16px;
  border-bottom: 1px solid var(--line);
}
/* The rail's buttons share the bar's full width rather than bunching at
   the left, so each group's hit area and underline span its own column. */
nav.tab-rail button {
  flex: 1 1 auto;
  justify-content: center;
  display: flex;
  align-items: baseline;
}
.rail-group {
  font-size: 12px;
  text-transform: uppercase;
  letter-spacing: 0.09em;
  color: var(--muted);
}
.rail-current {
  margin-left: 6px;
}
.rail-caret {
  margin-left: 5px;
  font-size: 10px;
  opacity: 0.7;
}
:deep(.dl-menu button.current) {
  color: var(--accent);
  font-weight: 600;
}

/* Narrow screens: one native select in place of the rail. */
.tab-select {
  display: flex;
  align-items: center;
  gap: 10px;
  margin: 14px 0 16px;
  padding-bottom: 12px;
  border-bottom: 1px solid var(--line);
}
.tab-select select {
  flex: 1 1 auto;
  min-width: 0;
}
</style>
