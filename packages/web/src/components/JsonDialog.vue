<script setup lang="ts">
/** A read-only view of one JSON value (a recorded contradiction, a
 *  rejected report entry, a fetched report) with copy and download. */
import { computed } from "vue";
import BaseDialog from "./BaseDialog.vue";
import CopyButton from "./CopyButton.vue";
import { downloadText } from "../utils/format";

const props = defineProps<{
  modelValue: boolean;
  title: string;
  /** The value shown; a string is shown as is (pretty-printed when it
   *  parses as JSON), anything else is serialized. */
  value: unknown;
  /** Suggested download file name. */
  filename?: string;
}>();
const emit = defineEmits<{ "update:modelValue": [value: boolean] }>();

const text = computed(() => {
  const v = props.value;
  if (typeof v === "string") {
    try {
      return JSON.stringify(JSON.parse(v), null, 2);
    } catch {
      return v;
    }
  }
  return JSON.stringify(v ?? null, null, 2);
});
</script>

<template>
  <BaseDialog
    :model-value="modelValue"
    :title="title"
    width="min(760px, 94vw)"
    @update:model-value="emit('update:modelValue', $event)"
  >
    <textarea
      class="json"
      readonly
      :value="text"
      aria-label="JSON"
      spellcheck="false"
    ></textarea>
    <template #actions>
      <CopyButton :text="text" label="Copy JSON" />
      <button
        class="btn ghost"
        type="button"
        @click="downloadText(filename ?? 'contradiction.json', text)"
      >
        Download
      </button>
      <span class="spacer"></span>
      <button
        class="btn ghost"
        type="button"
        @click="emit('update:modelValue', false)"
      >
        Close
      </button>
    </template>
  </BaseDialog>
</template>

<style scoped>
.json {
  width: 100%;
  box-sizing: border-box;
  min-height: 320px;
  max-height: 60vh;
  resize: vertical;
  font-family: var(--mono);
  font-size: 12px;
  white-space: pre;
  overflow: auto;
}
.spacer {
  flex: 1;
}
</style>
