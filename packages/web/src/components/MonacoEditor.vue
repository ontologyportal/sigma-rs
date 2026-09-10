<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, shallowRef, watch } from "vue";
import type * as Monaco from "monaco-editor/esm/vs/editor/editor.api.js";
import {
  diagsToMarkers,
  loadMonaco,
  lspEditor,
  setLspEditor,
  themeName,
  type MonacoNs,
} from "../services/monaco";
import { useShellStore } from "../stores/shell";

const props = withDefaults(
  defineProps<{
    /** The editor's text (v-model). */
    modelValue: string;
    /** Language id of the model. */
    language?: "kif" | "tptp";
    /** Read-only editor. */
    readOnly?: boolean;
    /** Ask/Tell pane preset: no line numbers, folding, or glyph margin; narrow
     *  decorations gutter; no overview ruler; no scroll past the last line. */
    compact?: boolean;
    /** Extra construction options, merged last. */
    options?: Record<string, unknown>;
    /** Register this editor as the LSP-backed one (`setLspEditor`) while mounted. */
    lsp?: boolean;
    /** Text shown centered in the host while Monaco loads. */
    placeholder?: string;
    /** Let Monaco suggest words already typed in the buffer (default off). */
    wordBasedSuggestions?: boolean;
  }>(),
  {
    language: "kif",
    readOnly: false,
    compact: false,
    options: () => ({}),
    lsp: false,
    placeholder: "Loading editor...",
    wordBasedSuggestions: false,
  },
);

const emit = defineEmits<{
  "update:modelValue": [value: string];
  ready: [editor: Monaco.editor.IStandaloneCodeEditor, monaco: MonacoNs];
  cursor: [pos: { lineNumber: number; column: number }];
  failed: [error: Error];
}>();

const shell = useShellStore();

const host = ref<HTMLElement | null>(null);
const mount = ref<HTMLElement | null>(null);
const editor = shallowRef<Monaco.editor.IStandaloneCodeEditor | null>(null);
const ready = ref(false);
const failed = ref(false);
const fallbackText = ref(props.modelValue);

let monaco: MonacoNs | null = null;
let unmounted = false;

const COMPACT_OPTIONS: Monaco.editor.IStandaloneEditorConstructionOptions = {
  lineNumbers: "off",
  folding: false,
  glyphMargin: false,
  lineDecorationsWidth: 6,
  scrollBeyondLastLine: false,
  overviewRulerLanes: 0,
  hideCursorInOverviewRuler: true,
};

function constructionOptions(): Monaco.editor.IStandaloneEditorConstructionOptions {
  return {
    value: props.modelValue,
    language: props.language,
    theme: themeName(shell.isDark),
    readOnly: props.readOnly,
    automaticLayout: true,
    minimap: { enabled: false },
    // Otherwise a hover diagnostic near the editor's edge gets clipped by the
    // container's own border/overflow instead of floating over it (#21).
    fixedOverflowWidgets: true,
    fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
    fontSize: 13,
    wordBasedSuggestions: props.wordBasedSuggestions ? "allDocuments" : "off",
    // The default ('configuredByTheme') would silently no-op unless a
    // theme opts in; force it on so the LSP's KB-aware semantic tokens
    // actually render.
    "semanticHighlighting.enabled": true,
    ...(props.compact ? COMPACT_OPTIONS : {}),
    ...(props.options as Monaco.editor.IStandaloneEditorConstructionOptions),
  };
}

onMounted(async () => {
  let m: MonacoNs;
  try {
    m = await loadMonaco();
  } catch (e) {
    if (unmounted) return;
    failed.value = true;
    emit("failed", e instanceof Error ? e : new Error(String(e)));
    return;
  }
  if (unmounted || !mount.value) return;
  monaco = m;
  const ed = m.editor.create(mount.value, constructionOptions());
  editor.value = ed;
  ready.value = true;
  ed.onDidChangeModelContent(() => emit("update:modelValue", ed.getValue()));
  ed.onDidChangeCursorPosition((e) => {
    emit("cursor", {
      lineNumber: e.position.lineNumber,
      column: e.position.column,
    });
  });
  if (props.lsp) setLspEditor(ed);
  emit("ready", ed, m);
});

onBeforeUnmount(() => {
  unmounted = true;
  const ed = editor.value;
  if (!ed) return;
  if (lspEditor() === ed) setLspEditor(null);
  const model = ed.getModel();
  ed.dispose();
  model?.dispose();
  editor.value = null;
});

watch(
  () => props.modelValue,
  (v) => {
    fallbackText.value = v;
    const ed = editor.value;
    if (ed && ed.getValue() !== v) {
      const pos = ed.getPosition();
      ed.setValue(v);
      if (pos) ed.setPosition(pos);
    }
  },
);

watch(fallbackText, (v) => {
  if (failed.value && v !== props.modelValue) emit("update:modelValue", v);
});

watch(
  () => shell.isDark,
  (dark) => {
    monaco?.editor.setTheme(themeName(dark));
  },
);

watch(
  () => props.language,
  (lang) => {
    const model = editor.value?.getModel();
    if (monaco && model) monaco.editor.setModelLanguage(model, lang);
  },
);

watch(
  () => props.readOnly,
  (readOnly) => {
    editor.value?.updateOptions({ readOnly });
  },
);

watch(
  () => props.lsp,
  (on) => {
    const ed = editor.value;
    if (!ed) return;
    if (on) setLspEditor(ed);
    else if (lspEditor() === ed) setLspEditor(null);
  },
);

/** Replace the 'sigma'-owned markers on the editor's model. */
function setMarkers(diags: any[]): void {
  const model = editor.value?.getModel();
  if (!monaco || !model) return;
  monaco.editor.setModelMarkers(model, "sigma", diagsToMarkers(monaco, diags));
}

/** Scroll `line` into the center, move the caret there, and focus. */
function revealLine(line: number, column = 1): void {
  const ed = editor.value;
  if (!ed) return;
  ed.revealLineInCenter(line);
  ed.setPosition({ lineNumber: line, column });
  ed.focus();
}

function focus(): void {
  editor.value?.focus();
}

function layout(): void {
  editor.value?.layout();
}

/** Run one of the editor's actions by id (e.g. `editor.action.formatDocument`). */
function runAction(id: string): void {
  editor.value?.getAction(id)?.run();
}

function getValue(): string {
  return editor.value ? editor.value.getValue() : props.modelValue;
}

function getPosition(): { lineNumber: number; column: number } | null {
  const pos = editor.value?.getPosition();
  return pos ? { lineNumber: pos.lineNumber, column: pos.column } : null;
}

defineExpose({
  editor,
  setMarkers,
  revealLine,
  focus,
  layout,
  runAction,
  getValue,
  getPosition,
});
</script>

<template>
  <div class="monaco-host" ref="host">
    <div v-if="failed" class="fallback">
      <textarea
        v-model="fallbackText"
        :readonly="readOnly"
        spellcheck="false"
      ></textarea>
    </div>
    <div v-else-if="!ready" class="placeholder">{{ placeholder }}</div>
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

.fallback {
  position: absolute;
  inset: 0;
  z-index: 1;
}

.fallback textarea {
  width: 100%;
  height: 100%;
  box-sizing: border-box;
  resize: none;
  border: 0;
  background: var(--card);
  color: var(--fg);
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 13px;
  padding: 6px 8px;
}
</style>
