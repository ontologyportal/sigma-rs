<script setup lang="ts">
import { computed, ref } from "vue";
import BaseDialog from "./BaseDialog.vue";
import { clearCache } from "../services/kb-cache";
import { useKBStore } from "../stores/kb";
import { useShellStore } from "../stores/shell";

const kb = useKBStore();
const shell = useShellStore();

const languageOptions = computed(() =>
  kb.languages.length
    ? kb.languages
    : [{ symbol: "EnglishLanguage", label: "English" }],
);

const versionText = computed(() => {
  const v = shell.version;
  return v ? `v${v.version} (build ${v.build}, ${v.commit})` : "dev build";
});

const cacheCleared = ref(false);

async function onClearCache() {
  await clearCache();
  cacheCleared.value = true;
  setTimeout(() => {
    cacheCleared.value = false;
  }, 2000);
}
</script>

<template>
  <BaseDialog v-model="shell.settingsOpen" title="Settings">
    <div class="settings-row">
      <span>Theme</span>
      <button
        class="btn ghost"
        type="button"
        title="Toggle light/dark theme"
        aria-label="Toggle light/dark theme"
        @click="shell.toggleTheme()"
      >
        ◐ Toggle
      </button>
    </div>
    <div class="settings-row">
      <label for="genericVarsToggle">Generic paraphrase variables</label>
      <input
        id="genericVarsToggle"
        v-model="kb.genericVars"
        type="checkbox"
        title='Show paraphrases as "an entity" / "the entity" instead of ?VarName'
      />
    </div>
    <div class="settings-row">
      <label for="langSelect">Language</label>
      <select id="langSelect" v-model="kb.uiLanguage">
        <option v-for="l in languageOptions" :key="l.symbol" :value="l.symbol">
          {{ l.label }}
        </option>
      </select>
    </div>
    <button
      class="btn ghost clear-cache"
      type="button"
      title="Clear the cached KB build in this browser — the next load re-fetches and re-builds from scratch"
      @click="onClearCache"
    >
      {{ cacheCleared ? "cache cleared" : "Clear cache" }}
    </button>
    <template #actions>
      <span class="version">{{ versionText }}</span>
      <button
        class="btn ghost"
        type="button"
        @click="shell.settingsOpen = false"
      >
        Close
      </button>
    </template>
  </BaseDialog>
</template>

<style scoped>
.settings-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 18px;
}
.settings-row label {
  margin: 0;
  flex: 0 0 auto;
}
/* min-width: 0 lets the select shrink inside the flex row instead of its
   longest option (e.g. a long language name) pushing past the dialog's
   padding -- a fixed-width dialog can't grow to accommodate it. */
.settings-row select {
  flex: 1 1 auto;
  min-width: 0;
  max-width: 100%;
}
.clear-cache {
  width: 100%;
  margin-bottom: 16px;
}
.version {
  font-size: 11px;
  color: var(--muted);
  font-family: var(--mono);
  opacity: 0.7;
}
</style>
