<script setup lang="ts">
import { ref, watch, onBeforeUnmount } from "vue";
import ProviderIcon from "./ProviderIcon.vue";
import { navigate } from "../router";
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
function chooseRepository() {
  auth.loginDialogOpen = false;
  return navigate("kb", {
    view: "constituents",
    chooseRepo: "github",
  });
}
function signOut() {
  auth.loginDialogOpen = false;
  auth.logoutDialogOpen = true;
}
</script>

<template>
  <BaseDialog
    v-model="auth.loginDialogOpen"
    :title="auth.githubSignedIn ? 'Your account' : 'Sign in'"
    class="login-dialog"
    width="min(480px, 94vw)"
  >
    <div class="account-card" v-if="auth.githubSignedIn">
      <div class="account-identity">
        <div class="account-name">
          <strong>{{ auth.accountName }}</strong>
        </div>
        <span>GitHub</span>
      </div>
      <button
        class="account-signout"
        type="button"
        @click="signOut"
        :disabled="busy"
      >
        Sign out
      </button>
    </div>
    <button
      v-if="auth.githubSignedIn"
      class="btn ghost choose-repository"
      type="button"
      :disabled="busy"
      @click="chooseRepository"
    >
      Choose repository
    </button>
    <p v-if="!auth.githubSignedIn" class="intro">
      Connect your account to save changes to your repositories.
    </p>
    <a
      class="btn oauth-button provider-button"
      :href="busy ? undefined : '/api/github-auth'"
      :aria-disabled="busy"
      @click="busy && $event.preventDefault()"
    >
      <ProviderIcon provider="github" :size="22" />
      <span>Continue with GitHub</span>
    </a>
    <div class="separator"><span>or</span></div>
    <form id="github-token-form" @submit.prevent="useToken">
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
        Your token is used for this browser session.
      </p>
      <p v-if="error" class="login-error" role="alert">{{ error }}</p>
      <button
        class="btn token-submit"
        type="submit"
        :disabled="busy || !token.trim()"
      >
        {{ busy ? "Checking token..." : "Connect with token" }}
      </button>
    </form>
    <template #actions>
      <button
        class="btn ghost"
        type="button"
        @click="auth.loginDialogOpen = false"
      >
        {{ auth.githubSignedIn ? "Done" : "Cancel" }}
      </button>
    </template>
  </BaseDialog>
</template>

<style scoped>
.login-dialog {
  padding: 28px;
  border-radius: 16px;
  box-shadow: 0 24px 80px rgb(0 0 0 / 20%);
}
:deep(.dialog-actions) {
  justify-content: flex-end;
  margin-top: 24px;
}
:deep(.dialog-actions .btn) {
  min-width: 88px;
}

:deep(h3) {
  font-size: 24px;
  letter-spacing: -0.02em;
  margin-bottom: 8px;
}
.intro {
  color: var(--muted);
  line-height: 1.55;
  margin: 0 0 24px;
}
input {
  width: 100%;
  box-sizing: border-box;
}
@media (max-width: 640px) {
  .login-dialog {
    padding: 20px;
  }
}
.provider-button {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 10px;
  min-height: 48px;
  width: 100%;
  box-sizing: border-box;
  padding: 12px 30px 12px 14px;
  position: relative;
  font-weight: 600;
  text-decoration: none;
}
.provider-button > svg {
  flex-shrink: 0;
}
.oauth-button {
  width: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 12px;
  box-sizing: border-box;
  padding: 12px 16px;
  font-weight: 600;
  text-decoration: none;
}
.separator {
  display: flex;
  align-items: center;
  gap: 12px;
  color: var(--muted);
  font-size: 11px;
  margin: 20px 0;
}
.separator::before,
.separator::after {
  content: "";
  flex: 1;
  height: 1px;
  background: var(--line);
}
form {
  margin-top: 18px;
  padding: 0;
}
form label {
  font-size: 12px;
  font-weight: 600;
}
form input {
  margin-top: 8px;
}
form .hint {
  margin: 10px 0 16px;
  font-size: 12px;
  line-height: 1.5;
}
.token-submit {
  width: 100%;
}
.login-error {
  color: var(--warn);
  font-size: 12px;
}
.account-card {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 14px;
  border: 1px solid var(--line);
  border-radius: 8px;
  margin-bottom: 22px;
}
.account-name {
  display: flex;
  align-items: center;
  gap: 8px;
}
.choose-repository {
  width: 100%;
  margin: -8px 0 22px;
}
.account-identity {
  min-width: 0;
  flex: 1;
  display: grid;
  gap: 4px;
}
.account-identity strong {
  font-size: 13px;
}
.account-identity > span {
  color: var(--muted);
  font-size: 11px;
  overflow-wrap: anywhere;
}
.account-signout {
  border: 0;
  padding: 4px 0;
  background: none;
  color: var(--muted);
  font: inherit;
  font-size: 12px;
  cursor: pointer;
}
.account-signout:hover {
  color: var(--fg);
}
</style>
