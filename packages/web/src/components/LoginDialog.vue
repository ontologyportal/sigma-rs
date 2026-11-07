<script setup lang="ts">
import { nextTick, ref, watch, onBeforeUnmount } from "vue";
import ProviderIcon from "./ProviderIcon.vue";
import { navigate } from "../router";
import BaseDialog from "./BaseDialog.vue";
import GitLabLogin from "./GitLabLogin.vue";
import { useAuthStore } from "../stores/auth";
import { errMsg } from "../utils/format";

const auth = useAuthStore();
const token = ref("");
const providerTabs = ref<HTMLDivElement | null>(null);
const busy = ref(false);
const gitlabBusy = ref(false);
const error = ref("");
let request: AbortController | null = null;

function reset() {
  request?.abort();
  request = null;
  token.value = "";
  error.value = "";
  busy.value = false;
  gitlabBusy.value = false;
}
watch(() => auth.loginDialogOpen, reset, { flush: "sync" });
onBeforeUnmount(reset);

function selectProvider(provider: "github" | "gitlab") {
  if (busy.value || gitlabBusy.value) return;
  token.value = "";
  error.value = "";
  auth.loginProvider = provider;
}
async function moveTab(event: KeyboardEvent) {
  if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
  event.preventDefault();
  if (busy.value || gitlabBusy.value) return;
  selectProvider(
    event.key === "Home"
      ? "github"
      : event.key === "End"
        ? "gitlab"
        : auth.loginProvider === "github"
          ? "gitlab"
          : "github",
  );
  await nextTick();
  providerTabs.value
    ?.querySelector<HTMLButtonElement>('[aria-selected="true"]')
    ?.focus();
}
async function useToken() {
  if (busy.value || gitlabBusy.value) return;
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
    chooseRepo: auth.provider,
    server: auth.gitlabUser?.server,
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
    :title="auth.signedIn ? 'Your account' : 'Sign in'"
    class="login-dialog"
    width="min(480px, 94vw)"
    :dismissible="!gitlabBusy"
  >
    <template #heading>
      <div class="login-heading">
        <h3>{{ auth.signedIn ? "Your account" : "Sign in" }}</h3>
        <div
          ref="providerTabs"
          class="provider-tabs"
          role="tablist"
          aria-label="Sign-in provider"
          @keydown="moveTab"
        >
          <button
            v-for="provider in ['github', 'gitlab'] as const"
            :key="provider"
            :id="`${provider}-tab`"
            type="button"
            role="tab"
            :aria-selected="auth.loginProvider === provider"
            :aria-controls="`${provider}-panel`"
            :tabindex="auth.loginProvider === provider ? 0 : -1"
            :disabled="busy || gitlabBusy"
            @click="selectProvider(provider)"
          >
            <ProviderIcon :provider="provider" :size="22" />
            {{ provider === "github" ? "GitHub" : "GitLab" }}
          </button>
        </div>
      </div>
    </template>
    <div class="account-card" v-if="auth.signedIn">
      <div class="account-identity">
        <div class="account-name">
          <strong>{{ auth.accountName }}</strong>
          <ProviderIcon
            v-if="auth.provider"
            :provider="auth.provider"
            :size="18"
          />
        </div>
        <span>{{
          auth.provider === "github" ? "GitHub" : auth.gitlabUser?.server
        }}</span>
      </div>
      <button
        class="account-signout"
        type="button"
        @click="signOut"
        :disabled="busy || gitlabBusy"
      >
        Sign out
      </button>
    </div>
    <button
      v-if="auth.signedIn"
      class="btn ghost choose-repository"
      type="button"
      :disabled="busy || gitlabBusy"
      @click="chooseRepository"
    >
      Choose repository
    </button>
    <p v-if="!auth.signedIn" class="intro">
      Connect your account to save changes to your repositories.
    </p>
    <div class="provider-content">
      <section
        v-if="auth.loginProvider === 'github'"
        id="github-panel"
        role="tabpanel"
        aria-labelledby="github-tab"
      >
        <a
          class="btn oauth-button provider-button"
          :href="busy || gitlabBusy ? undefined : '/api/github-auth'"
          :aria-disabled="busy || gitlabBusy"
          @click="(busy || gitlabBusy) && $event.preventDefault()"
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
            :disabled="busy || gitlabBusy"
            aria-describedby="github-token-help"
          />
          <p id="github-token-help" class="hint">
            Your token is used for this browser session.
          </p>
          <p v-if="error" class="login-error" role="alert">{{ error }}</p>
          <button
            class="btn token-submit"
            type="submit"
            :disabled="busy || gitlabBusy || !token.trim()"
          >
            {{ busy ? "Checking token..." : "Connect with token" }}
          </button>
        </form>
      </section>
      <section
        v-else
        id="gitlab-panel"
        role="tabpanel"
        aria-labelledby="gitlab-tab"
      >
        <fieldset :disabled="busy" class="gitlab-fields">
          <GitLabLogin
            v-if="auth.loginDialogOpen"
            :server="auth.loginServer"
            :dialog="true"
            @update:server="auth.loginServer = $event"
            @busy="gitlabBusy = $event"
          />
        </fieldset>
      </section>
    </div>
    <p v-if="auth.signedIn" class="switch-notice" role="status">
      Connecting another account will sign you out of your current account.
    </p>
    <template #actions>
      <button
        class="btn ghost"
        type="button"
        :disabled="gitlabBusy"
        @click="auth.loginDialogOpen = false"
      >
        {{ auth.signedIn ? "Done" : "Cancel" }}
      </button>
    </template>
  </BaseDialog>
</template>

<style scoped>
.login-heading {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
  margin-bottom: 20px;
}
.login-heading h3 {
  margin: 0;
  white-space: nowrap;
}
.login-heading .provider-tabs {
  margin: 0;
}
.login-heading .provider-tabs button {
  padding: 8px 10px;
  gap: 6px;
  font-size: 13px;
}
@media (max-width: 420px) {
  .login-heading h3 {
    font-size: 20px;
  }
  .login-heading .provider-tabs button {
    padding: 7px;
    font-size: 12px;
    gap: 4px;
  }
  .login-heading .provider-tabs :deep(svg) {
    width: 18px;
    height: 18px;
  }
}

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
.provider-tabs {
  display: flex;
  gap: 4px;
  padding: 4px;
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--bg);
  margin-bottom: 24px;
}
.provider-tabs button {
  flex: 1;
  display: flex;
  justify-content: center;
  align-items: center;
  gap: 10px;
  padding: 12px;
  border: 1px solid transparent;
  border-radius: 7px;
  background: transparent;
  color: var(--muted);
  font: inherit;
  font-weight: 600;
  cursor: pointer;
}
.provider-tabs button[aria-selected="true"] {
  background: var(--card);
  border-color: var(--line);
  color: var(--fg);
  box-shadow: 0 1px 3px rgb(0 0 0 / 8%);
}
.provider-tabs button:focus-visible {
  outline: 2px solid currentColor;
  outline-offset: 2px;
}
.provider-tabs button:disabled {
  cursor: default;
  opacity: 0.6;
}
.provider-description {
  color: var(--muted);
  font-size: 13px;
  line-height: 1.5;
  margin: 0 0 20px;
}
.provider-content :deep(.gitlab-login) {
  margin-top: 0;
}
.gitlab-fields {
  border: 0;
  padding: 0;
  margin: 0;
  min-width: 0;
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
.switch-notice {
  padding: 12px;
  margin: 16px 0 0;
  border-radius: 8px;
  background: var(--bg);
  color: var(--muted);
  font-size: 12px;
  line-height: 1.5;
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
