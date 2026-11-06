<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, watch, computed } from "vue";
import { useAuthStore } from "../stores/auth";
import { gitlabRequest } from "../api/gitlab";
import { useStatus } from "../composables/useStatus";
import BusyButton from "./BusyButton.vue";
import StatusLine from "./StatusLine.vue";

const props = defineProps<{ server?: string; fixedServer?: boolean }>();
const emit = defineEmits<{ "update:server": [server: string] }>();
const auth = useAuthStore();
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
    if (user && !serverUrl.value) {
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
onMounted(async () => {
  await auth.initGitlab();
  if (!serverUrl.value && auth.gitlabUser) {
    serverUrl.value = auth.gitlabUser.server;
    emit("update:server", serverUrl.value);
  }
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
  try {
    const user = await gitlabRequest<{ login: string; server?: string }>(
      "login",
      {
        token: value,
        server,
      },
    );
    if (active) {
      serverUrl.value = user.server ?? server;
      auth.gitlabUser = { login: user.login, server: serverUrl.value };
      emit("update:server", serverUrl.value);
      status.set(`Connected to ${serverUrl.value} as ${user.login}.`);
    }
  } catch (e) {
    if (active) status.fail(e);
  } finally {
    if (active) busy.value = false;
  }
}

async function disconnect() {
  if (busy.value) return;
  busy.value = true;
  token.value = "";
  try {
    await gitlabRequest("logout", {
      server: serverUrl.value.trim() || "https://gitlab.com",
    });
    if (
      auth.gitlabUser?.server ===
      new URL(serverUrl.value || "https://gitlab.com").origin
    )
      auth.gitlabUser = null;
    if (active)
      status.set(
        "Disconnected GitLab. Personal tokens remain valid; saved local files remain in this browser.",
      );
  } catch (e) {
    if (active) status.fail(e);
  } finally {
    if (active) busy.value = false;
  }
}
</script>

<template>
  <div class="gitlab-login mt-sm">
    <p v-if="connected" class="hint" role="status">
      Connected to {{ connected.server }} as {{ connected.login }}.
    </p>
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
      <p class="hint">
        Connect with an expiring personal access token with the
        <code>api</code> scope.
      </p>
      <label>
        GitLab server URL
        <input
          v-model="serverUrl"
          type="url"
          placeholder="https://gitlab.example.org"
          :readonly="fixedServer"
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
    <button
      type="button"
      class="btn ghost mt-sm"
      :disabled="busy"
      @click="disconnect"
    >
      Disconnect GitLab
    </button>
    <p class="hint">
      Imported files and saved edits may stay in this browser after
      disconnecting.
    </p>
    <StatusLine :text="status.text" :error="status.error" />
  </div>
</template>
