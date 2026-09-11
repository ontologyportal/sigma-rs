<script setup lang="ts">
import { computed, ref } from "vue";
import { useSymbolLinks } from "../../composables/useSymbolLinks";
import Card from "../Card.vue";
import ManPageFormulas from "./ManPageFormulas.vue";
import ManPageOverview from "./ManPageOverview.vue";
import WordNetEntry from "./WordNetEntry.vue";

type View = "overview" | "formulas" | "wordnet";

const props = defineProps<{
  /** The worker's `manpage` payload, or null when the symbol has none. */
  page: any | null;
  /** The symbol asked for, named in the missing-page card. */
  symbol: string;
  /** The requested sub-tab (`?view=`); unknown or inapplicable values show
   *  the overview. */
  view?: string;
}>();

const emit = defineEmits<{ back: []; "update:view": [view: View] }>();

const root = ref<HTMLElement | null>(null);
useSymbolLinks(root);

const hasWordnet = computed(() => (props.page?.wordnet?.length ?? 0) > 0);

const subtab = computed<View>(() => {
  if (props.view === "formulas") return "formulas";
  if (props.view === "wordnet" && hasWordnet.value) return "wordnet";
  return "overview";
});
</script>

<template>
  <Card v-if="!page" class="hint"
    >No man page for <code>{{ symbol }}</code
    >.</Card
  >
  <div v-else ref="root">
    <Card class="man">
      <div class="man-head">
        <a class="hint back" @click.prevent="emit('back')">← back to results</a>
        <h2>{{ page.name }}</h2>
        <div class="kinds">{{ page.kinds.join(" · ") || "symbol" }}</div>
        <div class="man-subtabs" role="tablist">
          <button
            type="button"
            role="tab"
            :aria-selected="subtab === 'overview'"
            @click="emit('update:view', 'overview')"
          >
            Overview
          </button>
          <button
            type="button"
            role="tab"
            :aria-selected="subtab === 'formulas'"
            @click="emit('update:view', 'formulas')"
          >
            Formulas
            <span class="hint">({{ page.references.length }})</span>
          </button>
          <button
            v-if="hasWordnet"
            type="button"
            role="tab"
            :aria-selected="subtab === 'wordnet'"
            @click="emit('update:view', 'wordnet')"
          >
            WordNet <span class="hint">({{ page.wordnet.length }})</span>
          </button>
        </div>
      </div>
      <ManPageOverview v-show="subtab === 'overview'" :page="page" />
      <ManPageFormulas v-show="subtab === 'formulas'" :page="page" />
      <div v-if="hasWordnet" v-show="subtab === 'wordnet'">
        <WordNetEntry v-for="(m, i) in page.wordnet" :key="i" :entry="m" />
      </div>
    </Card>
  </div>
</template>

<style scoped>
.man h2 {
  font-family: var(--mono);
  font-size: 20px;
  margin: 0 0 2px;
}
/* Sticky header: the symbol name, kinds and sub-tabs stay pinned while long
   formula lists scroll underneath. */
.man-head {
  position: sticky;
  top: 0;
  z-index: 5;
  background: var(--card);
  margin: -14px -14px 8px;
  padding: 12px 14px 0;
  border-bottom: 1px solid var(--line);
  border-radius: 10px 10px 0 0;
}
/* Man page sub-tabs (Overview / Formulas / WordNet) -- a lighter-weight
   local echo of nav.tabs, scoped inside the sticky man-head. */
.man-subtabs {
  display: flex;
  gap: 2px;
  margin-top: 8px;
}
.man-subtabs button {
  font: inherit;
  font-size: 13px;
  background: none;
  border: none;
  color: var(--muted);
  cursor: pointer;
  padding: 5px 10px;
  border-bottom: 2px solid transparent;
}
.man-subtabs button[aria-selected="true"] {
  color: var(--fg);
  border-bottom-color: var(--accent);
  font-weight: 600;
}
</style>
