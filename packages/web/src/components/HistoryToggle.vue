<script setup lang="ts">
/** The comfortable layout's run-history toggle: a ghost button with the
 *  history icon (v-model `open`). */
defineProps<{ open: boolean; title?: string }>();
const emit = defineEmits<{ "update:open": [open: boolean] }>();
</script>

<template>
  <button
    class="btn ghost history-toggle"
    type="button"
    :aria-expanded="open"
    :title="title ?? 'Earlier runs'"
    @click="emit('update:open', !open)"
  >
    <!-- A clock with a counter-clockwise arrow. -->
    <svg
      viewBox="0 0 16 16"
      width="14"
      height="14"
      fill="currentColor"
      aria-hidden="true"
    >
      <path
        d="m1.643 3.143-1.216-1.216A.25.25 0 0 0 0 2.104V5.75c0 .138.112.25.25.25h3.646a.25.25 0 0 0 .177-.427L2.715 4.215a6.5 6.5 0 1 1-1.18 4.458.75.75 0 1 0-1.493.154 8.001 8.001 0 1 0 1.6-5.684ZM7.75 4a.75.75 0 0 1 .75.75v2.992l2.028.812a.75.75 0 0 1-.557 1.392l-2.5-1A.751.751 0 0 1 7 8.25v-3.5A.75.75 0 0 1 7.75 4Z"
      />
    </svg>
    History
  </button>
</template>

<style scoped>
.history-toggle {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}
</style>
