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
  <BaseDialog v-model="auth.logoutDialogOpen" title="Log out of GitHub?">
    <p class="hint">
      You will need to log in again to submit changes or raise the API rate
      limit.
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
          Log out
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
