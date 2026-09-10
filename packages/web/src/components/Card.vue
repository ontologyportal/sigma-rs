<script setup lang="ts">
/** A bordered content block; attributes fall through to the root div. */
</script>

<template>
  <div class="card"><slot /></div>
</template>

<style scoped>
.card {
  background: var(--card);
  border: 1px solid var(--line);
  border-radius: 10px;
  padding: 14px;
  margin-bottom: 12px;
}
</style>
