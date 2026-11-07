<script setup lang="ts">
import { ref } from "vue";
import BaseDialog from "./BaseDialog.vue";
import { useAuthStore } from "../stores/auth";

const auth = useAuthStore();
const busy = ref(false);

async function confirm() {
  busy.value = true;
  try {
    await auth.logout();
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <BaseDialog v-model="auth.logoutDialogOpen" title="Sign out?">
    <p class="hint">
      Sign out of {{ auth.accountName }}? Your saved files and local edits will
      stay in this browser.
    </p>
    <template #actions>
      <span></span>
      <span class="inline">
        <button
          class="btn ghost"
          type="button"
          @click="auth.logoutDialogOpen = false"
        >
          Cancel
        </button>
        <button class="btn" type="button" :disabled="busy" @click="confirm">
          Sign out
        </button>
      </span>
    </template>
  </BaseDialog>
</template>

<style scoped>
p {
  margin: 0 0 18px;
}
</style>
