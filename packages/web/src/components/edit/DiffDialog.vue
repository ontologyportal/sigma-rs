<script setup lang="ts">
/**
 * Side-by-side upstream vs. local. Doubles as the conflict dialog: the only
 * difference when upstream has moved is the wording and what "take upstream"
 * costs, so both share one code path rather than drifting apart as two.
 */
import { computed, ref, watch } from "vue";
import BaseDialog from "../BaseDialog.vue";
import MonacoDiff from "../MonacoDiff.vue";
import { useChangesStore, type ChangeRow } from "../../stores/changes";
import { useKBStore } from "../../stores/kb";
import { originForKind } from "../../models/Origin";
import { errMsg } from "../../utils/format";

const props = defineProps<{
  /** Open state (v-model). */
  modelValue: boolean;
  /** The tracked change to show; null renders an empty dialog. */
  row: ChangeRow | null;
}>();

const emit = defineEmits<{
  "update:modelValue": [value: boolean];
  /** Upstream's text replaced the local copy (already saved to the KB). */
  taken: [text: string];
  /** The user chose to keep the local copy of a STALE row over the moved
   *  upstream; the owner acknowledges the new upstream base. Not emitted
   *  for a plain diff review of a non-stale row. */
  keep: [row: ChangeRow];
}>();

const changes = useChangesStore();
const kb = useKBStore();

const upstream = ref<string | null>(null);
const status = ref("");
const busy = ref(false);

const title = computed(() =>
  props.row?.stale ? "Upstream has changed" : "Local changes",
);
const message = computed(() => {
  const row = props.row;
  if (!row) return "";
  return row.stale
    ? `${row.name} changed upstream after you started editing it. Keeping yours proceeds with your copy, so your next pull request replaces the newer upstream text; taking upstream's discards every local change to this file.`
    : `${row.name} — upstream on the left, your saved copy on the right.`;
});
const takeLabel = computed(() =>
  props.row?.stale
    ? "Take upstream (discards your changes)"
    : "Revert to upstream",
);
const localText = computed(() =>
  props.row ? (kb.find(props.row.name, props.row.origin)?.text ?? "") : "",
);

function close() {
  emit("update:modelValue", false);
}

function keep() {
  const row = props.row;
  if (row?.stale) emit("keep", row);
  close();
}

async function load() {
  const row = props.row;
  upstream.value = null;
  if (!props.modelValue || !row) return;
  status.value = "Loading upstream copy…";
  try {
    const text = await changes.fetchUpstreamText(row.path);
    if (props.row !== row || !props.modelValue) return; // dialog moved on while we were fetching
    upstream.value = text;
    status.value = "";
  } catch (e) {
    if (props.row !== row) return;
    status.value = "Could not load the upstream copy: " + errMsg(e);
  }
}

watch(() => [props.modelValue, props.row] as const, load, { immediate: true });

async function take() {
  const row = props.row;
  if (!row) return;
  busy.value = true;
  status.value = "Replacing your copy…";
  try {
    const text = await changes.fetchUpstreamText(row.path);
    // Saving upstream's own content is what clears the tracking: recordSave
    // drops any record whose content matches upstream, so "revert" and "take
    // the newer upstream copy" are the same operation as an ordinary save.
    await kb.updateConstituentText(row.name, text, originForKind(row.origin));
    emit("taken", text);
    close();
  } catch (e) {
    status.value = errMsg(e);
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <BaseDialog
    :model-value="modelValue"
    width="min(1000px, 94vw)"
    @update:model-value="emit('update:modelValue', $event)"
  >
    <h3>{{ title }}</h3>
    <p class="hint">{{ message }}</p>
    <div class="diff-pane">
      <MonacoDiff
        v-if="upstream !== null"
        :original="upstream"
        :modified="localText"
      />
    </div>
    <template #actions>
      <span class="hint">{{ status }}</span>
      <span class="spacer"></span>
      <button class="btn ghost" type="button" :disabled="busy" @click="take">
        {{ takeLabel }}
      </button>
      <button class="btn" type="button" @click="keep">Keep mine</button>
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
