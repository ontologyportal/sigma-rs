<script setup lang="ts">
import LoadingScreen from './components/LoadingScreen.vue';
import { useBootStore } from './stores/boot.ts';

const boot = useBootStore();
</script>

<template>
    <loading-screen v-if="!boot.finished"/>
    <main v-else>
      <header>
        <a class="brand jump" data-tab="browse" aria-label="Go to home page">
          <img src="/logo.png" alt="SigmaKEE logo" />
          <div class="brand-text">
            <h1>SigmaKEE</h1>
            <p class="sub">
              Explore the Suggested Upper Merged Ontology (SUMO)
            </p>
          </div>
        </a>
        <div class="head-meta">
          <div class="head-controls">
            <a
              id="githubLogin"
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
              id="userMenu"
              class="head-btn"
              type="button"
              hidden
              title="Signed in with GitHub — click to log out"
              aria-haspopup="dialog"
              aria-controls="logoutDialog"
            >
              <img id="userAvatar" alt="" width="20" height="20" />
              <span id="userName"></span>
            </button>
            <button
              id="bugReport"
              class="head-btn"
              type="button"
              title="Report a bug"
              aria-label="Report a bug"
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
              id="settingsBtn"
              class="head-btn"
              type="button"
              title="Settings"
              aria-label="Settings"
              aria-haspopup="dialog"
              aria-controls="settingsDialog"
            >
              ⚙
            </button>
          </div>
        </div>
      </header>

      <nav class="tabs" role="tablist">
        <div class="tab-group" data-label="Explore">
          <button role="tab" data-tab="browse" :aria-selected="$route.name == 'home' || $route.name == 'browse'">
            Browse
          </button>
        </div>
        <div class="tab-group" data-label="Reason">
          <button role="tab" data-tab="prover" :aria-selected="$route.name == 'prover'">
            Ask/Tell
          </button>
          <button role="tab" data-tab="audit" :aria-selected="$route.name == 'audit'">
            Audit
          </button>
        </div>
        <div class="tab-group" data-label="Develop">
          <button role="tab" data-tab="edit" :aria-selected="$route.name == 'edit'">Edit</button>
          <button role="tab" data-tab="diagnostics" :aria-selected="$route.name == 'diagnostics'">
            Diagnostics
          </button>
        </div>
        <div class="tab-group" data-label="Manage">
          <button role="tab" data-tab="kb" :aria-selected="$route.name == 'kb'">
            Knowledge base
          </button>
          <button role="tab" data-tab="history" :aria-selected="$route.name == 'history'">
            History
          </button>
        </div>
      </nav>
    <router-view />
    </main>
</template>