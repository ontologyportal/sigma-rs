<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, watch } from "vue";
import { useRoute, type LocationQuery } from "vue-router";
import LoadingScreen from "./components/LoadingScreen.vue";
import SettingsDialog from "./components/SettingsDialog.vue";
import LoginDialog from "./components/LoginDialog.vue";
import LogoutDialog from "./components/LogoutDialog.vue";
import VersionDialog from "./components/VersionDialog.vue";
import { PROMOTE_TABS } from "./constants";
import { navigate, router, type TabName } from "./router";
import { installFlushOnHide } from "./services/kb-cache";
import { useAuthStore } from "./stores/auth";
import { useBootStore } from "./stores/boot";
import { useKBStore } from "./stores/kb";
import { useShellStore } from "./stores/shell";

const kb = useKBStore();
const boot = useBootStore();
const shell = useShellStore();
const auth = useAuthStore();
const route = useRoute();

/** The nav strip, grouped by activity: `[routeName, label]` pairs. */
const TAB_GROUPS: { label: string; tabs: [TabName, string][] }[] = [
  { label: "Explore", tabs: [["browse", "Browse"]] },
  {
    label: "Reason",
    tabs: [
      ["prover", "Ask/Tell"],
      ["audit", "Audit"],
    ],
  },
  {
    label: "Develop",
    tabs: [
      ["edit", "Edit"],
      ["diagnostics", "Diagnostics"],
    ],
  },
  {
    label: "Manage",
    tabs: [
      ["kb", "Knowledge base"],
      ["history", "History"],
    ],
  },
];

const errorCount = computed(
  () => kb.diagnostics.filter((d) => d.severity === "error").length,
);

function isSelected(name: TabName): boolean {
  if (name === "browse")
    return route.name === "home" || route.name === "browse";
  return route.name === name;
}

function gated(name: TabName): boolean {
  return kb.promoting && PROMOTE_TABS.includes(name);
}

function go(name: TabName) {
  if (gated(name)) return;
  navigate(name);
}

// A promote-gated tab that is showing when promotion starts is evicted to
// Browse and restored once the KB is usable again. `router.replace`, not
// `push`: an automatic eviction is not a navigation the user should have to
// hit Back through.
let evicted: { name: TabName; query: LocationQuery } | null = null;

watch(
  () => kb.promoting,
  (promoting) => {
    const current =
      route.name === "home" || !route.name ? "browse" : String(route.name);
    if (promoting) {
      if (PROMOTE_TABS.includes(current)) {
        evicted = { name: current as TabName, query: { ...route.query } };
        router.replace({ name: "browse" });
      }
    } else if (evicted && current === "browse") {
      router.replace({ name: evicted.name, query: evicted.query });
      evicted = null;
    }
  },
);

function onKeydown(e: KeyboardEvent) {
  const t = e.target;
  const typing =
    t instanceof HTMLInputElement ||
    t instanceof HTMLTextAreaElement ||
    t instanceof HTMLSelectElement ||
    (t instanceof HTMLElement && t.isContentEditable);
  if (e.key === "/" && !e.ctrlKey && !e.metaKey && !e.altKey && !typing) {
    e.preventDefault();
    navigate("browse");
    shell.requestSearchFocus();
  }
}

let removeFlush: (() => void) | null = null;

onMounted(() => {
  shell.init();
  shell.loadVersion();
  auth.init();
  removeFlush = installFlushOnHide();
  document.addEventListener("keydown", onKeydown);
});

onBeforeUnmount(() => {
  removeFlush?.();
  document.removeEventListener("keydown", onKeydown);
});
</script>

<template>
  <LoadingScreen v-if="!boot.finished" />
  <template v-else>
    <main>
      <header>
        <router-link
          :to="{ name: 'browse' }"
          class="brand"
          aria-label="Go to home page"
        >
          <img src="/logo.png" alt="SigmaKEE logo" />
          <div class="brand-text">
            <h1>SigmaKEE</h1>
            <p class="sub">
              Explore the Suggested Upper Merged Ontology (SUMO)
            </p>
          </div>
        </router-link>
        <div class="head-meta">
          <div class="head-controls">
            <a
              v-if="!auth.signedIn"
              class="head-btn"
              href="/api/github-auth"
              title="Log in with GitHub"
              aria-label="Log in with GitHub"
            >
              <svg
                width="16"
                height="16"
                viewBox="0 0 16 16"
                fill="currentColor"
                aria-hidden="true"
              >
                <path
                  d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27s1.36.09 2 .27c1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.012 8.012 0 0 0 16 8c0-4.42-3.58-8-8-8Z"
                />
              </svg>
              <span>Log in</span>
            </a>
            <button
              v-else
              class="head-btn user-menu"
              type="button"
              title="Signed in with GitHub — click to log out"
              aria-haspopup="dialog"
              @click="auth.logoutDialogOpen = true"
            >
              <img
                :src="auth.user.avatarUrl"
                :alt="auth.user.name"
                width="20"
                height="20"
                class="user-avatar"
              />
              <span class="user-name">{{ auth.user.name }}</span>
            </button>
            <button
              class="head-btn"
              type="button"
              title="Report a bug"
              aria-label="Report a bug"
              @click="shell.openBugReport()"
            >
              <svg
                width="16"
                height="16"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
                stroke-linejoin="round"
                aria-hidden="true"
              >
                <rect x="7" y="7" width="10" height="12" rx="5" />
                <path d="M9 7V6a3 3 0 0 1 6 0v1" />
                <path d="M12 10v9" />
                <path d="M7 11H3" />
                <path d="M7 15H4" />
                <path d="M17 11h4" />
                <path d="M17 15h3" />
                <path d="m8.5 4 1.5 2" />
                <path d="m15.5 4-1.5 2" />
              </svg>
            </button>
            <button
              class="head-btn settings-btn"
              type="button"
              title="Settings"
              aria-label="Settings"
              aria-haspopup="dialog"
              @click="shell.settingsOpen = true"
            >
              ⚙
            </button>
          </div>
        </div>
      </header>

      <nav class="tabs" role="tablist">
        <div
          v-for="group in TAB_GROUPS"
          :key="group.label"
          class="tab-group"
          :data-label="group.label"
        >
          <button
            v-for="[name, label] in group.tabs"
            :key="name"
            role="tab"
            type="button"
            :aria-selected="isSelected(name)"
            :class="{ disabled: gated(name) }"
            :aria-disabled="gated(name)"
            @click="go(name)"
          >
            {{ label }}
            <span
              v-if="name === 'diagnostics' && kb.diagnostics.length"
              class="tab-badge"
              :class="{ err: errorCount > 0 }"
              >{{ kb.diagnostics.length }}</span
            >
          </button>
        </div>
      </nav>

      <router-view v-slot="{ Component }">
        <keep-alive>
          <component :is="Component" />
        </keep-alive>
      </router-view>
    </main>

    <footer>
      <small>
        <a href="https://ontologyportal.github.io/sigma-rs"
          >Latest Inference Tests</a
        >
        -- <a href="https://ontologyportal.org">ontologyportal.org</a> --
        <a href="https://github.com/ontologyportal/sigma-rs/blob/main/LICENSE"
          >GPL-3.0</a
        >
      </small>
    </footer>

    <div class="toast" v-show="kb.promoting" role="status">
      <span class="spin"></span>
      <span
        >Post-processing — some features may not be available until
        complete.</span
      >
    </div>

    <SettingsDialog />
    <LoginDialog />
    <LogoutDialog />
    <VersionDialog />
  </template>
</template>

<style scoped>
/* Header: the brand (logo + title + tagline) is one inseparable unit; the
   meta block (theme/bug/settings controls) sits to its right on wide screens
   and becomes a full-width row underneath on narrow ones. */
header {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 12px 16px;
  flex-wrap: wrap;
}
.brand {
  display: flex;
  align-items: center;
  gap: 12px;
  min-width: 0;
  color: inherit;
}
.brand:hover {
  text-decoration: none;
}
.brand img {
  width: 50px;
  flex: 0 0 auto;
}
.brand-text {
  min-width: 0;
}
.head-meta {
  margin-left: auto;
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  gap: 2px;
  text-align: right;
}
@media (max-width: 620px) {
  .brand img {
    width: 38px;
  }
  h1 {
    font-size: 18px;
  }
  .sub {
    font-size: 12px;
  }
  .head-meta {
    margin-left: 0;
    flex: 1 1 100%;
    flex-direction: row;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: 4px 12px;
    text-align: left;
  }
}
.head-controls {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  margin-top: 6px;
}
@media (max-width: 620px) {
  .head-controls {
    margin-top: 0;
  }
}

nav.tabs {
  display: flex;
  gap: 0;
  margin: 18px 0 16px;
  border-bottom: 1px solid var(--line);
  overflow-x: auto;
  overflow-y: hidden;
  -webkit-overflow-scrolling: touch;
  scrollbar-width: thin;
  /* On a narrow viewport this strip scrolls horizontally with no native
     scrollbar on most mobile browsers -- fade both edges so a clipped tab
     reads as "more to scroll to," not as the strip simply ending there. */
  mask-image: linear-gradient(
    to right,
    transparent,
    black 16px,
    black calc(100% - 16px),
    transparent
  );
  -webkit-mask-image: linear-gradient(
    to right,
    transparent,
    black 16px,
    black calc(100% - 16px),
    transparent
  );
}
/* Tabs grouped by activity: a quiet label above each cluster. */
.tab-group {
  display: flex;
  gap: 2px;
  position: relative;
  padding-top: 14px;
  flex: 0 0 auto;
}
.tab-group::before {
  content: attr(data-label);
  position: absolute;
  top: 0;
  left: 14px;
  font-size: 9px;
  text-transform: uppercase;
  letter-spacing: 0.09em;
  color: var(--muted);
  opacity: 0.8;
}
.tab-group + .tab-group {
  margin-left: 10px;
  padding-left: 10px;
  border-left: 1px solid var(--line);
}
nav.tabs button {
  font: inherit;
  background: none;
  border: none;
  color: var(--muted);
  cursor: pointer;
  padding: 9px 14px;
  border-bottom: 2px solid transparent;
  margin-bottom: -1px;
  flex: 0 0 auto;
  white-space: nowrap;
}
nav.tabs button[aria-selected="true"] {
  color: var(--fg);
  border-bottom-color: var(--accent);
  font-weight: 600;
}
nav.tabs button.disabled {
  opacity: 0.4;
  cursor: not-allowed;
}
/* Diagnostics count on its tab button; colored by worst severity. */
.tab-badge {
  display: inline-block;
  min-width: 17px;
  margin-left: 5px;
  padding: 0 5px;
  border-radius: 999px;
  font-size: 11px;
  font-weight: 700;
  line-height: 17px;
  text-align: center;
  background: color-mix(in srgb, var(--warn) 22%, transparent);
  color: var(--warn);
}
.tab-badge.err {
  background: color-mix(in srgb, var(--muted) 22%, transparent);
  color: var(--muted);
}
/* Header controls: GitHub login/user-menu, bug report, settings -- one
   shared size and visual style so signing in doesn't change the row's
   rhythm. Widths differ (icon-only vs. icon+label vs. avatar+name), which
   is expected; height and style stay identical. */
.head-btn {
  font: inherit;
  font-size: 15px;
  line-height: 1;
  cursor: pointer;
  text-decoration: none;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  height: 34px;
  box-sizing: border-box;
  padding: 0 10px;
  background: var(--bg);
  color: var(--muted);
  border: 1px solid var(--line);
  border-radius: 8px;
}
.head-btn:hover {
  color: var(--fg);
  border-color: var(--accent);
}
.head-btn svg {
  display: block;
}
.user-avatar {
  border-radius: 50%;
  display: block;
}
.user-name {
  max-width: 140px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
/* The gear glyph renders small relative to its em box compared to the SVG
   icons beside it (bug report) or the avatar (signed in) -- bump it alone. */
.settings-btn {
  font-size: 20px;
}

.toast {
  position: fixed;
  right: 16px;
  bottom: 16px;
  display: flex;
  align-items: center;
  gap: 10px;
  background: var(--card);
  border: 1px solid var(--line);
  border-radius: 10px;
  padding: 10px 14px;
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.2);
  font-size: 13px;
  color: var(--muted);
  z-index: 50;
}
.toast .spin {
  width: 15px;
  height: 15px;
  border: 2px solid var(--line);
  border-top-color: var(--accent);
  border-radius: 50%;
  animation: spin 0.8s linear infinite;
}
@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}
</style>
