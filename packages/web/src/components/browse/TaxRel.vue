<script setup lang="ts">
/** A taxonomy edge-kind pill (`subclass`, `instance`, ...), color-coded per
 *  relation to match the graph's edges; unknown relations render muted. */
defineProps<{
  /** The relation name shown in the pill. */
  rel: string;
}>();
</script>

<template>
  <span class="tax-rel" :data-rel="rel">{{ rel }}</span>
</template>

<style scoped>
.tax-rel {
  display: inline-block;
  font-size: 10px;
  font-weight: 600;
  line-height: 1.5;
  padding: 0 6px;
  border-radius: 999px;
  margin-right: 5px;
  vertical-align: 1px;
  background: color-mix(in srgb, var(--muted) 16%, transparent);
  color: var(--muted);
}
.tax-rel[data-rel="subclass"] {
  background: color-mix(in srgb, var(--accent) 15%, transparent);
  color: var(--accent);
}
.tax-rel[data-rel="instance"] {
  background: color-mix(in srgb, var(--ok) 15%, transparent);
  color: var(--ok);
}
.tax-rel[data-rel="subrelation"] {
  background: color-mix(in srgb, var(--op) 15%, transparent);
  color: var(--op);
}
.tax-rel[data-rel="subAttribute"] {
  background: color-mix(in srgb, var(--warn) 20%, transparent);
  color: var(--warn);
}
</style>
