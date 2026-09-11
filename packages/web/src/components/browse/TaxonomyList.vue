<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import {
  ancestorLevels,
  walkAncestors,
  type TaxEdge,
} from "../../services/taxonomy";
import TaxonomyBranch, { type TaxNode } from "./TaxonomyBranch.vue";
import TaxRel from "./TaxRel.vue";

const props = defineProps<{
  /** The man page whose ancestor chain and direct children to list. */
  page: any;
}>();

/** How many direct children the listing shows before eliding the rest. */
const TAX_MAX_CHILDREN = 200;

const status = ref("tracing taxonomy…");
const parentEdges = ref<Map<string, TaxEdge[]> | null>(null);
let seq = 0;

async function trace() {
  const p = props.page;
  const mine = ++seq;
  status.value = "tracing taxonomy…";
  parentEdges.value = null;
  const edges = await walkAncestors(p, { cancelled: () => mine !== seq });
  if (mine !== seq) return;
  parentEdges.value = edges;
  status.value = "";
}

/** The ancestors as a tree rooted at the direct parents. A symbol is
 *  expanded only at its shortest level (from `ancestorLevels`) and only
 *  once, so a DAG with many paths to Entity stays readable: every later
 *  occurrence is a plain leaf link. */
const ancestors = computed<TaxNode[]>(() => {
  const p = props.page;
  const edges = parentEdges.value;
  if (!edges) return [];
  const levelOf = new Map<string, number>();
  ancestorLevels(p, edges).forEach((level, i) => {
    for (const sym of level) levelOf.set(sym, i + 1);
  });
  const expanded = new Set<string>();
  const build = (sym: string, depth: number): TaxNode[] =>
    (edges.get(sym) ?? []).map((e) => {
      const expand =
        levelOf.get(e.parent) === depth + 1 && !expanded.has(e.parent);
      if (expand) expanded.add(e.parent);
      return {
        relation: e.relation,
        sym: e.parent,
        children: expand ? build(e.parent, depth + 1) : null,
      };
    });
  return build(p.name, 0);
});

const shownChildren = computed<TaxEdge[]>(() =>
  props.page.children.slice(0, TAX_MAX_CHILDREN),
);
const elidedChildren = computed(
  () => props.page.children.length - shownChildren.value.length,
);

watch(() => props.page, trace, { immediate: true });
onBeforeUnmount(() => {
  seq += 1;
});
</script>

<template>
  <div class="taxlist">
    <h4>Ancestors</h4>
    <span v-if="status" class="hint">{{ status }}</span>
    <TaxonomyBranch v-else-if="ancestors.length" :nodes="ancestors" />
    <span v-else class="hint">none</span>
    <h4>Children</h4>
    <ul v-if="shownChildren.length" class="tax-children">
      <li v-for="e in shownChildren" :key="`${e.relation}:${e.parent}`">
        <TaxRel :rel="e.relation" /><a class="open xref" :data-sym="e.parent">{{
          e.parent
        }}</a>
      </li>
    </ul>
    <span v-else class="hint">none</span>
    <div v-if="elidedChildren" class="hint">+{{ elidedChildren }} more</div>
  </div>
</template>

<style scoped>
.taxlist {
  font-size: 14px;
}
.taxlist h4 {
  font-size: 12px;
  font-weight: 600;
  color: var(--muted);
  margin: 8px 0 4px;
}
.taxlist h4:first-child {
  margin-top: 4px;
}
.tax-children {
  margin: 0;
  padding: 0;
  list-style: none;
  display: flex;
  flex-wrap: wrap;
  gap: 2px 14px;
}
</style>
