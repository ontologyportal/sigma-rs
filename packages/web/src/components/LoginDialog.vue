<script setup lang="ts">
import BaseDialog from "./BaseDialog.vue";
import { useAuthStore } from "../stores/auth";

const auth = useAuthStore();
</script>

<template>
  <BaseDialog v-model="auth.loginDialogOpen" title="Log in required">
    <p class="hint">
      GitHub's API is rate-limited for anonymous requests. Log in with GitHub to
      continue.
    </p>
    <template #actions>
      <span></span>
      <span class="inline">
        <button
          class="btn ghost"
          type="button"
          @click="auth.loginDialogOpen = false"
        >
          Cancel
        </button>
        <a class="btn" href="/api/github-auth">Log in with GitHub</a>
      </span>
    </template>
  </BaseDialog>
</template>

<style scoped>
p {
  margin: 0 0 18px;
}
</style>
