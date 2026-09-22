<script setup lang="ts">
/** A bordered content block; attributes fall through to the root div. */
defineProps<{
  /** Bold section heading rendered above the body. */
  title?: string;
  /** Muted one-line description under the heading (or the `description`
   *  slot, for markup). */
  description?: string;
  /** whether to let the card grow to fit its parent */
  grow?: boolean;
  /** whether to shrink the card to fit its parent */
  shrink?: boolean;
}>();
</script>

<template>
  <div :class="{ card: true, grow, shrink }">
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
  width: 100%;
  margin-bottom: 12px;
}
.grow {
  flex-grow: 1;
}

.shrink {
  flex-shrink: 1;
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
