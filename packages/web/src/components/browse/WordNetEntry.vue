<script setup lang="ts">
/** One WordNet-SUMO mapping (`SearchHit.wordnet` / `ManPage.wordnet`),
 *  spelled out as `"dog" (n) — subsuming mapping` plus its gloss. */
defineProps<{
  entry: { words: string; pos: string; mapping: string; gloss: string };
}>();
</script>

<template>
  <div class="wn-entry">
    <div>
      "{{ entry.words }}" ({{ entry.pos }}) — {{ entry.mapping }} mapping
    </div>
    <div class="hint">{{ entry.gloss }}</div>
  </div>
</template>

<style scoped>
.wn-entry {
  font-size: 13px;
  margin-top: 4px;
  padding-left: 8px;
  border-left: 2px solid var(--line);
}
.wn-entry + .wn-entry {
  margin-top: 8px;
}
</style>
