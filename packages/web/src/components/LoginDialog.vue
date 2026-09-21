<script setup lang="ts">
import { ref, watch, onBeforeUnmount } from "vue";
import BaseDialog from "./BaseDialog.vue";
import { useAuthStore } from "../stores/auth";
import { errMsg } from "../utils/format";

const auth = useAuthStore();
const token = ref("");
const busy = ref(false);
const error = ref("");
let request: AbortController | null = null;

function reset() {
  request?.abort();
  request = null;
  token.value = "";
  error.value = "";
  busy.value = false;
}

watch(() => auth.loginDialogOpen, reset, { flush: "sync" });
onBeforeUnmount(reset);

async function useToken() {
  if (busy.value) return;
  const pending = new AbortController();
  request = pending;
  busy.value = true;
  error.value = "";
  try {
    await auth.loginWithToken(token.value, pending.signal);
  } catch (e) {
    if (!pending.signal.aborted) error.value = errMsg(e);
  } finally {
    if (request === pending) {
      request = null;
      busy.value = false;
    }
  }
}
</script>

<template>
  <BaseDialog v-model="auth.loginDialogOpen" title="Connect to GitHub">
    <p class="hint">
      Log in with GitHub or use an access token to submit pull requests.
    </p>
    <form @submit.prevent="useToken">
      <label for="github-token">GitHub access token</label>
      <input
        id="github-token"
        v-model="token"
        type="password"
        autocomplete="off"
        spellcheck="false"
        :disabled="busy"
        aria-describedby="github-token-help"
      />
      <p id="github-token-help" class="hint">
        Kept only until you reload or close this page. No redirect is needed.
      </p>
      <p v-if="error" role="alert">{{ error }}</p>
      <button class="btn" type="submit" :disabled="busy || !token.trim()">
        {{ busy ? "Checking token..." : "Use access token" }}
      </button>
    </form>
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
input {
  width: 100%;
  box-sizing: border-box;
  margin: 6px 0;
}
</style>
