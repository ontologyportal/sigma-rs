<script setup lang="ts">
import BaseDialog from "./BaseDialog.vue";
import ReleaseNotes from "./ReleaseNotes.vue";
import { useShellStore } from "../stores/shell";

const shell = useShellStore();
</script>

<template>
  <BaseDialog
    v-model="shell.versionDialog.open"
    :title="shell.versionDialog.title"
    width="min(640px, 92vw)"
  >
    <ReleaseNotes
      v-if="shell.versionDialog.notesHtml"
      class="scroll"
      :html="shell.versionDialog.notesHtml"
      :url="shell.versionDialog.releaseUrl"
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
.scroll {
  max-height: 60vh;
  overflow-y: auto;
}
</style>
