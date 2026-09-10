<script setup lang="ts">
/** A `<details>` block with a muted one-line summary, for secondary content
 *  (raw output, sources, advanced options). `open` is the initial state;
 *  `toggle` reports every change. */
defineProps<{
  summary: string;
  open?: boolean;
}>();

const emit = defineEmits<{ toggle: [open: boolean] }>();

function onToggle(e: Event) {
  emit("toggle", (e.target as HTMLDetailsElement).open);
}
</script>

<template>
  <details class="disclosure" :open="open" @toggle="onToggle">
    <summary class="hint">{{ summary }}</summary>
    <slot />
  </details>
</template>

<style scoped>
.disclosure {
  margin-top: 10px;
}
summary {
  cursor: pointer;
}
.disclosure :deep(pre) {
  font-family: var(--mono);
  font-size: 12px;
  white-space: pre-wrap;
  background: var(--bg);
  border: 1px solid var(--line);
  border-radius: 7px;
  padding: 10px;
  overflow-x: auto;
}
</style>
