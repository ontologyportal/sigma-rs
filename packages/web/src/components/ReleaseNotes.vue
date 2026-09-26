<script setup lang="ts">
/** One release's rendered notes (`renderReleaseNotes` output) and a link to
 *  its page on GitHub. */
defineProps<{
  html: string | null;
  url: string | null;
}>();
</script>

<template>
  <div class="release-notes">
    <div v-if="html" class="notes" v-html="html" />
    <p v-else class="hint">No release notes.</p>
    <a
      v-if="url"
      class="release-link"
      :href="url"
      target="_blank"
      rel="noopener noreferrer"
    >
      View this release on GitHub
    </a>
  </div>
</template>

<style scoped>
.release-notes {
  margin: 0 0 18px;
}
.release-link {
  font-size: 12px;
}
.notes :deep(h4) {
  margin: 16px 0 6px;
  font-size: 13px;
}
.notes :deep(p),
.notes :deep(ul) {
  margin: 0 0 10px;
}
.notes :deep(:first-child) {
  margin-top: 0;
}
.notes :deep(pre) {
  font-family: var(--mono);
  font-size: 12px;
  white-space: pre-wrap;
  overflow-x: auto;
}
</style>
