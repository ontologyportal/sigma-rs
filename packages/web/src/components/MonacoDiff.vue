<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, shallowRef, watch } from "vue";
import type * as Monaco from "monaco-editor/esm/vs/editor/editor.api.js";
import { loadMonaco, themeName, type MonacoNs } from "../services/monaco";
import { useShellStore } from "../stores/shell";
import { errMsg } from "../utils/format";

const props = withDefaults(
  defineProps<{
    /** Left-hand (upstream) text. */
    original: string;
    /** Right-hand (local) text. */
    modified: string;
    /** Language id for both models. */
    language?: "kif" | "tptp";
  }>(),
  {
    language: "kif",
  },
);

const shell = useShellStore();

const mount = ref<HTMLElement | null>(null);
const diff = shallowRef<Monaco.editor.IStandaloneDiffEditor | null>(null);
const ready = ref(false);
const error = ref("");

let monaco: MonacoNs | null = null;
let unmounted = false;

function disposeModels(): void {
  const model = diff.value?.getModel();
  if (!model) return;
  diff.value?.setModel(null);
  model.original.dispose();
  model.modified.dispose();
}

function applyModels(): void {
  const ed = diff.value;
  if (!monaco || !ed) return;
  disposeModels();
  ed.setModel({
    original: monaco.editor.createModel(props.original, props.language),
    modified: monaco.editor.createModel(props.modified, props.language),
  });
}

onMounted(async () => {
  let m: MonacoNs;
  try {
    m = await loadMonaco();
  } catch (e) {
    if (unmounted) return;
    error.value = "Failed to load the editor: " + errMsg(e);
    return;
  }
  if (unmounted || !mount.value) return;
  monaco = m;
  diff.value = m.editor.createDiffEditor(mount.value, {
    readOnly: true,
    automaticLayout: true,
    renderSideBySide: true,
    minimap: { enabled: false },
    fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
    fontSize: 12,
    theme: themeName(shell.isDark),
  });
  applyModels();
  ready.value = true;
});

onBeforeUnmount(() => {
  unmounted = true;
  disposeModels();
  diff.value?.dispose();
  diff.value = null;
});

watch(() => [props.original, props.modified, props.language], applyModels);

watch(
  () => shell.isDark,
  (dark) => {
    monaco?.editor.setTheme(themeName(dark));
  },
);

defineExpose({ diff });
</script>

<template>
  <div class="monaco-host">
    <div v-if="error" class="placeholder error">{{ error }}</div>
    <div v-else-if="!ready" class="placeholder">Loading diff...</div>
    <div ref="mount" class="mount"></div>
  </div>
</template>

<style scoped>
.monaco-host {
  position: relative;
  height: 100%;
  min-height: inherit;
}

.mount {
  position: absolute;
  inset: 0;
}

.placeholder {
  position: absolute;
  inset: 0;
  color: var(--muted);
  font-size: 13px;
  display: flex;
  align-items: center;
  justify-content: center;
  height: 100%;
}

.placeholder.error {
  color: var(--bad);
}
</style>
