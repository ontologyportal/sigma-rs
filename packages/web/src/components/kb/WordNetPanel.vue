<script setup lang="ts">
/** The WordNet lexicon card: the on/off toggle and the fetched mapping
 *  files with their sizes. Enable/disable acts on the LIVE session
 *  immediately (install / clear), separate from the persisted setting that
 *  decides what the NEXT boot does. */
import { computed, ref } from "vue";
import { useWordNetStore } from "../../stores/wordnet";
import { formatSize } from "../../utils/format";

const wordnet = useWordNetStore();

const toggling = ref(false);

const total = computed(() =>
  formatSize(wordnet.files.reduce((sum, f) => sum + f.size, 0)),
);

async function onChange(e: Event) {
  const enabled = (e.target as HTMLInputElement).checked;
  toggling.value = true;
  try {
    wordnet.setEnabled(enabled);
    if (enabled) await wordnet.install();
    else await wordnet.clear();
  } finally {
    toggling.value = false;
  }
}
</script>

<template>
  <label class="check toggle">
    <input
      type="checkbox"
      :checked="wordnet.enabled"
      :disabled="toggling"
      @change="onChange"
    />
    Enable WordNet search
  </label>
  <ul class="results list">
    <li v-if="!wordnet.enabled" class="hint">
      Disabled — search runs without WordNet synonym expansion.
    </li>
    <li v-else-if="!wordnet.files.length" class="hint">Loading…</li>
    <template v-else>
      <li v-for="f in wordnet.files" :key="f.name" class="loaded-row">
        <span class="mono">{{ f.name }}</span>
        <span class="hint">{{ formatSize(f.size) }}</span>
      </li>
      <li class="loaded-row">
        <span><b>Total</b></span>
        <span class="hint">{{ total }}</span>
      </li>
    </template>
  </ul>
</template>

<style scoped>
.toggle,
.list {
  margin-top: 8px;
}
</style>
