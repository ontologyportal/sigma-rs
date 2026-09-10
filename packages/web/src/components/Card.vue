<script setup lang="ts">
/** A bordered content block; attributes fall through to the root div. */
defineProps<{
  /** Bold section heading rendered above the body. */
  title?: string;
  /** Muted one-line description under the heading (or the `description`
   *  slot, for markup). */
  description?: string;
}>();
</script>

<template>
  <div class="card">
    <div
      v-if="title || description || $slots.description || $slots.header"
      class="card-head"
    >
      <div>
        <div v-if="title" class="card-title">{{ title }}</div>
        <div v-if="description || $slots.description" class="hint">
          <slot name="description">{{ description }}</slot>
        </div>
      </div>
      <slot name="header" />
    </div>
    <slot />
  </div>
</template>

<style scoped>
.card {
  background: var(--card);
  border: 1px solid var(--line);
  border-radius: 10px;
  padding: 14px;
  margin-bottom: 12px;
}
/* Heading row: title + description on the left, an optional header slot
   (buttons, status) pushed right; wraps on narrow screens. */
.card-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}
.card-title {
  font-weight: 600;
}
</style>
