<script setup lang="ts">
import TaxRel from "./TaxRel.vue";

/** One level of the ancestor listing: a relation pill + man-page link per
 *  node, recursing into `children` (the node's own parents) when present. */
export interface TaxNode {
  relation: string;
  sym: string;
  /** The node's parents; null for a leaf (a root, or a symbol already
   *  expanded on a shorter path). */
  children: TaxNode[] | null;
}

defineProps<{
  /** The nodes to list at this level. */
  nodes: TaxNode[];
}>();
</script>

<template>
  <ul class="tax-branch">
    <li v-for="n in nodes" :key="`${n.relation}:${n.sym}`">
      <TaxRel :rel="n.relation" /><a class="open xref" :data-sym="n.sym">{{
        n.sym
      }}</a>
      <TaxonomyBranch v-if="n.children?.length" :nodes="n.children" />
    </li>
  </ul>
</template>

<style scoped>
.tax-branch {
  margin: 0;
  padding-left: 18px;
  list-style: none;
}
.tax-branch li {
  margin: 2px 0;
}
</style>
