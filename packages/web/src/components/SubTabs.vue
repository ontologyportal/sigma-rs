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
  <div class="subtabs seg" role="tablist">
    <button
      v-for="o in options"
      :key="o.value"
      type="button"
      role="tab"
      :aria-selected="modelValue === o.value"
      @click="emit('update:modelValue', o.value)"
    >
      {{ o.label }}
      <span v-if="o.count" class="count-badge">{{ o.count }}</span>
    </button>
  </div>
</template>

<style scoped>
.subtabs {
  display: inline-flex;
  margin-bottom: 14px;
  border-radius: 999px;
}
.subtabs button {
  border-radius: 999px;
  padding: 5px 14px;
}
</style>
