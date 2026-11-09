<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, watch, computed } from "vue";
import { useAuthStore } from "../stores/auth";
import { useShellStore } from "../stores/shell";
import { gitlabRequest } from "../api/gitlab";
import { useStatus } from "../composables/useStatus";
import BusyButton from "./BusyButton.vue";
import StatusLine from "./StatusLine.vue";

const props = defineProps<{
  server?: string;
  fixedServer?: boolean;
  settings?: boolean;
}>();
const emit = defineEmits<{
  "update:server": [server: string];
  busy: [busy: boolean];
}>();
const auth = useAuthStore();
const shell = useShellStore();
const serverUrl = ref(props.server ?? auth.gitlabUser?.server ?? "");
const connected = computed(() => {
  if (!auth.gitlabUser) return null;
  try {
    return new URL(serverUrl.value).origin === auth.gitlabUser.server
      ? auth.gitlabUser
      : null;
  } catch {
    return null;
  }
});
watch(
  () => auth.gitlabUser,
  (user) => {
    if (user && !props.fixedServer) {
      serverUrl.value = user.server;
      emit("update:server", user.server);
    }
  },
);
watch(
  () => props.server,
  (value) => {
    serverUrl.value = value ?? "";
  },
);
const token = ref("");
const busy = ref(false);
const status = useStatus();
const oauthConfigured = ref(false);
let active = true;
let pending: AbortController | null = null;
onMounted(async () => {
  await auth.initGitlab();
  if (!active) return;
  if (!serverUrl.value && auth.gitlabUser) {
    serverUrl.value = auth.gitlabUser.server;
    emit("update:server", serverUrl.value);
  }
  if (!props.settings) return;
  try {
    const config = await gitlabRequest<{ oauthConfigured: boolean }>(
      "capabilities",
    );
    if (active) oauthConfigured.value = config.oauthConfigured === true;
  } catch {
    // Token sign-in remains available when the capability check fails.
  }
});
onBeforeUnmount(() => {
  active = false;
  pending?.abort();
  emit("busy", false);
  token.value = "";
});

async function connect() {
  if (busy.value) return;
  if (!serverUrl.value.trim()) {
    status.set("Enter your GitLab server URL.", true);
    return;
  }
  let server: string;
  try {
    server = new URL(serverUrl.value.trim()).origin;
  } catch {
    status.set("Enter a complete HTTPS GitLab server URL.", true);
    return;
  }
  if (!server.startsWith("https://")) {
    status.set("Enter an HTTPS GitLab server URL.", true);
    return;
  }
  const value = token.value.trim();
  token.value = "";
  if (!value) {
    status.set("Enter your GitLab personal access token.", true);
    return;
  }
  busy.value = true;
  emit("busy", true);
  pending = new AbortController();
  try {
    const user = await auth.loginWithGitlabToken(server, value, pending.signal);
    if (active) {
      serverUrl.value = user.server ?? server;
      emit("update:server", serverUrl.value);
      status.clear();
    }
  } catch (e) {
    if (active) status.fail(e);
  } finally {
    if (active) busy.value = false;
    emit("busy", false);
  }
}

async function disconnect() {
  if (busy.value) return;
  busy.value = true;
  emit("busy", true);
  status.clear();
  try {
    await auth.logoutGitlab();
    status.set("Signed out of GitLab.");
  } catch (e) {
    status.fail(e);
  } finally {
    busy.value = false;
    emit("busy", false);
  }
}
</script>

<template>
  <div v-if="!settings" class="gitlab-connection inline tight mt-sm">
    <span class="hint">{{
      connected
        ? `GitLab: ${connected.login}`
        : "Connect your GitLab account to access private projects and push changes."
    }}</span>
    <button type="button" class="btn ghost" @click="shell.settingsOpen = true">
      {{ connected ? "Manage GitLab in Settings" : "Sign in from Settings" }}
    </button>
  </div>
  <div v-else class="gitlab-login mt-sm">
    <template v-if="auth.gitlabUser">
      <p class="hint" role="status">
        Connected to {{ auth.gitlabUser.server }} as
        {{ auth.gitlabUser.login }}.
      </p>
      <BusyButton
        :busy="busy"
        label="Sign out of GitLab"
        busy-label="Signing out..."
        @click="disconnect"
      />
    </template>
    <template v-else>
      <p
        v-if="
          oauthConfigured &&
          (!serverUrl || /^https:\/\/gitlab\.com(?:\/|$)/i.test(serverUrl))
        "
        class="hint"
      >
        <a href="/api/gitlab-auth">Sign in with GitLab</a>, or use a personal
        access token below.
      </p>
      <div>
        <label>
          GitLab server URL
          <input
            v-model="serverUrl"
            type="url"
            placeholder="https://gitlab.example.org"
            :disabled="busy"
            @change="emit('update:server', serverUrl)"
          />
        </label>
        <label>
          GitLab personal access token
          <input
            v-model="token"
            type="password"
            autocomplete="off"
            spellcheck="false"
            :disabled="busy"
            @keydown.enter.prevent="connect"
          />
        </label>
        <BusyButton
          :busy="busy"
          label="Connect with token"
          busy-label="Connecting..."
          @click="connect"
        />
      </div>
    </template>
    <StatusLine :text="status.text" :error="status.error" />
  </div>
</template>

<style scoped>
.gitlab-login .hint {
  font-size: 12px;
  line-height: 1.5;
  margin: 0 0 18px;
}
.gitlab-login label {
  display: block;
  font-size: 12px;
  font-weight: 600;
  margin: 16px 0;
}
.gitlab-login input {
  font-weight: 400;
  display: block;
  width: 100%;
  box-sizing: border-box;
  margin-top: 8px;
}
.gitlab-login :deep(.btn) {
  width: 100%;
  margin-top: 2px;
}
.gitlab-connection {
  justify-content: space-between;
  flex-wrap: wrap;
}
</style>
