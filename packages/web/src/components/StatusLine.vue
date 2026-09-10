<script setup lang="ts">
/** A one-line muted status/log message that turns red for errors. Always
 *  rendered (empty when there is no text) so the layout does not jump. */
defineProps<{
  text: string;
  error?: boolean;
}>();
</script>

<template>
  <div class="hint status-line" :class="{ bad: error }">{{ text }}</div>
</template>

<style scoped>
.status-line {
  min-height: 1.2em;
}
</style>
