<script setup lang="ts">
import BaseDialog from "./BaseDialog.vue";
import { useShellStore } from "../stores/shell";

const shell = useShellStore();
</script>

<template>
  <BaseDialog
    v-model="shell.versionDialog.open"
    :title="shell.versionDialog.title"
  >
    <p class="hint">{{ shell.versionDialog.body }}</p>
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
</style>
