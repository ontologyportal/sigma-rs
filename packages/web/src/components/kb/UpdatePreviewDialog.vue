<script setup lang="ts">
/**
 * The update-preference "auto-check"/"Update now" review step: reuses
 * DiffDialog's Monaco-diff-plus-decide shape (incoming on the left, what's
 * currently loaded on the right), generalized to a plain `current`/
 * `incoming` pair instead of DiffDialog's `ChangeRow`-and-edit-conflict
 * framing, so it works for any source kind and steps through several
 * changed files in one repo/URL source one at a time.
 */
import { computed, ref, watch } from "vue";
import BaseDialog from "../BaseDialog.vue";
import MonacoDiff from "../MonacoDiff.vue";
import { useKBStore, type PendingUpdateFile } from "../../stores/kb";

const props = defineProps<{
  /** Open state (v-model). */
  modelValue: boolean;
  /** The files awaiting review, in order. */
  files: PendingUpdateFile[];
}>();

const emit = defineEmits<{
  "update:modelValue": [value: boolean];
  /** Every file has been applied or skipped, or the dialog was closed
   *  early -- the caller acknowledges the whole reviewed batch either way. */
  done: [];
}>();

const kb = useKBStore();
const index = ref(0);
const busy = ref(false);

watch(
  () => props.modelValue,
  (open) => {
    if (open) index.value = 0;
  },
);

const current = computed<PendingUpdateFile | null>(
  () => props.files[index.value] ?? null,
);
const title = computed(() =>
  props.files.length > 1
    ? `Review updates (${index.value + 1}/${props.files.length})`
    : "Review update",
);

function finish() {
  emit("update:modelValue", false);
  emit("done");
}

function advance() {
  if (index.value + 1 < props.files.length) index.value += 1;
  else finish();
}

async function apply() {
  const f = current.value;
  if (!f) return;
  busy.value = true;
  try {
    await kb.applyReviewFile(f);
    advance();
  } finally {
    busy.value = false;
  }
}

function skip() {
  advance();
}
</script>

<template>
  <BaseDialog
    :model-value="modelValue"
    width="min(1000px, 94vw)"
    @update:model-value="(v) => !v && finish()"
  >
    <h3>{{ title }}</h3>
    <p v-if="current" class="hint">
      {{ current.name }} — the update on the left, what's currently loaded on
      the right.
    </p>
    <div class="diff-pane">
      <MonacoDiff
        v-if="current"
        :original="current.incoming"
        :modified="current.current"
      />
    </div>
    <template #actions>
      <span class="spacer"></span>
      <button class="btn ghost" type="button" :disabled="busy" @click="skip">
        Skip
      </button>
      <button class="btn" type="button" :disabled="busy" @click="apply">
        Apply
      </button>
    </template>
  </BaseDialog>
</template>

<style scoped>
h3 {
  margin: 0 0 6px;
  font-size: 14px;
}
p {
  margin: 0 0 12px;
}
.diff-pane {
  height: min(52vh, 460px);
  border: 1px solid var(--line);
  border-radius: 7px;
  overflow: hidden;
}
</style>
