<script setup lang="ts">
/**
 * Split TPTP preview: a read-only pane showing the WHOLE knowledge base's
 * TPTP translation (an individual file's translation is meaningless without
 * the rest of the KB's declarations), scrolled to follow the edited file as
 * the cursor moves. The pane works for any buffer; only cursor-follow needs
 * a real loaded file to resolve positions against.
 */
import { computed, nextTick, onBeforeUnmount, ref, watch } from "vue";
import MonacoEditor from "../MonacoEditor.vue";
import { monacoNs } from "../../services/monaco";
import { lspRequest, tagToUri } from "../../services/lsp";
import { useKBStore } from "../../stores/kb";
import type { Origin } from "../../models/Origin";
import { errMsg } from "../../utils/format";

// Retranslating the whole KB is the heavy half of this feature, so it gets a
// longer debounce than edit-validate's 400ms rather than riding along with it.
const TPTP_REFRESH_DELAY_MS = 1200;

const props = defineProps<{
  /** Whether the pane is showing; nothing is translated while it is not. */
  open: boolean;
  /** The file in the editor, or null for a scratch buffer. */
  current: { name: string; origin: Origin } | null;
  /** The editor's caret, followed into the translation. */
  cursor: { lineNumber: number; column: number } | null;
  /** The editor's text; a change queues a retranslation. */
  text: string;
}>();

const kb = useKBStore();

const ed = ref<InstanceType<typeof MonacoEditor> | null>(null);
const tptpText = ref("");
const status = ref("");

let decorationIds: string[] = [];
let refreshTimer: ReturnType<typeof setTimeout> | undefined;
let followTimer: ReturnType<typeof setTimeout> | undefined;

/** Cursor-follow needs a real loaded file to resolve positions against the
 *  live KB; the pane itself does not. */
const usable = computed(
  () =>
    !!props.current && !!kb.find(props.current.name, props.current.origin.kind),
);

/** Retranslate the whole KB into the pane. Only while it is actually open --
 *  every caller can fire unconditionally. */
async function refresh() {
  if (!props.open) return;
  status.value = "generating…";
  try {
    const text =
      (await lspRequest<{ tptp: string }>("sumo/toTptp", {}))?.tptp ?? "";
    if (!props.open) return;
    // Most edits leave the dump untouched; setValue on a KB-sized model resets
    // its tokenization, folding and scroll position, so skip an identical one.
    if (text !== tptpText.value) tptpText.value = text;
    status.value = `${text.split("\n").length} lines`;
    follow();
  } catch (e) {
    status.value = "translation failed: " + errMsg(e);
  }
}

/** Queue a retranslation. Used by the typing path, which must not wait on it. */
function scheduleRefresh() {
  if (!props.open) return;
  clearTimeout(refreshTimer);
  refreshTimer = setTimeout(refresh, TPTP_REFRESH_DELAY_MS);
}

// Cursor-follow: cheap (cache lookups against the last generated dump, no
// retranslation), so it can run on every cursor move with only a light
// debounce -- mainly to avoid flooding postMessage while arrow-keying/scrolling.
function scheduleFollow() {
  clearTimeout(followTimer);
  followTimer = setTimeout(follow, 120);
}

async function follow() {
  const editor = ed.value?.editor;
  const pos = props.cursor;
  if (!props.open || !editor || !pos || !usable.value) return;
  // LSP positions are UTF-16 code units (the server advertises UTF-16),
  // which is exactly Monaco's column encoding -- only the 1-based -> 0-based
  // shift applies; the server does its own byte-offset conversion against
  // the open document.
  let line: number | null | undefined;
  try {
    const r = await lspRequest<{ line: number | null }>(
      "sumo/tptpLineForPosition",
      {
        textDocument: { uri: tagToUri(props.current!.name) },
        position: { line: pos.lineNumber - 1, character: pos.column - 1 },
      },
    );
    line = r?.line;
  } catch {
    return;
  }
  const m = monacoNs();
  if (line == null || !m || !ed.value?.editor) return;
  const lineNumber = line + 1; // 0-based from Rust -> Monaco's 1-based
  editor.revealLineInCenterIfOutsideViewport(lineNumber);
  decorationIds = editor.deltaDecorations(decorationIds, [
    {
      range: new m.Range(lineNumber, 1, lineNumber, 1),
      options: { isWholeLine: true, className: "tptp-follow-line" },
    },
  ]);
}

watch(
  () => props.open,
  async (on) => {
    if (!on) return;
    await nextTick();
    ed.value?.layout();
    await refresh();
  },
);

watch(() => props.text, scheduleRefresh);
watch(() => props.cursor, scheduleFollow);

onBeforeUnmount(() => {
  clearTimeout(refreshTimer);
  clearTimeout(followTimer);
});

defineExpose({ refresh, scheduleRefresh });
</script>

<template>
  <div class="tptp-pane">
    <div class="tptp-pane-header">
      <span>TPTP — whole knowledge base</span>
      <span class="hint">{{ status }}</span>
    </div>
    <div class="tptp-container">
      <MonacoEditor
        ref="ed"
        v-model="tptpText"
        language="tptp"
        read-only
        :options="{ lineNumbers: 'on' }"
        placeholder="Generating…"
      />
    </div>
  </div>
</template>

<style scoped>
/* TPTP preview pane: BELOW the editor normally, BESIDE it in fullscreen
   (see the body.edit-fullscreen rule at the end). The toggle button only
   shows/hides it; placement follows the fullscreen state. */
.tptp-pane {
  flex: 0 0 auto;
  min-width: 0;
  display: flex;
  flex-direction: column;
  border-top: 1px solid var(--line);
  height: 300px;
}
.tptp-pane-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 6px 10px;
  font-size: 12px;
  font-family: var(--mono);
  color: var(--muted);
  border-bottom: 1px solid var(--line);
  flex: 0 0 auto;
}
.tptp-container {
  position: relative;
  flex: 1 1 auto;
  min-height: 0;
}
/* Follow-cursor highlight in the TPTP pane (deltaDecorations className). */
:deep(.tptp-follow-line) {
  background: color-mix(in srgb, var(--accent) 14%, transparent);
}
/* Fullscreen flips the pane to the side: equal halves, fluid height, the
   divider moves from the top edge to the left edge. */
:global(body.edit-fullscreen) .tptp-pane {
  flex: 1 1 0;
  height: auto;
  border-top: none;
  border-left: 1px solid var(--line);
}
</style>
