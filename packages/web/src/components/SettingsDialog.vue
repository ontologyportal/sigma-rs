<script setup lang="ts">
import { computed, ref } from "vue";
import BaseDialog from "./BaseDialog.vue";
import { clearCache } from "../services/kb-cache";
import { downloadBackup } from "../services/backup";
import { useKBStore } from "../stores/kb";
import { useShellStore } from "../stores/shell";
import { AVAILABLE_LAYOUTS } from "../constants/index.ts";
import { errMsg } from "../utils/format";

const kb = useKBStore();
const shell = useShellStore();

const languageOptions = computed(() =>
  kb.languages.length
    ? kb.languages
    : [
        {
          symbol: kb.symbols.defaultLanguage,
          label: kb.symbols.defaultLanguage,
        },
      ],
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

const backupBusy = ref(false);
const backupLabel = ref("");

async function onBackup() {
  backupBusy.value = true;
  backupLabel.value = "";
  try {
    const n = await downloadBackup();
    backupLabel.value = `${n} file${n === 1 ? "" : "s"} saved`;
  } catch (e) {
    backupLabel.value = errMsg(e);
  } finally {
    backupBusy.value = false;
    setTimeout(() => {
      backupLabel.value = "";
    }, 4000);
  }
}
</script>

<template>
  <BaseDialog v-model="shell.settingsOpen" title="Settings">
    <div class="settings-row">
      <label for="appTheme">App Theme</label>
      <button
        id="appTheme"
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
      <label for="appLayout">App Layout</label>
      <select
        id="appLayout"
        :disabled="shell.layoutNarrow"
        :title="
          shell.layoutNarrow
            ? 'Classic needs more width -- using Comfortable until the window is wider'
            : undefined
        "
        @change="shell.changeLayout(shell.layout)"
        v-model="shell.layout"
      >
        <option v-for="l in AVAILABLE_LAYOUTS" :key="l" :value="l">
          {{ String(l[0]).toUpperCase() + String(l).slice(1) }}
        </option>
      </select>
    </div>
    <div v-if="shell.layoutNarrow" class="hint layout-narrow-note">
      Layout settings unavailable on smaller screens
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
    <div class="maintenance">
      <button
        class="btn ghost clear-cache"
        type="button"
        title="Clear the cached KB build in this browser — the next load re-fetches and re-builds from scratch"
        @click="onClearCache"
      >
        {{ cacheCleared ? "cache cleared" : "Clear cache" }}
      </button>
      <button
        class="btn ghost"
        type="button"
        :disabled="backupBusy"
        title="Download everything this browser holds — local edits, uploaded files, the cached KB build, and the settings that index them — as one zip"
        @click="onBackup"
      >
        {{ backupBusy ? "Preparing…" : "Back up data" }}
      </button>
      <span v-if="backupLabel" class="hint">{{ backupLabel }}</span>
    </div>
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
/* Matches nav.tabs/.diag-pager's own disabled treatment elsewhere in the
   app -- select has no built-in :disabled look worth relying on. */
.settings-row select:disabled {
  opacity: 0.45;
  cursor: not-allowed;
}
.layout-narrow-note {
  margin: -10px 0 18px;
  font-size: 12px;
}
.maintenance {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  margin-bottom: 16px;
}
/* Share the row evenly, and stack rather than overflow on a narrow dialog. */
.maintenance .btn {
  flex: 1 1 140px;
}
.maintenance .hint {
  flex: 1 0 100%;
  font-size: 12px;
}
.version {
  font-size: 11px;
  color: var(--muted);
  font-family: var(--mono);
  opacity: 0.7;
}
</style>
