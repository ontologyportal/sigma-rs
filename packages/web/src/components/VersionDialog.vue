<script setup lang="ts">
import BaseDialog from "./BaseDialog.vue";
import { useShellStore } from "../stores/shell";

const shell = useShellStore();
</script>

<template>
  <BaseDialog
    v-model="shell.versionDialog.open"
    :title="shell.versionDialog.title"
    width="min(640px, 92vw)"
  >
    <div
      v-if="shell.versionDialog.notesHtml"
      class="notes"
      v-html="shell.versionDialog.notesHtml"
    />
    <p v-else class="hint">{{ shell.versionDialog.body }}</p>
    <template #actions>
      <span></span>
      <button
        class="btn ghost"
        type="button"
        @click="shell.versionDialog.open = false"
      >
        Got it
      </button>
    </template>
  </BaseDialog>
</template>

<style scoped>
p {
  margin: 0 0 18px;
}
.notes {
  max-height: 60vh;
  overflow-y: auto;
  margin: 0 0 18px;
}
.notes :deep(img) {
  max-width: 100%;
  height: auto;
  border-radius: 6px;
}
.notes :deep(h4) {
  margin: 16px 0 6px;
  font-size: 13px;
}
.notes :deep(p),
.notes :deep(ul) {
  margin: 0 0 10px;
}
.notes :deep(:first-child) {
  margin-top: 0;
}
</style>
