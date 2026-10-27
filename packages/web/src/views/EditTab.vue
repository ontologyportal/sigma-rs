<script setup lang="ts">
/**
 * Edit tab: the in-browser IDE (Monaco) for KIF constituents -- file picker,
 * live validation, save, the TPTP split pane, the contribution panel, and
 * the fullscreen toggle. Owns the open files and their editor buffers.
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
import FileInfoDialog from "../components/edit/FileInfoDialog.vue";
import ProblemsPanel from "../components/edit/ProblemsPanel.vue";
import TptpPane from "../components/edit/TptpPane.vue";
import { useStatus } from "../composables/useStatus";
import { useTabQuery } from "../composables/useTabQuery";
import { EDIT_LAST_FILE_KEY } from "../constants";
import { engineFile, type Constituent } from "../models/Constituent";

/** A KB constituent or a test file -- anything the editor can open. */
type OpenedFile = Pick<Constituent, "name" | "origin" | "text">;
import {
  LocalOrigin,
  originId,
  serializeOrigin,
  type Origin,
} from "../models/Origin";
import { navigate, updateParams } from "../router";
import { lspRequest, lspSyncDocument } from "../services/lsp";
import type { MonacoNs } from "../services/monaco";
import { call } from "../services/sigma";
import { useAuthStore } from "../stores/auth";
import { useChangesStore, type ChangeRow } from "../stores/changes";
import { useKBStore } from "../stores/kb";
import { useLibraryStore } from "../stores/library";
import { isTestFile, testDialect, useTestsStore } from "../stores/tests";
import { downloadText, errMsg } from "../utils/format";
import type { Diagnostic } from "../stores/kb";

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
const tests = useTestsStore();
const changes = useChangesStore();
const auth = useAuthStore();
const library = useLibraryStore();

const ed = ref<InstanceType<typeof MonacoEditor> | null>(null);
const tptpPane = ref<InstanceType<typeof TptpPane> | null>(null);
const downloadBtn = ref<HTMLElement | null>(null);
const splitBtn = ref<HTMLElement | null>(null);

const current = ref<{ name: string; origin: Origin } | null>(null);
const text = ref(NEW_FILE_TEXT);
let loadedText = NEW_FILE_TEXT;
type EditorTab = {
  id: number;
  file: { name: string; origin: Origin } | null;
  text: string;
  loadedText: string;
};
const tabs = ref<EditorTab[]>([]);
const activeTab = ref<number | null>(null);
let nextTabId = 0;
const draggedTab = ref<number | null>(null);

watch(
  current,
  (file) => {
    const tab = tabs.value.find((t) => t.id === activeTab.value);
    if (tab) tab.file = file;
  },
  { flush: "sync" },
);

const editingTest = computed(
  () => !!current.value && isTestFile(current.value.name),
);
const language = computed(() =>
  editingTest.value ? testDialect(current.value!.name) : "kif",
);
const diags = shallowRef<Diagnostic[]>([]);
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
const diffOpen = ref(false);
const diffRow = ref<ChangeRow | null>(null);
const infoOpen = ref(false);

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
  editor.addAction({
    id: "sumo.audit-sentence",
    label: "Audit this sentence",
    contextMenuGroupId: "navigation",
    contextMenuOrder: 1,
    run: (e) => {
      const pos = ctxPos || e.getPosition();
      ctxPos = null;
      if (pos) auditSentenceAt(e, pos);
    },
  });
  resolveReady();
  scheduleValidate();
}

/** Open the Audit tab focused on the sentence at `pos`: the buffer is synced
 *  into the live KB first, so the byte offset names the same sentence there. */
async function auditSentenceAt(
  editor: Monaco.editor.ICodeEditor,
  pos: Monaco.IPosition,
) {
  const file = current.value;
  const known =
    file && !isTestFile(file.name)
      ? kb.find(file.name, file.origin.kind)
      : undefined;
  const model = editor.getModel();
  if (!known || !model) {
    saveStatus.set("Only sentences of a loaded KB file can be audited.", true);
    return;
  }
  const buffer = model.getValue();
  const offset = new TextEncoder().encode(
    buffer.slice(0, model.getOffsetAt(pos)),
  ).length;
  try {
    await lspSyncDocument(known.file, buffer);
    const { focus } = await call("sentenceAt", { file: known.file, offset });
    if (!focus) {
      saveStatus.set(
        "No sentence there to audit: right-click inside a formula.",
        true,
      );
      return;
    }
    navigate("audit", { focus: known.file, at: offset });
  } catch (e) {
    saveStatus.set("Could not start the audit: " + errMsg(e), true);
  }
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
  let result: Diagnostic[];
  try {
    if (file && isTestFile(file.name)) {
      await call(
        testDialect(file.name) === "kif" ? "parseTest" : "parseTptpTest",
        { name: file.name, text: buffer },
      );
      result = [];
    } else
      result = known
        ? await lspSyncDocument(known.file, buffer)
        : (await call("validateFormula", { kif: buffer })).diagnostics;
  } catch (e) {
    if (text.value !== buffer || current.value !== file) return;
    diags.value = [];
    ed.value?.setMarkers([]);
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
      ? { file: fileParam(file), line: result[0]?.line || 0 }
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
  const c = editingTest.value
    ? tests.find(f.name)
    : kb.find(f.name, f.origin.kind);
  return !!c && c.text !== text.value;
});

const fileLabel = computed(() => {
  const name = current.value ? current.value.name : "new file (unsaved)";
  return dirty.value ? `${name} \u2022` : name;
});

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

let openRequest = 0;

async function openFile(c: OpenedFile | null, tabId?: number) {
  const request = ++openRequest;
  if ((text.value !== loadedText && dirty.value) || saving.value) {
    if (!(await flushAutosave())) {
      if (request === openRequest && current.value)
        updateParams({ file: fileParam(current.value) });
      return false;
    }
  }
  if (request !== openRequest) return false;
  clearTimeout(autosaveTimer);
  const previous = tabs.value.find((t) => t.id === activeTab.value);
  if (previous) {
    previous.text = text.value;
    previous.loadedText = loadedText;
  }
  let tab = tabs.value.find((t) =>
    tabId !== undefined
      ? t.id === tabId
      : c &&
        t.file?.name === c.name &&
        originId(t.file.origin) === originId(c.origin),
  );
  if (c)
    c =
      (isTestFile(c.name)
        ? tests.find(c.name)
        : kb.find(c.name, c.origin.kind)) ?? c;
  if (!tab) {
    tab = {
      id: ++nextTabId,
      file: c ? { name: c.name, origin: c.origin } : null,
      text: c ? c.text : NEW_FILE_TEXT,
      loadedText: c ? c.text : NEW_FILE_TEXT,
    };
    tabs.value.push(tab);
  } else if (c && tab.text === tab.loadedText) {
    tab.text = c.text;
    tab.loadedText = c.text;
  }
  activeTab.value = tab.id;
  current.value = tab.file;
  text.value = tab.text;
  loadedText = tab.loadedText;
  saveStatus.clear();
  diags.value = [];
  ed.value?.setMarkers([]);
  if (editingTest.value) tptpOpen.value = false;
  log.set("");
  scheduleValidate();
  checkStaleOnOpen();
  return true;
}

async function selectTab(tab: EditorTab) {
  if (tab.id === activeTab.value) return true;
  const opened = await openFile(
    tab.file ? { ...tab.file, text: tab.text } : null,
    tab.id,
  );
  if (opened) updateParams(tab.file ? { file: fileParam(tab.file) } : {});
  return opened;
}

async function closeTab(id: number) {
  const tab = tabs.value.find((t) => t.id === id);
  if (!tab) return;
  const active = id === activeTab.value;
  const buffer = active ? text.value : tab.text;
  if (
    !tab.file &&
    buffer !== NEW_FILE_TEXT &&
    !window.confirm(
      "Discard this unnamed file's unsaved changes? Use Save As first to keep them.",
    )
  )
    return;
  if (active) {
    if (!(await flushAutosave())) return;
    const index = tabs.value.indexOf(tab);
    const next = tabs.value[index + 1] ?? tabs.value[index - 1];
    if (next) {
      if (!(await selectTab(next))) return;
    } else {
      activeTab.value = null;
      current.value = null;
      text.value = NEW_FILE_TEXT;
      loadedText = NEW_FILE_TEXT;
      diags.value = [];
      ed.value?.setMarkers([]);
      status.value = "";
      statusLink.value = null;
      cursor.value = null;
      tptpOpen.value = false;
      saveStatus.clear();
      log.clear();
      updateParams({});
    }
  }
  tabs.value = tabs.value.filter((t) => t.id !== id);
  await nextTick();
  ed.value?.closeDocument(String(id));
}

function startTabDrag(event: DragEvent, id: number) {
  draggedTab.value = id;
  if (event.dataTransfer) {
    event.dataTransfer.effectAllowed = "move";
    event.dataTransfer.setData("text/plain", String(id));
  }
}

function dropTab(target: number) {
  const from = tabs.value.findIndex((t) => t.id === draggedTab.value);
  const to = tabs.value.findIndex((t) => t.id === target);
  draggedTab.value = null;
  if (from < 0 || to < 0 || from === to) return;
  const [tab] = tabs.value.splice(from, 1);
  tabs.value.splice(to, 0, tab!);
}

onActivated(() => {
  const t =
    editingTest.value && current.value && tests.find(current.value.name);
  if (t && text.value === loadedText && t.text !== loadedText) openFile(t);
});

async function onCreate() {
  if (await openFile(null)) updateParams({});
}

// -- Deep links -----------------------------------------------------------------

/** The `file` query value naming `f`: a test's own name, else the engine name
 *  diagnostics and citations link with. */
function fileParam(f: { name: string; origin: Origin }): string {
  return isTestFile(f.name) ? f.name : engineFile(f.name, f.origin);
}

const { onQuery, str, num } = useTabQuery(["edit"]);

function storedLastFile(): string {
  try {
    return localStorage.getItem(EDIT_LAST_FILE_KEY) ?? "";
  } catch {
    return "";
  }
}

watch(current, (f) => {
  try {
    if (f) localStorage.setItem(EDIT_LAST_FILE_KEY, fileParam(f));
    else localStorage.removeItem(EDIT_LAST_FILE_KEY);
  } catch {
    // storage unavailable: the tab just opens on a new file next time
  }
});

/** The first visit without a `file` link reopens the last-edited file. */
let restored = false;

onQuery(async (q) => {
  let file = str(q.file);
  const fromStore = !file && !restored;
  restored = true;
  if (fromStore) file = storedLastFile();
  if (file && file !== (current.value && fileParam(current.value))) {
    // Engine name first (what citations link with), then the bare name, which
    // is all a link from before uploads were namespaced carries.
    let c: OpenedFile | undefined =
      tests.find(file) ?? kb.byFile(file) ?? kb.find(file);
    // A library test is fetched on first use.
    if (!c && isTestFile(file))
      c = await tests
        .whenLoaded()
        .then(() => tests.ensure(file))
        .catch(() => undefined);
    if (c) {
      const opened = await openFile(c);
      if (opened && fromStore) updateParams({ ...q, file });
    } else if (fromStore) {
      try {
        localStorage.removeItem(EDIT_LAST_FILE_KEY);
      } catch {
        // nothing to forget
      }
    } else log.set(`${file} is not among the loaded files.`, true);
  }
  const line = num(q.l);
  if (line) {
    await editorReady;
    await nextTick();
    ed.value?.revealLine(line);
  }
});

// -- Save -------------------------------------------------------------------------

const AUTOSAVE_DELAY_MS = 5000;
let autosaveTimer: ReturnType<typeof setTimeout> | undefined;
let saveTask: Promise<boolean> | null = null;
let disposed = false;

function scheduleAutosave() {
  clearTimeout(autosaveTimer);
  if (
    disposed ||
    !current.value ||
    (!saving.value && (!dirty.value || text.value === loadedText))
  )
    return;
  autosaveTimer = setTimeout(() => {
    void flushAutosave(false);
  }, AUTOSAVE_DELAY_MS);
}

async function flushAutosave(drain = true): Promise<boolean> {
  clearTimeout(autosaveTimer);
  while (
    saveTask ||
    (current.value && dirty.value && text.value !== loadedText)
  ) {
    if (saveTask) {
      if (!(await saveTask)) return false;
      if (!drain) {
        scheduleAutosave();
        return true;
      }
    } else {
      const file = current.value!;
      if (!(await saveTo(file.name, file.origin))) return false;
      if (!drain) return true;
    }
  }
  return true;
}

watch(text, scheduleAutosave, { flush: "sync" });

function onBeforeUnload(event: BeforeUnloadEvent) {
  if (
    dirty.value ||
    saving.value ||
    (!current.value && text.value !== loadedText) ||
    tabs.value.some(
      (t) => t.id !== activeTab.value && !t.file && t.text !== NEW_FILE_TEXT,
    )
  ) {
    event.preventDefault();
    event.returnValue = "";
  }
}

function onVisibilityChange() {
  if (document.visibilityState === "hidden") void flushAutosave();
}

async function onSaveAs() {
  await saveWithName(false);
}

async function onRename() {
  await saveWithName(true);
}

async function saveWithName(rename: boolean) {
  if (saving.value) return;
  const source = current.value;
  if (rename && !source) return;
  const entered = window.prompt(
    rename
      ? source?.origin.kind === "file"
        ? "Rename file to:"
        : "Rename locally to (the original source stays unchanged):"
      : "Save As:",
    source?.name.split("/").pop() ?? "untitled.kif",
  );
  if (entered === null) return;
  const name = entered.trim();
  if (
    !name ||
    /[\\/:]/.test(name) ||
    [...name].some((c) => c.charCodeAt(0) < 32) ||
    name === "." ||
    name === ".."
  ) {
    saveStatus.set("Enter a filename without path separators.", true);
    return;
  }
  if (rename && name === source?.name) return;
  if (
    source &&
    (isTestFile(name) !== isTestFile(source.name) ||
      (isTestFile(name) && testDialect(name) !== testDialect(source.name)))
  ) {
    saveStatus.set("Keep the same file type when choosing a new name.", true);
    return;
  }
  if (
    kb.find(name, "file") ||
    library.entry(name, "file") ||
    tests.available.some((t) => t.name === name) ||
    tests.find(name)
  ) {
    saveStatus.set(
      `${name} already exists. Choose a different filename.`,
      true,
    );
    return;
  }
  await saveTo(name, new LocalOrigin(), rename ? source : null);
}

function saveTo(
  name: string,
  origin: Origin,
  renamed: { name: string; origin: Origin } | null = null,
): Promise<boolean> {
  clearTimeout(autosaveTimer);
  const task = writeTo(name, origin, renamed);
  saveTask = task;
  void task.finally(() => {
    if (saveTask === task) saveTask = null;
  });
  return task;
}

async function writeTo(
  name: string,
  origin: Origin,
  renamed: { name: string; origin: Origin } | null = null,
) {
  const buffer = text.value;
  const source = current.value;
  let written = false;
  saving.value = true;
  saveStatus.set(`Saving ${name} locally...`);
  try {
    if (isTestFile(name)) {
      const r = await tests.saveCurrent(buffer, testDialect(name), {
        name,
        origin: serializeOrigin(origin),
      });
      if (!r.saved) {
        saveStatus.set(
          r.notices?.join(" | ") || `Could not save ${name}.`,
          true,
        );
        return false;
      }
    } else {
      const r = await kb.updateConstituentText(name, buffer, origin);
      if (r.notices.length) log.set(r.notices.join(" | "));
      else log.clear();
    }
    written = true;
    if (current.value === source) {
      if (
        !source ||
        source.name !== name ||
        originId(source.origin) !== originId(origin)
      ) {
        current.value = { name, origin };
        updateParams({ file: fileParam({ name, origin }) });
      }
      loadedText = buffer;
      if (!source && text.value !== buffer) scheduleAutosave();
    }
    if (renamed) {
      if (isTestFile(renamed.name)) {
        await tests.remove(renamed.name, originId(renamed.origin));
        await changes.forget(renamed.name, renamed.origin.kind);
      } else await kb.remove(renamed.name, renamed.origin.kind);
      if (renamed.origin.kind === "file")
        await library.deleteEntry(renamed.name, "file");
    }
    scheduleValidate();
    const saved =
      origin.kind === "sumo"
        ? `Saved ${name} locally - it stays here until you push it to GitHub.`
        : origin.kind === "url"
          ? `Saved ${name} locally - the copy at the source URL is unchanged.`
          : `Saved ${name}.`;
    saveStatus.set(
      renamed
        ? `Renamed ${renamed.name} to ${name}.` +
            (renamed.origin.kind !== "file"
              ? " Saved locally; the original source is unchanged."
              : "")
        : saved,
    );
    return true;
  } catch (e) {
    const stored =
      source &&
      (isTestFile(source.name)
        ? tests.find(source.name)
        : kb.find(source.name, source.origin.kind));
    // Local persistence can succeed before KB postprocessing fails.
    if (current.value === source && stored?.text === buffer)
      loadedText = buffer;
    if (renamed && written)
      saveStatus.set(
        `Saved ${name}, but could not finish removing ${renamed.name}: ${errMsg(e)}`,
        true,
      );
    else saveStatus.fail(e);
    return false;
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
  status.value = "generating TPTP...";
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

onActivated(() => {
  document.addEventListener("keydown", onKeydown);
  document.addEventListener("visibilitychange", onVisibilityChange);
  window.addEventListener("beforeunload", onBeforeUnload);
});
onDeactivated(() => {
  void flushAutosave();
  document.removeEventListener("keydown", onKeydown);
  setFullscreen(false);
});
onBeforeUnmount(() => {
  disposed = true;
  void flushAutosave();
  clearTimeout(autosaveTimer);
  document.removeEventListener("visibilitychange", onVisibilityChange);
  window.removeEventListener("beforeunload", onBeforeUnload);
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
    ? `GitHub - ${n} file${n === 1 ? "" : "s"} changed and not yet pushed`
    : "Push changes or create a pull request";
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
            title="Open a file from the KB tab"
            aria-label="Open a file from the KB tab"
            @click="navigate('kb')"
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
            type="button"
            title="New file (use Save As to choose a filename)"
            aria-label="New file"
            @click="onCreate"
          >
            <svg
              viewBox="0 0 16 16"
              width="16"
              height="16"
              fill="currentColor"
              aria-hidden="true"
            >
              <path
                d="M7.25 2a.75.75 0 0 1 1.5 0v5.25H14a.75.75 0 0 1 0 1.5H8.75V14a.75.75 0 0 1-1.5 0V8.75H2a.75.75 0 0 1 0-1.5h5.25V2Z"
              />
            </svg>
          </button>
          <button
            type="button"
            title="Save As a new local file"
            aria-label="Save As"
            :disabled="saving"
            @click="onSaveAs"
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
              <path
                d="M8.5 13.25H2.75V2.75h8.5l2 2V8M5 2.75v3.5h5v-3.5M5 13.25v-4h3"
              />
              <path d="M12 9.5v5M9.5 12h5" />
            </svg>
          </button>
          <button
            type="button"
            :title="
              current && current.origin.kind !== 'file'
                ? 'Rename locally (the original source stays unchanged)'
                : 'Rename this file'
            "
            aria-label="Rename"
            :disabled="saving || !current"
            @click="onRename"
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
              <path
                d="m10.5 2.5 3 3M2 14l3.5-1 8-8a2.12 2.12 0 0 0-3-3l-8 8L2 14Z"
              />
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
            title="Download..."
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
            aria-label="Push changes or create a pull request"
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
            v-show="!editingTest"
            ref="splitBtn"
            type="button"
            :aria-pressed="tptpOpen"
            aria-haspopup="menu"
            :aria-expanded="splitMenuOpen"
            title="Split-screen views..."
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
          <button
            type="button"
            title="File info: KB footprint and edit history"
            aria-label="File info: KB footprint and edit history"
            @click="infoOpen = true"
          >
            &#9432;
          </button>
        </div>
        <span
          class="file-name"
          :title="
            dirty
              ? 'Unsaved changes; autosaves after 5 seconds of inactivity'
              : 'Changes save locally after 5 seconds of inactivity'
          "
          >{{ fileLabel }}</span
        >
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
      <!-- Local save status and errors stay visible under the toolbar. -->
      <StatusLine :text="saveStatus.text" :error="saveStatus.error" />

      <!-- Push selected changes or propose them upstream. -->
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

      <FileInfoDialog v-model="infoOpen" :current="current" />
    </Card>

    <Card class="edit-pane" :class="{ split: tptpOpen }">
      <div class="editor-container">
        <div class="editor-tabs" role="tablist" aria-label="Open files">
          <div
            v-for="tab in tabs"
            :key="tab.id"
            class="editor-file-tab"
            :class="{
              active: tab.id === activeTab,
              dragging: tab.id === draggedTab,
            }"
            draggable="true"
            @dragstart="startTabDrag($event, tab.id)"
            @dragend="draggedTab = null"
            @dragover.prevent
            @drop.prevent="dropTab(tab.id)"
          >
            <button
              type="button"
              role="tab"
              :aria-selected="tab.id === activeTab"
              :title="tab.file?.name ?? 'New file (unsaved)'"
              @click="selectTab(tab)"
            >
              <span class="tab-name">{{
                tab.file?.name ?? "New file (unsaved)"
              }}</span>
              <span
                v-if="
                  tab.id === activeTab
                    ? dirty || (!current && text !== NEW_FILE_TEXT)
                    : !tab.file && tab.text !== NEW_FILE_TEXT
                "
                >*</span
              >
            </button>
            <button
              type="button"
              class="close-tab"
              :aria-label="'Close ' + (tab.file?.name ?? 'new file')"
              title="Close tab"
              @click.stop="closeTab(tab.id)"
            >
              &times;
            </button>
          </div>
        </div>
        <div v-if="!tabs.length" class="editor-empty">
          Open a file or create a new file to start editing.
        </div>
        <MonacoEditor
          v-show="tabs.length"
          ref="ed"
          v-model="text"
          :document-key="String(activeTab)"
          :language="language"
          :lsp="!editingTest"
          placeholder="Loading editor..."
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
        This file ({{ language === "tptp" ? "TPTP" : "KIF" }})
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
/* Joined icon button group (open - save as - download - GitHub). */
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
  overflow: visible;
}
.editor-tabs {
  display: flex;
  flex-shrink: 0;
  overflow-x: auto;
  border-bottom: 1px solid var(--line);
}
.editor-file-tab {
  display: flex;
  flex-shrink: 0;
  max-width: 220px;
  min-width: 0;
  border-right: 1px solid var(--line);
  border-bottom: 2px solid transparent;
}
.editor-file-tab.active {
  border-bottom-color: var(--accent);
  background: var(--card);
}
.editor-file-tab.dragging {
  opacity: 0.5;
}
.editor-file-tab button {
  border: 0;
  border-radius: 0;
  background: transparent;
  color: var(--muted);
  padding: 8px 10px;
}
.editor-file-tab button[role="tab"] {
  display: flex;
  align-items: center;
  gap: 4px;
  min-width: 0;
}
.tab-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.editor-file-tab.active button {
  color: var(--fg);
}
.editor-file-tab .close-tab {
  flex-shrink: 0;
  padding: 8px;
}
.editor-file-tab button:hover {
  background: var(--line);
}
.editor-empty {
  padding: 24px;
  color: var(--muted);
}
.editor-container :deep(.monaco-host) {
  flex: 1;
  min-height: 0;
}
.editor-container {
  display: flex;
  flex-direction: column;
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
