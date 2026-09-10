<script setup lang="ts">
/**
 * Edit tab: the in-browser IDE (Monaco) for KIF constituents -- file picker,
 * live validation, save, the TPTP split pane, the contribution panel, and
 * the fullscreen toggle. Owns the buffer and the current file.
 */
import {
  computed,
  nextTick,
  onActivated,
  onBeforeUnmount,
  onDeactivated,
  ref,
  shallowRef,
  watch,
} from "vue";
import type * as Monaco from "monaco-editor/esm/vs/editor/editor.api.js";
import Card from "../components/Card.vue";
import DropMenu from "../components/DropMenu.vue";
import MonacoEditor from "../components/MonacoEditor.vue";
import StatusLine from "../components/StatusLine.vue";
import ContributePanel from "../components/edit/ContributePanel.vue";
import DiffDialog from "../components/edit/DiffDialog.vue";
import OpenFileDialog from "../components/edit/OpenFileDialog.vue";
import ProblemsPanel from "../components/edit/ProblemsPanel.vue";
import TptpPane from "../components/edit/TptpPane.vue";
import { useStatus } from "../composables/useStatus";
import { useTabQuery } from "../composables/useTabQuery";
import type { Constituent } from "../models/Constituent";
import { LocalOrigin, type Origin } from "../models/Origin";
import { navigate, updateParams } from "../router";
import { lspRequest, lspSyncDocument } from "../services/lsp";
import type { MonacoNs } from "../services/monaco";
import { call } from "../services/sigma";
import { useAuthStore } from "../stores/auth";
import { useChangesStore, type ChangeRow } from "../stores/changes";
import { useKBStore } from "../stores/kb";
import { downloadText, errMsg } from "../utils/format";

const NEW_FILE_TEXT = "; New KIF file\n";

// Two icon variants for the one button: outward corner-brackets to enter,
// inward ones to exit -- the same visual language most fullscreen toggles use.
const FULLSCREEN_ENTER_PATH =
  "M2 5.5V2.75A.75.75 0 0 1 2.75 2h2.75M2 10.5v2.75c0 .414.336.75.75.75h2.75" +
  "M14 5.5V2.75a.75.75 0 0 0-.75-.75h-2.75M14 10.5v2.75a.75.75 0 0 1-.75.75h-2.75";
const FULLSCREEN_EXIT_PATH =
  "M5.5 2v2.75a.75.75 0 0 1-.75.75H2M10.5 2v2.75c0 .414.336.75.75.75H14" +
  "M5.5 14v-2.75a.75.75 0 0 0-.75-.75H2M10.5 14v-2.75c0-.414.336-.75.75-.75H14";

const kb = useKBStore();
const changes = useChangesStore();
const auth = useAuthStore();

const ed = ref<InstanceType<typeof MonacoEditor> | null>(null);
const tptpPane = ref<InstanceType<typeof TptpPane> | null>(null);
const downloadBtn = ref<HTMLElement | null>(null);
const splitBtn = ref<HTMLElement | null>(null);

const current = ref<{ name: string; origin: Origin } | null>(null);
const text = ref(NEW_FILE_TEXT);
const diags = shallowRef<any[]>([]);
const cursor = ref<{ lineNumber: number; column: number } | null>(null);
/** Toolbar status; when `statusLink` is set it renders as a link into the
 *  Diagnostics tab, filtered to this file. */
const status = ref("");
const statusLink = ref<{ file: string; line: number } | null>(null);
const saveStatus = useStatus();
const log = useStatus();
const saving = ref(false);
const fullscreen = ref(false);
const tptpOpen = ref(false);
const downloadMenuOpen = ref(false);
const splitMenuOpen = ref(false);
const ghPanelOpen = ref(false);
const openDialogOpen = ref(false);
const diffOpen = ref(false);
const diffRow = ref<ChangeRow | null>(null);

// -- Editor -------------------------------------------------------------------

let resolveReady: () => void = () => {};
const editorReady = new Promise<void>((r) => {
  resolveReady = r;
});

function onReady(editor: Monaco.editor.IStandaloneCodeEditor, _m: MonacoNs) {
  // Right-click a symbol -> its man page. Monaco does not reliably move the
  // caret on right-click, so the click's own position is captured and used in
  // preference to the cursor.
  let ctxPos: Monaco.IPosition | null = null;
  editor.onContextMenu((e) => {
    ctxPos = e.target?.position ?? null;
  });
  editor.addAction({
    id: "sumo.open-documentation",
    label: "Open SUMO documentation",
    contextMenuGroupId: "navigation",
    contextMenuOrder: 0,
    run: (e) => {
      const model = e.getModel();
      const pos = ctxPos || e.getPosition();
      ctxPos = null;
      const word = model && pos && model.getWordAtPosition(pos);
      if (!word) return;
      // `?x` / `@row` are KIF variables, not terms -- nothing to document.
      const prev =
        word.startColumn > 1
          ? model.getValueInRange({
              startLineNumber: pos.lineNumber,
              startColumn: word.startColumn - 1,
              endLineNumber: pos.lineNumber,
              endColumn: word.startColumn,
            })
          : "";
      if (prev === "?" || prev === "@") return;
      navigate("browse", { sym: word.word });
    },
  });
  resolveReady();
  scheduleValidate();
}

function onFailed(e: Error) {
  log.set("Failed to load the editor: " + errMsg(e), true);
  resolveReady();
}

function onCursor(pos: { lineNumber: number; column: number }) {
  cursor.value = pos;
}

// -- Validation -----------------------------------------------------------------

let validateTimer: ReturnType<typeof setTimeout> | undefined;
// Coalesce validate requests: whole-file validation of a large constituent
// takes a couple of seconds in the worker, so at most one runs at a time and
// at most one rerun is queued behind it -- typing never piles up a backlog.
let validateBusy = false;
let validateQueued = false;

// One console line per LANE CHANGE (not per keystroke): tells apart "the LSP
// lane engaged for file X" from "this buffer has no backing constituent, so
// only parse checking runs" -- the two look identical in the UI when the LSP
// lane fails to engage.
let lastValidateLane = "";
function logValidateLane(lane: string) {
  if (lane === lastValidateLane) return;
  lastValidateLane = lane;
  console.info("[edit] validate lane:", lane);
}

function scheduleValidate() {
  clearTimeout(validateTimer);
  validateTimer = setTimeout(runValidate, 400);
}

async function runValidate() {
  if (validateBusy) {
    validateQueued = true;
    return;
  }
  validateBusy = true;
  try {
    await validateNow();
  } finally {
    validateBusy = false;
    if (validateQueued) {
      validateQueued = false;
      runValidate();
    }
  }
}

async function validateNow() {
  const buffer = text.value;
  const file = current.value;
  // A buffer belonging to a loaded constituent is diffed into the live KB and
  // validated against it, so semantic diagnostics resolve. A scratch buffer has
  // no backing file, so it falls back to parse-only checking in a throwaway KB.
  const known = file ? kb.find(file.name, file.origin.kind) : undefined;
  logValidateLane(known ? `lsp (${known.name})` : "scratch (parse-only)");
  let result: any[];
  try {
    result = known
      ? await lspSyncDocument(known.name, buffer)
      : (await call("validateFormula", { kif: buffer })).diagnostics;
  } catch (e) {
    status.value = "validate failed: " + errMsg(e);
    statusLink.value = null;
    return;
  }
  if (text.value !== buffer || current.value !== file) return; // stale
  ed.value?.setMarkers(result);
  diags.value = result;
  if (!result.length) {
    status.value = "no diagnostics";
    statusLink.value = null;
  } else {
    // Link the count into the Diagnostics tab, filtered to this file and
    // landing on the diagnostic nearest the first problem in the buffer.
    const errs = result.filter((d) => d.severity === "error").length;
    status.value =
      `${result.length} diagnostic${result.length === 1 ? "" : "s"}` +
      (errs ? ` (${errs} error${errs === 1 ? "" : "s"})` : "");
    statusLink.value = file
      ? { file: file.name, line: result[0]?.line || 0 }
      : null;
  }
  tptpPane.value?.scheduleRefresh();
}

watch(text, scheduleValidate);

// -- File state -----------------------------------------------------------------

/** True when the buffer holds edits that have not been saved. What a pull
 *  request carries is the SAVED text, so this is also what stops the
 *  Contribute panel from pushing a version the user has already moved past. */
const dirty = computed(() => {
  const f = current.value;
  if (!f) return false;
  const c = kb.find(f.name, f.origin.kind);
  return !!c && c.text !== text.value;
});

const fileLabel = computed(() => {
  const name = current.value ? current.value.name : "new file (unsaved)";
  return dirty.value ? `${name} •` : name;
});

/** Save persists wherever the buffer came from and is offered for both
 *  writable origins -- a `file` upload to OPFS, a `sumo` file to the edit
 *  store. A `url` buffer has nowhere to be saved. */
const saveHidden = computed(() => current.value?.origin.kind === "url");

const rowKey = (r: { name: string; origin: string }) => `${r.origin}:${r.name}`;

// Files the user has already been shown a conflict dialog for; reset when
// the row disappears.
const warned = new Set<string>();
watch(
  () => changes.rows,
  (rows) => {
    const live = new Set(rows.map(rowKey));
    for (const k of [...warned]) if (!live.has(k)) warned.delete(k);
  },
);

/** Warn once per file when the editor opens something upstream has moved on
 *  from. Opening the file is the moment the user can act on it; the badge and
 *  the row's "resolve" link carry the same signal the rest of the time. */
function checkStaleOnOpen() {
  const f = current.value;
  if (!f || f.origin.kind !== "sumo") return;
  const row = changes.rows.find(
    (r) => r.name === f.name && r.origin === "sumo",
  );
  if (!row?.stale) return;
  const k = rowKey(row);
  if (warned.has(k)) return;
  warned.add(k);
  openDiff(row);
}

function openFile(c: Constituent | null) {
  current.value = c ? { name: c.name, origin: c.origin } : null;
  text.value = c ? c.text : NEW_FILE_TEXT;
  saveStatus.clear();
  const kind = c ? c.origin.kind : "file"; // an unsaved new file is local
  log.set(
    kind === "url"
      ? "Loaded from a URL — it can be edited and downloaded here, but not saved or submitted."
      : "",
  );
  scheduleValidate();
  checkStaleOnOpen();
}

function onPick(c: Constituent) {
  openFile(c);
  updateParams({ file: c.name });
}

function onCreate() {
  openFile(null);
  updateParams({});
}

// -- Deep links -----------------------------------------------------------------

const { onQuery, str, num } = useTabQuery(["edit"]);

onQuery(async (q) => {
  const file = str(q.file);
  if (file && file !== current.value?.name) {
    // Match on name alone -- a deep link shouldn't have to know the origin.
    const c = kb.find(file);
    if (c) openFile(c);
    else log.set(`${file} is not among the loaded constituents.`, true);
  }
  const line = num(q.l);
  if (line) {
    await editorReady;
    await nextTick();
    ed.value?.revealLine(line);
  }
});

// -- Save -------------------------------------------------------------------------

async function onSave() {
  let name: string;
  let origin: Origin;
  if (current.value) {
    ({ name, origin } = current.value);
  } else {
    // A file created via "+ New file" has no name yet -- ask for one now.
    const entered = window.prompt(
      "Filename for this new file:",
      "untitled.kif",
    );
    if (entered === null) return; // cancelled -- leave the buffer as-is, no status change
    name = entered.trim();
    if (!name) {
      saveStatus.set("Enter a filename to save.", true);
      return;
    }
    origin = new LocalOrigin();
  }

  saving.value = true; // icon-only button: disable, don't swap the label
  try {
    const r = await kb.updateConstituentText(name, text.value, origin);
    current.value = { name, origin };
    log.clear();
    scheduleValidate();
    const saved =
      origin.kind === "sumo"
        ? `Saved ${name} locally — it stays here until you push it to GitHub.`
        : `Saved ${name}.`;
    saveStatus.set(r.notices.length ? r.notices.join(" | ") : saved);
  } catch (e) {
    saveStatus.fail(e);
  } finally {
    saving.value = false;
  }
}

// Delegates to Monaco's own format-document command rather than calling
// formatKif + applying the edit by hand -- Monaco's command already runs it
// through the SAME registered provider (defineKifLanguage) and applies the
// result as one undoable edit, preserving cursor/scroll/undo history the way
// a hand-rolled setValue() would not.
function onFormat() {
  ed.value?.runAction("editor.action.formatDocument");
}

// -- Download menu ---------------------------------------------------------------
//
// The current buffer as KIF (a real download, independent of the in-browser
// OPFS/KB state), or the WHOLE knowledge base's TPTP translation (the same
// dump the TPTP pane shows -- an individual file's translation is meaningless
// without the rest of the KB's declarations, so there is no per-file option).

function onDownloadKif() {
  downloadMenuOpen.value = false;
  downloadText(current.value ? current.value.name : "untitled.kif", text.value);
}

async function onDownloadTptp() {
  downloadMenuOpen.value = false;
  const prior = { text: status.value, link: statusLink.value };
  status.value = "generating TPTP…";
  statusLink.value = null;
  try {
    const r = await lspRequest<{ tptp: string }>("sumo/toTptp", {});
    downloadText("knowledge-base.tptp", r?.tptp ?? "");
    status.value = prior.text;
    statusLink.value = prior.link;
  } catch (e) {
    status.value = "TPTP translation failed: " + errMsg(e);
  }
}

// -- Split-screen menu -----------------------------------------------------------
//
// While a split view is open, the button acts as a plain dismiss -- the menu
// only appears when there is something to choose.

function onSplitClick() {
  if (tptpOpen.value) {
    tptpOpen.value = false;
    return;
  }
  splitMenuOpen.value = !splitMenuOpen.value;
}

function onTptpTranslation() {
  splitMenuOpen.value = false;
  tptpOpen.value = !tptpOpen.value;
}

watch(tptpOpen, async () => {
  await nextTick();
  ed.value?.layout();
});

// -- Fullscreen ------------------------------------------------------------------
//
// Lifts the tab (toolbar + editor + log) to cover the viewport via a
// body-level class (body.edit-fullscreen), rather than moving/re-parenting
// any DOM -- the CSS alone describes the two end states. The View Transitions
// API (where supported) morphs between them automatically; browsers without
// it just get an instant toggle, still fully functional.

const fullscreenTitle = computed(() =>
  fullscreen.value ? "Exit fullscreen editor" : "Toggle fullscreen editor",
);
const fullscreenIconPath = computed(() =>
  fullscreen.value ? FULLSCREEN_EXIT_PATH : FULLSCREEN_ENTER_PATH,
);

function setFullscreen(on: boolean) {
  if (on === fullscreen.value) return;
  const apply = () => {
    fullscreen.value = on;
    document.body.classList.toggle("edit-fullscreen", on);
    // The container's size just changed outside of any window resize, which
    // is the one case automaticLayout's own ResizeObserver can lag behind --
    // an explicit layout() is cheap insurance against a stale-sized canvas.
    ed.value?.layout();
  };
  if (document.startViewTransition) document.startViewTransition(apply);
  else apply();
}

function onKeydown(e: KeyboardEvent) {
  if (e.key === "Escape" && fullscreen.value) setFullscreen(false);
}

onActivated(() => document.addEventListener("keydown", onKeydown));
onDeactivated(() => {
  document.removeEventListener("keydown", onKeydown);
  setFullscreen(false);
});
onBeforeUnmount(() => {
  document.removeEventListener("keydown", onKeydown);
  clearTimeout(validateTimer);
  setFullscreen(false);
});

// -- GitHub ------------------------------------------------------------------------

const ghCount = computed(() => changes.actionableCount);
const ghStale = computed(() => changes.rows.some((r) => r.stale));
const ghTitle = computed(() => {
  const n = ghCount.value;
  return n
    ? `GitHub — ${n} file${n === 1 ? "" : "s"} changed and not yet pushed`
    : "Submit changes to GitHub as a pull request";
});

function onPropose() {
  if (!auth.signedIn) {
    auth.openLoginDialog();
    return;
  }
  ghPanelOpen.value = !ghPanelOpen.value;
}

function openDiff(row: ChangeRow) {
  diffRow.value = row;
  diffOpen.value = true;
}

function onTaken(t: string) {
  const f = current.value;
  const row = diffRow.value;
  if (f && row && f.name === row.name && f.origin.kind === row.origin)
    text.value = t;
}

function onKeep(row: ChangeRow) {
  changes.acknowledgeUpstream(row.name, row.origin);
}

function onJump({ line, col }: { line: number; col: number }) {
  ed.value?.revealLine(line, col);
}
</script>

<template>
  <div class="edit-tab">
    <Card>
      <div class="edit-toolbar">
        <div class="btn-group" role="toolbar" aria-label="Editor actions">
          <button
            type="button"
            title="Open a file, or create a new one"
            aria-label="Open a file, or create a new one"
            @click="openDialogOpen = true"
          >
            <svg
              viewBox="0 0 16 16"
              width="16"
              height="16"
              fill="currentColor"
              aria-hidden="true"
            >
              <path
                d="M1.75 1A1.75 1.75 0 0 0 0 2.75v10.5C0 14.216.784 15 1.75 15h12.5A1.75 1.75 0 0 0 16 13.25v-8.5A1.75 1.75 0 0 0 14.25 3H7.5a.25.25 0 0 1-.2-.1l-.9-1.2A1.75 1.75 0 0 0 5 1H1.75Z"
              />
            </svg>
          </button>
          <button
            v-show="!saveHidden"
            type="button"
            title="Save to the in-browser knowledge base"
            aria-label="Save to the in-browser knowledge base"
            :disabled="saving"
            @click="onSave"
          >
            <svg
              viewBox="0 0 16 16"
              width="16"
              height="16"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <path
                d="M2.75 2.75h8.19a1 1 0 0 1 .7.3l1.31 1.3a1 1 0 0 1 .3.71v8.19h-10.5z"
              />
              <path d="M5 2.75V6.25h5V2.75M5 13.25V9.25h6v4" />
            </svg>
          </button>
          <button
            type="button"
            title="Format document (fix indentation to match paren nesting)"
            aria-label="Format document"
            @click="onFormat"
          >
            <svg
              viewBox="0 0 16 16"
              width="16"
              height="16"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              aria-hidden="true"
            >
              <path d="M2 3h12M2 6.5h8M2 10h10M2 13.5h6" />
            </svg>
          </button>
          <button
            ref="downloadBtn"
            type="button"
            aria-haspopup="menu"
            :aria-expanded="downloadMenuOpen"
            title="Download…"
            aria-label="Download this file or the TPTP translation"
            @click="downloadMenuOpen = !downloadMenuOpen"
          >
            <svg
              viewBox="0 0 16 16"
              width="16"
              height="16"
              fill="currentColor"
              aria-hidden="true"
            >
              <path
                d="M7.25 1.75a.75.75 0 0 1 1.5 0v6.69l2.22-2.22a.75.75 0 1 1 1.06 1.06l-3.5 3.5a.75.75 0 0 1-1.06 0l-3.5-3.5a.75.75 0 1 1 1.06-1.06l2.22 2.22ZM2.5 13a.75.75 0 0 0 0 1.5h11a.75.75 0 0 0 0-1.5Z"
              />
            </svg>
          </button>
          <button
            class="gh-propose"
            type="button"
            :aria-expanded="ghPanelOpen"
            :title="ghTitle"
            aria-label="Submit changes to GitHub as a pull request"
            @click="onPropose"
          >
            <svg
              class="gh-mark"
              viewBox="0 0 16 16"
              width="16"
              height="16"
              fill="currentColor"
              aria-hidden="true"
            >
              <path
                d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27s1.36.09 2 .27c1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.012 8.012 0 0 0 16 8c0-4.42-3.58-8-8-8Z"
              />
            </svg>
            <span
              v-show="ghCount"
              class="gh-badge"
              :class="{ stale: ghStale }"
              >{{ ghCount || "" }}</span
            >
          </button>
          <button
            type="button"
            :aria-pressed="fullscreen"
            :title="fullscreenTitle"
            aria-label="Toggle fullscreen editor"
            @click="setFullscreen(!fullscreen)"
          >
            <svg
              viewBox="0 0 16 16"
              width="16"
              height="16"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <path :d="fullscreenIconPath" />
            </svg>
          </button>
          <button
            ref="splitBtn"
            type="button"
            :aria-pressed="tptpOpen"
            aria-haspopup="menu"
            :aria-expanded="splitMenuOpen"
            title="Split-screen views…"
            aria-label="Split-screen views"
            @click="onSplitClick"
          >
            <svg
              viewBox="0 0 16 16"
              width="16"
              height="16"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <rect x="1" y="2.5" width="6" height="11" rx="1" />
              <rect x="9" y="2.5" width="6" height="11" rx="1" />
            </svg>
          </button>
        </div>
        <span class="file-name" :title="dirty ? 'Unsaved changes' : ''">{{
          fileLabel
        }}</span>
        <span class="spacer"></span>
        <span class="edit-status hint">
          <a
            v-if="statusLink"
            class="jump-diag"
            title="Show these in the Diagnostics tab"
            @click="
              navigate('diagnostics', {
                file: statusLink.file,
                l: statusLink.line,
              })
            "
            >{{ status }}</a
          >
          <template v-else>{{ status }}</template>
        </span>
      </div>
      <!-- Save feedback (success/error) lives right under the toolbar, not
         buried below the editor -- the one place a user glances after
         clicking Save. -->
      <StatusLine :text="saveStatus.text" :error="saveStatus.error" />

      <!-- Open-file dialog: pick a loaded constituent or start a new file.
         A new file starts unnamed -- Save prompts for a filename the
         first time, rather than asking for one up front here. -->
      <OpenFileDialog
        v-model="openDialogOpen"
        @pick="onPick"
        @create="onCreate"
      />

      <!-- Contribute the selected changes upstream as a branch + pull request -->
      <ContributePanel
        v-show="ghPanelOpen"
        :open="ghPanelOpen"
        :current="current"
        :dirty="dirty"
        @diff="openDiff"
      />

      <DiffDialog
        v-model="diffOpen"
        :row="diffRow"
        @taken="onTaken"
        @keep="onKeep"
      />
    </Card>

    <Card class="edit-pane" :class="{ split: tptpOpen }">
      <div class="editor-container">
        <MonacoEditor
          ref="ed"
          v-model="text"
          lsp
          placeholder="Loading editor…"
          @cursor="onCursor"
          @ready="onReady"
          @failed="onFailed"
        />
      </div>
      <TptpPane
        v-show="tptpOpen"
        ref="tptpPane"
        :open="tptpOpen"
        :current="current"
        :cursor="cursor"
        :text="text"
      />
    </Card>

    <ProblemsPanel :diags="diags" :file="current?.name" @jump="onJump" />
    <StatusLine :text="log.text" :error="log.error" />

    <!-- Both menus live OUTSIDE .btn-group (whose overflow:hidden would clip
       them, and whose `button + button` divider chain a nested div would
       break); DropMenu fixes them under their anchor button. -->
    <DropMenu v-model="downloadMenuOpen" :anchor="downloadBtn">
      <button type="button" role="menuitem" @click="onDownloadKif">
        This file (KIF)
      </button>
      <button type="button" role="menuitem" @click="onDownloadTptp">
        Whole knowledge base (TPTP)
      </button>
    </DropMenu>
    <DropMenu v-model="splitMenuOpen" :anchor="splitBtn">
      <button type="button" role="menuitem" @click="onTptpTranslation">
        TPTP Translation
      </button>
    </DropMenu>
  </div>
</template>

<style scoped>
/* Edit tab: one compact toolbar row -- filename on the left, status + actions
   pushed right; wraps gracefully when narrow. */
.edit-toolbar {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}
.edit-toolbar .spacer {
  flex: 1 1 0;
}
.edit-toolbar .edit-status {
  text-align: right;
}
.file-name {
  font-family: var(--mono);
  font-size: 13px;
}
/* Joined icon button group (open - save - download - GitHub). */
.btn-group {
  display: inline-flex;
  border: 1px solid var(--line);
  border-radius: 8px;
  overflow: hidden;
  background: var(--bg);
}
.btn-group > button {
  font: inherit;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  background: none;
  border: none;
  color: var(--fg);
  cursor: pointer;
  padding: 0 12px;
  height: 41px;
}
.btn-group > button + button {
  border-left: 1px solid var(--line);
}
.btn-group > button:hover {
  background: var(--card);
  color: var(--accent);
}
.btn-group > button[aria-expanded="true"],
.btn-group > button[aria-pressed="true"] {
  color: var(--accent);
  background: color-mix(in srgb, var(--accent) 10%, transparent);
}
/* GitHub mark inside the Submit-change button */
.gh-propose {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  position: relative;
}
.gh-mark {
  flex: 0 0 auto;
}
/* Unpushed-change count, on the GitHub button. Amber once any tracked file
   has moved upstream underneath it -- the count alone cannot say that. */
.gh-badge {
  min-width: 16px;
  padding: 0 5px;
  border-radius: 999px;
  font-size: 10px;
  font-weight: 700;
  line-height: 16px;
  text-align: center;
  background: color-mix(in srgb, var(--accent) 22%, transparent);
  color: var(--accent);
}
.gh-badge.stale {
  background: color-mix(in srgb, var(--warn) 26%, transparent);
  color: var(--warn);
}
/* IDE diagnostic count -> Diagnostics tab */
a.jump-diag {
  cursor: pointer;
  color: var(--accent);
}
a.jump-diag:hover {
  text-decoration: underline;
}

.card.edit-pane {
  padding: 0;
  overflow: hidden;
}
.editor-container {
  height: 520px;
}
/* TPTP preview pane: BELOW the editor normally, BESIDE it in fullscreen
   (see the body.edit-fullscreen rules further down). The toggle button
   only shows/hides it; placement follows the fullscreen state. */
.edit-pane.split {
  display: flex;
  flex-direction: column;
  align-items: stretch;
}
.edit-pane.split .editor-container {
  flex: 0 0 auto;
  min-width: 0;
  height: 520px;
}

/* Fullscreen editor: the tab root (toolbar + editor + log) lifted out of
   <main>'s max-width/flow to cover the viewport. Animated via the View
   Transitions API where supported (setFullscreen wraps the class toggle in
   document.startViewTransition -- these rules only need to describe the two
   end states; the browser interpolates the geometry morph between them);
   toggling the class directly is still correct with no transition library
   on browsers that lack it. */
:global(body.edit-fullscreen) .edit-tab {
  position: fixed;
  inset: 0;
  z-index: 200;
  margin: 0;
  max-width: none;
  background: var(--bg);
  padding: 16px 20px;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
:global(body.edit-fullscreen) .edit-pane {
  flex: 1 1 auto;
  display: flex;
  flex-direction: column;
  min-height: 0;
}
:global(body.edit-fullscreen) .editor-container {
  flex: 1 1 auto;
  height: auto;
}
/* Fullscreen flips the pane to the side: row layout, equal halves, both
   columns fluid-height. */
:global(body.edit-fullscreen) .edit-pane.split {
  flex-direction: row;
}
:global(body.edit-fullscreen) .edit-pane.split .editor-container {
  flex: 1 1 0;
  height: auto;
}
</style>
