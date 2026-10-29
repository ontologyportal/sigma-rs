<script setup lang="ts">
/**
 * Shown once per load when a boot-time check finds something new: a
 * constituent source whose upstream moved since the last load
 * (`kb.changedOnLoad`), or a contradiction source that reported new
 * contradictions (`contradictions.changedOnLoad`). Each row is already
 * applied under "auto-update" or awaits review/import under "auto-check",
 * and opens the same review the Sources card does. Closing it loses
 * nothing: the alerts stay on their cards until dismissed.
 */
import { computed, ref } from "vue";
import BaseDialog from "./BaseDialog.vue";
import UpdatePreviewDialog from "./kb/UpdatePreviewDialog.vue";
import { navigate } from "../router";
import { useKBStore, type UpdateAlert } from "../stores/kb";
import {
  useContradictionsStore,
  type ContradictionAlert,
} from "../stores/contradictions";

const kb = useKBStore();
const contradictions = useContradictionsStore();

const open = computed({
  get: () =>
    kb.changedOnLoad.length > 0 || contradictions.changedOnLoad.length > 0,
  set: (v: boolean) => {
    if (!v) {
      kb.changedOnLoad = [];
      contradictions.changedOnLoad = [];
    }
  },
});

const reviewOpen = ref(false);
const reviewing = ref<UpdateAlert | null>(null);

function review(a: UpdateAlert) {
  reviewing.value = a;
  reviewOpen.value = true;
}

function onReviewDone() {
  const a = reviewing.value;
  if (a) kb.finishReview(a.review ?? [], a.key);
  reviewing.value = null;
}

function importPending(a: ContradictionAlert) {
  void contradictions.applyPending(a);
}

function showFinding(a: ContradictionAlert) {
  contradictions.focusKey = a.keys?.[0] ?? null;
  open.value = false;
  navigate("audit");
}

function openSources() {
  open.value = false;
  navigate("kb", {
    view: kb.changedOnLoad.length ? "sources" : "contradictions",
  });
}
</script>

<template>
  <BaseDialog
    v-model="open"
    title="Sources changed since your last visit"
    width="min(560px, 92vw)"
  >
    <template v-if="kb.changedOnLoad.length">
      <p class="hint">Since the last load, these sources have changed:</p>
      <ul class="changed">
        <li v-for="a in kb.changedOnLoad" :key="a.key">
          <span class="msg">{{ a.message }}</span>
          <button
            v-if="a.review?.length"
            class="btn ghost changed-review"
            type="button"
            @click="review(a)"
          >
            Review
          </button>
          <span v-else class="hint">applied</span>
        </li>
      </ul>
    </template>
    <template v-if="contradictions.changedOnLoad.length">
      <p class="hint">New contradictions have been reported:</p>
      <ul class="changed">
        <li v-for="a in contradictions.changedOnLoad" :key="a.key">
          <span class="msg">{{ a.message }}</span>
          <button
            v-if="a.pending"
            class="btn ghost changed-review"
            type="button"
            @click="importPending(a)"
          >
            Import
          </button>
          <button
            v-else-if="a.keys?.length"
            class="btn ghost changed-review"
            type="button"
            @click="showFinding(a)"
          >
            Show
          </button>
          <span v-else class="hint">recorded</span>
        </li>
      </ul>
    </template>
    <template #actions>
      <a class="hint jump" @click="openSources">Manage sources</a>
      <button class="btn ghost" type="button" @click="open = false">
        Close
      </button>
    </template>
  </BaseDialog>
  <UpdatePreviewDialog
    v-model="reviewOpen"
    :files="reviewing?.review ?? []"
    @done="onReviewDone"
  />
</template>

<style scoped>
p {
  margin: 0 0 10px;
}
.changed {
  list-style: none;
  margin: 0 0 12px;
  padding: 0;
  max-height: 50vh;
  overflow-y: auto;
}
.changed li {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 6px 0;
  border-bottom: 1px solid var(--line);
}
.changed li:last-child {
  border-bottom: none;
}
.msg {
  flex: 1 1 auto;
  min-width: 0;
  overflow-wrap: anywhere;
}
.jump {
  cursor: pointer;
  color: var(--accent);
}
</style>
