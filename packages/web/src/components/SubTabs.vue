<script setup lang="ts" generic="T extends string">
/** A view's sub-tab switch -- a compact segmented pill, not another
 *  underlined tab strip (nav.tabs already fills that role one level up;
 *  stacking a second one, even relabeled, reads as nested navigation).
 *  Emits the picked value; the parent owns where it lives (the URL). */
defineProps<{
  options: { value: T; label: string; count?: number }[];
  modelValue: T;
}>();

const emit = defineEmits<{ "update:modelValue": [value: T] }>();
</script>

<template>
  <div class="subtabs" role="tablist">
    <button
      v-for="o in options"
      :key="o.value"
      type="button"
      role="tab"
      :aria-selected="modelValue === o.value"
      @click="emit('update:modelValue', o.value)"
    >
      {{ o.label }}
      <span v-if="o.count" class="hint">({{ o.count }})</span>
    </button>
  </div>
</template>

<style scoped>
.subtabs {
  display: inline-flex;
  gap: 2px;
  margin-bottom: 14px;
  padding: 3px;
  border-radius: 999px;
  background: var(--card);
  border: 1px solid var(--line);
}
.subtabs button {
  font: inherit;
  font-size: 12px;
  font-weight: 600;
  background: none;
  border: none;
  border-radius: 999px;
  color: var(--muted);
  cursor: pointer;
  padding: 5px 14px;
}
.subtabs button[aria-selected="true"] {
  color: var(--bg);
  background: var(--accent);
}
/* The count span (.hint) would otherwise stay --muted even inside the
   solid-accent selected pill, which reads too low-contrast there. */
.subtabs button[aria-selected="true"] .hint {
  color: inherit;
  opacity: 0.85;
}
</style>
