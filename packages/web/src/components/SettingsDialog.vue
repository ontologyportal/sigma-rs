<script setup lang="ts">
import { watch } from 'vue';


const model = defineModel<boolean>();
watch(model, async (v, o) => {
    console.log("changed");
})
</script>

<template>
<dialog v-if="model" id="settingsDialog">
  <h3>Settings</h3>
  <div class="settings-row">
    <span>Theme</span>
    <button
      id="themeToggle"
      class="btn ghost"
      type="button"
      title="Toggle light/dark theme"
      aria-label="Toggle light/dark theme"
    >
      ◐ Toggle
    </button>
  </div>
  <div class="settings-row">
    <label for="genericVarsToggle">Generic paraphrase variables</label>
    <input type="checkbox" id="genericVarsToggle" title="Show paraphrases as &quot;an entity&quot; / &quot;the entity&quot; instead of ?VarName">
  </div>
  <div class="settings-row">
    <label for="langSelect">Language</label>
    <select id="langSelect">
      <option value="EnglishLanguage">English</option>
    </select>
  </div>
  <button
    class="btn ghost"
    id="clearCacheLink"
    type="button"
    title="Clear the cached KB build in this browser — the next load re-fetches and re-builds from scratch"
  >
    Clear cache
  </button>
  <div class="dialog-actions">
    <span id="settingsVersion">dev build</span>
    <button class="btn ghost" @click="model = false" type="button">
      Close
    </button>
  </div>
</dialog>
</template>