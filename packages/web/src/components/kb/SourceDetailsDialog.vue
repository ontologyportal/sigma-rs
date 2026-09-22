<script setup lang="ts">
/** One Sources-card row's details: repo coordinates + current commit for a
 *  GitHub source, the URL for a remote file, the added date for a local
 *  upload. The commit is fetched lazily, only while the dialog is open. */
import { ref, watch } from "vue";
import { fetchRepoLastCommit } from "../../api/github.ts";
import { GitOrigin, RemoteOrigin, LocalOrigin } from "../../models/Origin";
import type { SourceGroup } from "../../stores/kb";
import BaseDialog from "../BaseDialog.vue";

const props = defineProps<{
  modelValue: boolean;
  group: SourceGroup | null;
}>();
const emit = defineEmits<{ "update:modelValue": [value: boolean] }>();

const commit = ref<{ sha: string | null; date: Date | null } | null>(null);
const commitError = ref<string | null>(null);
const commitLoading = ref(false);

watch(
  () => [props.modelValue, props.group] as const,
  ([open, group]) => {
    commit.value = null;
    commitError.value = null;
    if (!open || !group || group.origin.kind !== "sumo") return;
    const g = group.origin as GitOrigin;
    commitLoading.value = true;
    fetchRepoLastCommit(g.owner, g.repo, g.branch)
      .then((c) => (commit.value = c))
      .catch(
        (e) => (commitError.value = e instanceof Error ? e.message : String(e)),
      )
      .finally(() => (commitLoading.value = false));
  },
  { immediate: true },
);

function close() {
  emit("update:modelValue", false);
}

function fmtDate(d: Date): string {
  return d.toLocaleString();
}
</script>

<template>
  <BaseDialog
    :model-value="modelValue"
    title="Source details"
    width="min(440px, 92vw)"
    @update:model-value="emit('update:modelValue', $event)"
  >
    <div v-if="group" class="src-details">
      <template v-if="group.origin.kind === 'sumo'">
        <div class="src-field">
          <span class="src-field-label">Owner</span>
          <span class="src-field-value">{{
            (group.origin as GitOrigin).owner
          }}</span>
        </div>
        <div class="src-field">
          <span class="src-field-label">Repository</span>
          <span class="src-field-value">{{
            (group.origin as GitOrigin).repo
          }}</span>
        </div>
        <div class="src-field">
          <span class="src-field-label">Branch</span>
          <span class="src-field-value">{{
            (group.origin as GitOrigin).branch
          }}</span>
        </div>
        <div class="src-field">
          <span class="src-field-label">Commit</span>
          <span class="src-field-value">
            <span v-if="commitLoading" class="hint">Loading…</span>
            <span v-else-if="commitError" class="hint">{{ commitError }}</span>
            <template v-else-if="commit?.sha">
              <code>{{ commit.sha.slice(0, 10) }}</code>
              <span v-if="commit.date" class="hint">
                — {{ fmtDate(commit.date) }}</span
              >
            </template>
            <span v-else class="hint">Unknown</span>
          </span>
        </div>
      </template>

      <template v-else-if="group.origin.kind === 'url'">
        <div class="src-field">
          <span class="src-field-label">URL</span>
          <span class="src-field-value src-url">{{
            (group.origin as RemoteOrigin).url
          }}</span>
        </div>
      </template>

      <template v-else>
        <div class="src-field">
          <span class="src-field-label">Added</span>
          <span class="src-field-value">{{
            fmtDate((group.origin as LocalOrigin).added)
          }}</span>
        </div>
      </template>
    </div>

    <template #actions>
      <span class="spacer"></span>
      <button class="btn ghost" type="button" @click="close">Close</button>
    </template>
  </BaseDialog>
</template>

<style scoped>
.src-details {
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.src-field {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.src-field-label {
  font-size: 11px;
  color: var(--muted);
  text-transform: uppercase;
  letter-spacing: 0.04em;
}
.src-field-value {
  font-size: 13px;
}
.src-field-value code {
  font-family: var(--mono);
  font-size: 12px;
}
.src-url {
  word-break: break-all;
}
</style>
