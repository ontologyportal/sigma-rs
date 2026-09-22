<script setup lang="ts">
import { nextTick, ref, watch, onBeforeUnmount } from "vue";
import BaseDialog from "./BaseDialog.vue";
import { useAuthStore } from "../stores/auth";
import { errMsg } from "../utils/format";

const auth = useAuthStore();
const token = ref("");
const tokenOpen = ref(false);
const tokenInput = ref<HTMLInputElement | null>(null);
const busy = ref(false);
const error = ref("");
let request: AbortController | null = null;

function reset() {
  request?.abort();
  request = null;
  token.value = "";
  tokenOpen.value = false;
  error.value = "";
  busy.value = false;
}

watch(() => auth.loginDialogOpen, reset, { flush: "sync" });
onBeforeUnmount(reset);

async function toggleToken() {
  tokenOpen.value = !tokenOpen.value;
  if (tokenOpen.value) {
    await nextTick();
    tokenInput.value?.focus();
  }
}

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
  <BaseDialog
    v-model="auth.loginDialogOpen"
    title="Connect to GitHub"
    width="min(440px, 92vw)"
  >
    <p class="hint">
      Log in with GitHub or use an access token to submit pull requests.
    </p>
    <template #actions>
      <div class="login-actions">
        <div class="login-buttons">
          <button
            class="btn ghost token-toggle"
            type="button"
            :aria-expanded="tokenOpen"
            aria-controls="github-token-form"
            @click="toggleToken"
          >
            Use Access Token
            <span
              class="caret"
              :class="{ expanded: tokenOpen }"
              aria-hidden="true"
              >&#9662;</span
            >
          </button>
          <button
            class="btn ghost"
            type="button"
            @click="auth.loginDialogOpen = false"
          >
            Cancel
          </button>
          <a class="btn" href="/api/github-auth">Log in with GitHub</a>
        </div>
        <form
          v-if="tokenOpen"
          id="github-token-form"
          @submit.prevent="useToken"
        >
          <label for="github-token">GitHub access token</label>
          <input
            id="github-token"
            ref="tokenInput"
            v-model="token"
            type="password"
            autocomplete="off"
            spellcheck="false"
            :disabled="busy"
            aria-describedby="github-token-help"
          />
          <p id="github-token-help" class="hint">
            Kept only until you reload or close this page. No redirect is
            needed.
          </p>
          <p v-if="error" role="alert">{{ error }}</p>
          <button class="btn" type="submit" :disabled="busy || !token.trim()">
            {{ busy ? "Checking token..." : "Submit" }}
          </button>
        </form>
      </div>
    </template>
  </BaseDialog>
</template>

<style scoped>
.login-actions {
  width: 100%;
  min-width: 0;
}
.login-buttons {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 6px;
}
.login-buttons .btn {
  padding: 9px 10px;
  font-size: 12px;
  white-space: nowrap;
}
.token-toggle {
  margin-right: auto;
}
.caret {
  display: inline-block;
  margin-left: 6px;
  transition: transform 0.15s;
}
.caret.expanded {
  transform: rotate(180deg);
}
form {
  margin-top: 18px;
  padding-top: 18px;
  border-top: 1px solid var(--line);
}

p {
  margin: 0 0 18px;
}
input {
  width: 100%;
  box-sizing: border-box;
  margin: 6px 0;
}
</style>
