<script setup lang="ts">
import { computed, watch } from "vue";
import { useRoute, type LocationQuery } from "vue-router";
import { PROMOTE_TABS } from "../constants";
import { navigate, type TabName } from "../router";
import { useKBStore } from "../stores/kb";

/** The tab navigation: a grouped tab strip on wide screens, a grouped
 *  `<select>` on narrow ones (no horizontal scrolling). Returns to the
 *  query each tab was last on, greys promote-gated tabs while the KB is
 *  post-processing, and badges Diagnostics with its finding count. */

const kb = useKBStore();
const route = useRoute();

/** The nav, grouped by activity: `[routeName, label]` pairs. */
const TAB_GROUPS: { label: string; tabs: [TabName, string][] }[] = [
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
      ["history", "History"],
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

// The tab bar returns to where each tab was left (its last query), so a
// visit elsewhere never resets a man page, a filter, or an open file.
const lastQuery = new Map<TabName, LocationQuery>();
watch(
  () => route.fullPath,
  () => lastQuery.set(currentTab.value, { ...route.query }),
  { immediate: true },
);

function go(name: TabName) {
  if (gated(name) || name === currentTab.value) return;
  navigate(name, lastQuery.get(name));
}

function onSelect(e: Event) {
  go((e.target as HTMLSelectElement).value as TabName);
}
</script>

<template>
  <nav class="tabs" role="tablist">
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
  <label class="tab-select">
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
</template>

<style scoped>
/* Wide screens: the tab strip, grouped by activity. It wraps rather than
   scrolls if it ever runs out of room. */
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
  left: 14px;
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
nav.tabs button {
  font: inherit;
  background: none;
  border: none;
  color: var(--muted);
  cursor: pointer;
  padding: 9px 14px;
  border-bottom: 2px solid transparent;
  margin-bottom: -1px;
  flex: 0 0 auto;
  white-space: nowrap;
}
nav.tabs button[aria-selected="true"] {
  color: var(--fg);
  border-bottom-color: var(--accent);
  font-weight: 600;
}
nav.tabs button.disabled {
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

/* Narrow screens: one native select in place of the strip. The strip's
   seven tabs need ~720px, so the switch happens well above phone widths. */
.tab-select {
  display: none;
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
@media (max-width: 760px) {
  nav.tabs {
    display: none;
  }
  .tab-select {
    display: flex;
  }
}
</style>
