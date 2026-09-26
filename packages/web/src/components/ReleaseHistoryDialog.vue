<script setup lang="ts">
/** Every release's notes, newest first, opened by clicking the version
 *  number in the settings dialog. The running version's entry starts open. */
import BaseDialog from "./BaseDialog.vue";
import BusyButton from "./BusyButton.vue";
import Disclosure from "./Disclosure.vue";
import ReleaseNotes from "./ReleaseNotes.vue";
import { APP_REPO } from "../constants";
import { useShellStore, type ReleaseEntry } from "../stores/shell";

const shell = useShellStore();

const allReleasesUrl = `https://github.com/${APP_REPO.owner}/${APP_REPO.repo}/releases`;

function isCurrent(e: ReleaseEntry): boolean {
  return e.version === shell.version?.version;
}

function summary(e: ReleaseEntry): string {
  const parts = [`v${e.version}`];
  if (e.date) parts.push(e.date);
  return parts.join(" - ") + (isCurrent(e) ? " (current)" : "");
}
</script>

<template>
  <BaseDialog
    v-model="shell.releaseHistory.open"
    title="Release notes"
    width="min(640px, 92vw)"
  >
    <div class="history">
      <p
        v-if="!shell.releaseHistory.entries && shell.releaseHistory.loading"
        class="hint"
      >
        Loading releases...
      </p>
      <p v-else-if="shell.releaseHistory.entries?.length === 0" class="hint">
        No releases published yet.
      </p>
      <Disclosure
        v-for="(e, i) in shell.releaseHistory.entries ?? []"
        :key="e.version"
        :summary="summary(e)"
        :open="isCurrent(e) || (i === 0 && !shell.version)"
        class="entry"
      >
        <ReleaseNotes :html="e.notesHtml" :url="e.url" />
      </Disclosure>
      <p v-if="shell.releaseHistory.error" class="hint error">
        Could not load releases: {{ shell.releaseHistory.error }}
      </p>
      <BusyButton
        v-if="
          shell.hasMoreReleases &&
          (shell.releaseHistory.entries || shell.releaseHistory.error)
        "
        class="load-more"
        ghost
        :busy="shell.releaseHistory.loading"
        :label="shell.releaseHistory.error ? 'Retry' : 'Load more'"
        busy-label="Loading..."
        @click="shell.loadMoreReleases()"
      />
    </div>
    <template #actions>
      <a :href="allReleasesUrl" target="_blank" rel="noopener noreferrer">
        All releases on GitHub
      </a>
      <span class="spacer"></span>
      <button
        class="btn ghost"
        type="button"
        @click="shell.releaseHistory.open = false"
      >
        Close
      </button>
    </template>
  </BaseDialog>
</template>

<style scoped>
.history {
  max-height: 60vh;
  overflow-y: auto;
  margin: 0 0 18px;
}
.history > p {
  margin: 10px 0 0;
}
.entry :deep(summary) {
  font-family: var(--mono);
}
.entry .release-notes {
  margin: 8px 0 0 14px;
}
.load-more {
  display: block;
  margin: 12px auto 0;
}
.error {
  color: var(--bad);
}
</style>
