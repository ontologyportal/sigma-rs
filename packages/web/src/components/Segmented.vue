<script setup lang="ts" generic="T extends string">
/** A row of mutually exclusive buttons (v-model), for small mode switches. */
defineProps<{
  modelValue: T;
  options: { value: T; label: string; title?: string }[];
  /** Accessible name of the group. */
  label: string;
  disabled?: boolean;
}>();

const emit = defineEmits<{ "update:modelValue": [value: T] }>();
</script>

<template>
  <div class="segmented" role="radiogroup" :aria-label="label">
    <button
      v-for="o in options"
      :key="o.value"
      type="button"
      role="radio"
      :aria-checked="o.value === modelValue"
      :title="o.title"
      :disabled="disabled"
      @click="emit('update:modelValue', o.value)"
    >
      {{ o.label }}
    </button>
  </div>
</template>

<style scoped>
/* The shared 41px control height (see styles.css), so a segmented control
   lines up with inputs and selects in the same row. */
.segmented {
  display: inline-flex;
  align-items: stretch;
  height: 41px;
  padding: 3px;
  gap: 2px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--bg);
}
button {
  font: inherit;
  font-size: 13px;
  color: var(--muted);
  background: none;
  border: none;
  border-radius: 6px;
  padding: 5px 12px;
  cursor: pointer;
}
button:hover:not(:disabled) {
  color: var(--fg);
}
button[aria-checked="true"] {
  background: color-mix(in srgb, var(--accent) 14%, transparent);
  color: var(--accent);
  font-weight: 600;
}
button:disabled {
  cursor: default;
  opacity: 0.6;
}
</style>
