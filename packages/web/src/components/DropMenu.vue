<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useOutsideClick } from "../composables/useOutsideClick";

const props = defineProps<{
  /** Open state (v-model). */
  modelValue: boolean;
  /** The button the menu hangs under; clicks on it do not count as outside. */
  anchor: HTMLElement | null;
}>();

const emit = defineEmits<{ "update:modelValue": [value: boolean] }>();

const menu = ref<HTMLElement | null>(null);
const pos = ref<{ left: string; top: string }>({ left: "0px", top: "0px" });

function close() {
  emit("update:modelValue", false);
}

watch(
  () => props.modelValue,
  (open) => {
    if (!open || !props.anchor) return;
    const r = props.anchor.getBoundingClientRect();
    pos.value = {
      left: `${Math.round(r.left)}px`,
      top: `${Math.round(r.bottom + 4)}px`,
    };
  },
  { immediate: true },
);

useOutsideClick(
  [menu, () => props.anchor],
  close,
  computed(() => props.modelValue),
);
</script>

<template>
  <Teleport to="body">
    <div v-if="modelValue" ref="menu" class="dl-menu" role="menu" :style="pos">
      <slot :close="close" />
    </div>
  </Teleport>
</template>

<style scoped>
.dl-menu {
  position: fixed;
  z-index: 300;
  min-width: 220px;
  padding: 4px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--card);
  box-shadow: 0 6px 20px rgba(0, 0, 0, 0.25);
}
.dl-menu > :deep(button) {
  display: block;
  width: 100%;
  text-align: left;
  font: inherit;
  font-size: 13px;
  background: none;
  border: none;
  border-radius: 6px;
  color: var(--fg);
  cursor: pointer;
  padding: 8px 10px;
}
.dl-menu > :deep(button:hover) {
  background: color-mix(in srgb, var(--accent) 10%, transparent);
  color: var(--accent);
}
.dl-menu > :deep(button[disabled]) {
  color: var(--muted);
  cursor: default;
  background: none;
}
</style>
