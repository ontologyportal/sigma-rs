<script setup lang="ts">
/** One WordNet-SUMO mapping (`SearchHit.wordnet` / `ManPage.wordnet`),
 *  spelled out as `"dog" (n) — subsuming mapping` plus its gloss. The words
 *  open the sense in the WordNet tab's mapping editor. */
import { navigate } from "../../router";

const props = defineProps<{
  entry: {
    id: string;
    words: string;
    pos: string;
    mapping: string;
    gloss: string;
  };
}>();

function open(e: Event) {
  e.preventDefault();
  e.stopPropagation();
  navigate("wordnet", { q: props.entry.id, s: props.entry.id });
}
</script>

<template>
  <div class="wn-entry">
    <div>
      <a
        class="wn-open"
        :title="`Open ${entry.id} in the WordNet mapping editor`"
        @click="open"
        >"{{ entry.words }}"</a
      >
      ({{ entry.pos }}) — {{ entry.mapping }} mapping
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
.wn-open {
  color: var(--accent);
  cursor: pointer;
}
.wn-open:hover {
  text-decoration: underline;
}
</style>
