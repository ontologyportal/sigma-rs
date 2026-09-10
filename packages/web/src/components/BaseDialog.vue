<script setup lang="ts">
import { onMounted, ref, watch } from "vue";

const props = withDefaults(
  defineProps<{
    /** Open state (v-model); true calls `showModal()`, false calls `close()`. */
    modelValue: boolean;
    /** Heading rendered above the body when given. */
    title?: string;
    /** CSS width of the dialog box. */
    width?: string;
  }>(),
  { title: "", width: "min(360px, 92vw)" },
);

const emit = defineEmits<{
  "update:modelValue": [value: boolean];
  /** Fired when the native dialog closes (Escape, a close button, or the model turning false). */
  close: [];
}>();

const el = ref<HTMLDialogElement | null>(null);

function onClose() {
  emit("update:modelValue", false);
  emit("close");
}

function sync(open: boolean) {
  const d = el.value;
  if (!d) return;
  if (open && !d.open) d.showModal();
  else if (!open && d.open) d.close();
}

watch(() => props.modelValue, sync, { flush: "post" });
onMounted(() => sync(props.modelValue));
</script>

<template>
  <dialog ref="el" :style="{ width }" @close="onClose">
    <h3 v-if="title">{{ title }}</h3>
    <slot />
    <div v-if="$slots.actions" class="dialog-actions">
      <slot name="actions" />
    </div>
  </dialog>
</template>

<style scoped>
dialog {
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--card);
  color: var(--fg);
  padding: 22px 28px;
  box-sizing: border-box;
}
dialog::backdrop {
  background: rgba(0, 0, 0, 0.45);
}
h3 {
  margin: 0 0 16px;
  font-size: 14px;
}
.dialog-actions {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  margin-top: 12px;
}
.dialog-actions :deep(.spacer) {
  flex: 1 1 auto;
}
</style>
