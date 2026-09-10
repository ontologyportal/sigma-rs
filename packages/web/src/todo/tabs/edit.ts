/** Edit tab state: the in-browser IDE (Monaco) for KIF constituents — file
 *  picker, live validation, save, and the fullscreen toggle.
 *
 * Reactive state (not DOM ids) so `EditTab.vue` can bind to it directly;
 * `populateEditPicker`/`onEditPickerChange`/`isBufferDirty`/`ensureEditorReady`/
 * `setEditFullscreen` stay the entry points `router.ts` and `contribute.ts`
 * call into. The editor container itself is still addressed by id (`$`) from
 * inside functions, same as the other Monaco integrations (Ask/Tell's panes,
 * the diff dialog below) -- Monaco needs a raw DOM node to mount into, not a
 * Vue-templated one.
 *
 * The open-file dialog, the format button, and the download menu are pure UI
 * triggers nothing outside `EditTab.vue` calls, so they live in the
 * component directly instead of here. */

import { nextTick, ref } from 'vue';
import { state } from '../state.ts';
import { call } from '../rpc.ts';
import { $, esc, isDarkTheme } from '../dom.ts';
import { loadMonaco, diagsToMarkers } from '../editor/monaco.ts';
import { lspSyncDocument } from '../editor/lsp-client.ts';
import { refreshTptpPane, scheduleTptpRefresh, scheduleTptpFollow } from '../editor/tptp-pane.ts';
import { updateConstituentText } from '../kb.ts';
import { checkStaleOnOpen } from './contribute.ts';
import { navigate } from '../router.ts';

let editValidateTimer = null;
let editDiagnostics = [];

export const editDiagSummary = ref('No errors or warnings');
export const editDiagItems = ref<{ i: number; sev: string; loc: string; message: string; kind: string; code: string }[]>([]);

/** Render only actionable errors/warnings for the buffer currently in Edit.
 * Monaco still receives all severities as inline markers. */
function renderEditDiagnostics(diags) {
  editDiagnostics = diags.filter((d) => d.severity === 'error' || d.severity === 'warning');
  const errors = editDiagnostics.filter((d) => d.severity === 'error').length;
  const warnings = editDiagnostics.length - errors;
  const parts = [];
  if (errors) parts.push(`${errors} error${errors === 1 ? '' : 's'}`);
  if (warnings) parts.push(`${warnings} warning${warnings === 1 ? '' : 's'}`);
  editDiagSummary.value = parts.length ? parts.join(', ') : 'No errors or warnings';
  editDiagItems.value = editDiagnostics.map((d, i) => ({
    i,
    sev: d.severity,
    loc: `${state.editCurrentFile?.name || 'untitled'}:${Math.max(1, d.line || 1)}:${Math.max(1, d.col || 1)}`,
    message: d.message,
    kind: d.kind,
    code: d.code,
  }));
}

export function onEditDiagClick(i: number) {
  if (!state.monacoEditor) return;
  const d = editDiagnostics[i];
  if (!d) return;
  const lineNumber = Math.max(1, d.line || 1);
  const column = Math.max(1, d.col || 1);
  state.monacoEditor.setPosition({ lineNumber, column });
  state.monacoEditor.revealPositionInCenter({ lineNumber, column });
  state.monacoEditor.focus();
}

// One console line per LANE CHANGE (not per keystroke): tells apart "the LSP
// lane engaged for file X" from "this buffer has no backing constituent, so
// only parse checking runs" — the two look identical in the UI when the LSP
// lane fails to engage.
let lastValidateLane = '';
function logValidateLane(lane) {
  if (lane === lastValidateLane) return;
  lastValidateLane = lane;
  console.info('[edit] validate lane:', lane);
}

function scheduleEditValidate() {
  clearTimeout(editValidateTimer);
  editValidateTimer = setTimeout(() => { updateEditFileLabel(); runEditValidate(); }, 400);
}

// Coalesce validate requests: whole-file validation of a large constituent
// takes a couple of seconds in the worker, so at most one runs at a time and
// at most one rerun is queued behind it — typing never piles up a backlog.
let editValidateBusy = false;
let editValidateQueued = false;

async function runEditValidate() {
  if (!state.monacoEditor) return;
  if (editValidateBusy) { editValidateQueued = true; return; }
  editValidateBusy = true;
  try {
    await runEditValidateNow();
  } finally {
    editValidateBusy = false;
    if (editValidateQueued) { editValidateQueued = false; runEditValidate(); }
  }
}

export const editStatusHtml = ref('');

async function runEditValidateNow() {
  const editor = state.monacoEditor;
  if (!editor) return;
  const text = editor.getValue();
  const model = editor.getModel();
  const version = model?.getVersionId();
  const validatingFile = state.editCurrentFile
    ? `${state.editCurrentFile.name}|${state.editCurrentFile.origin}` : '';
  // A buffer belonging to a loaded constituent is diffed into the live KB and
  // validated against it, so semantic diagnostics resolve. A scratch buffer has
  // no backing file, so it falls back to parse-only checking in a throwaway KB.
  const known = state.editCurrentFile
    && state.constituents.find((c) => c.name === state.editCurrentFile.name && c.origin === state.editCurrentFile.origin);
  logValidateLane(known ? `lsp (${known.name})` : 'scratch (parse-only)');
  let diags = [];
  try {
    diags = known
      ? await lspSyncDocument(known.name, text)
      : (await call('validateFormula', { kif: text })).diagnostics;
  } catch (e) { editStatusHtml.value = 'validate failed: ' + (e && e.message || e); return; }
  if (!state.monacoEditor || state.monacoEditor.getModel() !== model
      || model?.getVersionId() !== version
      || validatingFile !== (state.editCurrentFile ? `${state.editCurrentFile.name}|${state.editCurrentFile.origin}` : '')) return;
  state.monaco.editor.setModelMarkers(model, 'sigma', diagsToMarkers(diags));
  renderEditDiagnostics(diags);
  const errs = diags.filter((d) => d.severity === 'error').length;
  if (!diags.length) { editStatusHtml.value = 'no diagnostics'; return; }
  // Link the count into the Diagnostics tab, filtered to this file and landing
  // on the diagnostic nearest the first problem in the buffer.
  const label = `${diags.length} diagnostic${diags.length === 1 ? '' : 's'}` +
    (errs ? ` (${errs} error${errs === 1 ? '' : 's'})` : '');
  const file = state.editCurrentFile ? state.editCurrentFile.name : '';
  const line = diags[0]?.line || 0;
  editStatusHtml.value = file
    ? `<a class="jump-diag" data-file="${esc(file)}" data-line="${line}"
         title="Show these in the Diagnostics tab">${esc(label)}</a>`
    : esc(label);
  scheduleTptpRefresh();
}

function setEditorContent(text) {
  if (!state.monacoEditor) return;
  state.monacoEditor.setValue(text);
  runEditValidate();
}

export const editPickerOptions = ref<{ value: string; label: string }[]>([{ value: '__new__', label: '+ New file…' }]);

/** Populate the file picker from the currently loaded constituents, preserving the selection when possible. */
export async function populateEditPicker() {
  const sel = $('editPicker');
  if (!sel) return;
  const current = sel.value;
  editPickerOptions.value = [
    { value: '__new__', label: '+ New file…' },
    ...state.constituents.map((c) => ({ value: `${c.name}|${c.origin}`, label: c.name })),
  ];
  await nextTick(); // let Vue patch the <option>s before selecting one
  sel.value = editPickerOptions.value.some((o) => o.value === current) ? current : '__new__';
}

/**
 * Save persists wherever the buffer came from and is offered for both writable
 * origins — a `file` upload to OPFS, a `sumo` file to the edit store, where it
 * stays local until it is pushed. A `url` buffer has nowhere to be saved.
 *
 * The GitHub button is always available: it lists every tracked change across
 * the whole KB now, not just an action on the open file.
 */
export const editSaveHidden = ref(false);
export const editLogText = ref('');
export const editLogIsError = ref(false);
export const editSaveStatusText = ref('');
export const editSaveStatusIsError = ref(false);

function updateEditActions() {
  const origin = state.editCurrentFile ? state.editCurrentFile.origin : 'file';  // unsaved new file is local
  editSaveHidden.value = origin === 'url';
  editLogIsError.value = false;   // clear any prior error styling
  editLogText.value = origin === 'url'
    ? 'Loaded from a URL — it can be edited and downloaded here, but not saved or submitted.'
    : '';
  // A save result from whatever file was open before must not linger once
  // the user has switched to a different one.
  editSaveStatusIsError.value = false;
  editSaveStatusText.value = '';

  // The TPTP pane resolves cursor positions against a real loaded file — an
  // unsaved new buffer has nothing in the KB to resolve against yet.
  refreshTptpPane();   // no-op unless the pane is actually open
}

export function onEditPickerChange() {
  const val = $('editPicker').value;
  if (val === '__new__') {
    state.editCurrentFile = null;
    setEditorContent('; New KIF file\n');
    updateEditActions();
    updateEditFileLabel();
    return;
  }
  const sep = val.indexOf('|');
  const name = val.slice(0, sep), origin = val.slice(sep + 1);
  const c = state.constituents.find((x) => x.name === name && x.origin === origin);
  state.editCurrentFile = c ? { name: c.name, origin: c.origin } : null;
  setEditorContent(c ? c.text : '');
  updateEditActions();
  updateEditFileLabel();
  checkStaleOnOpen(state.editCurrentFile);
}

/** True when the buffer holds edits that have not been saved. What a pull
 *  request carries is the SAVED text, so this is also what stops the Contribute
 *  panel from pushing a version the user has already moved past. */
export function isBufferDirty() {
  const f = state.editCurrentFile;
  if (!f || !state.monacoEditor) return false;
  const c = state.constituents.find((x) => x.name === f.name && x.origin === f.origin);
  return Boolean(c) && c.text !== state.monacoEditor.getValue();
}

/** The filename shown beside the toolbar button group, marked when the buffer
 *  has moved ahead of what is saved. */
export const editFileNameText = ref('');
export const editFileNameTitle = ref('');

function updateEditFileLabel() {
  const name = state.editCurrentFile ? state.editCurrentFile.name : 'new file (unsaved)';
  const dirty = isBufferDirty();
  editFileNameText.value = dirty ? `${name} •` : name;
  editFileNameTitle.value = dirty ? 'Unsaved changes' : '';
}

// Memoized: a fast tab switch (or router.ts's `applyRoute`/`afterEach` both
// landing on Edit in quick succession) could otherwise have two concurrent
// callers each get past the `monacoEditor` guard (it is only set at the very
// end, after an await) and build a SECOND editor. The loser's
// `onEditPickerChange()` would then reset the buffer, clobbering any cursor
// position a deep link had just set.
let editorReadyPromise = null;
export function ensureEditorReady() {
  if (!editorReadyPromise) {
    editorReadyPromise = createEditor().catch((e) => {
      editorReadyPromise = null;   // let a later visit retry
      throw e;
    });
  }
  return editorReadyPromise;
}

async function createEditor() {
  if (state.monacoEditor) return;
  const container = $('editorContainer');
  let m;
  try {
    m = await loadMonaco();
  } catch (e) {
    container.dataset.placeholder = 'Failed to load the editor: ' + (e && e.message || e);
    return;
  }
  const dark = isDarkTheme();
  const editor = m.editor.create(container, {
    value: '',
    language: 'kif',
    theme: dark ? 'kif-dark' : 'kif-light',
    automaticLayout: true,
    minimap: { enabled: false },
    // Otherwise a hover diagnostic near the editor's edge gets clipped by the
    // container's own border/overflow instead of floating over it (#21).
    fixedOverflowWidgets: true,
    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
    fontSize: 13,
    // Only the KB's own symbols (via lspCompletionProvider) should be
    // suggested, not arbitrary strings already typed in the buffer.
    wordBasedSuggestions: 'off',
    // The default ('configuredByTheme') would silently no-op unless a
    // theme opts in; force it on so the LSP's KB-aware semantic tokens
    // (see `lspSemanticTokensProvider`) actually render.
    'semanticHighlighting.enabled': true,
  });
  state.monacoEditor = editor;
  (window as any).__sigmaEditor = editor; // debug handle (automation/devtools)
  editor.onDidChangeModelContent(scheduleEditValidate);
  editor.onDidChangeCursorPosition(scheduleTptpFollow);

  // Right-click a symbol → its man page. Monaco does not reliably move the
  // caret on right-click, so the click's own position is captured and used in
  // preference to the cursor.
  let ctxPos = null;
  editor.onContextMenu((e) => { ctxPos = e.target?.position ?? null; });
  editor.addAction({
    id: 'sumo.open-documentation',
    label: 'Open SUMO documentation',
    contextMenuGroupId: 'navigation',
    contextMenuOrder: 0,
    run: (ed) => {
      const model = ed.getModel();
      const pos = ctxPos || ed.getPosition();
      ctxPos = null;
      const word = model && pos && model.getWordAtPosition(pos);
      if (!word) return;
      // `?x` / `@row` are KIF variables, not terms — nothing to document.
      const prev = word.startColumn > 1
        ? model.getValueInRange({
            startLineNumber: pos.lineNumber, startColumn: word.startColumn - 1,
            endLineNumber: pos.lineNumber,   endColumn: word.startColumn })
        : '';
      if (prev === '?' || prev === '@') return;
      navigate('browse', { sym: word.word });
    },
  });

  await populateEditPicker();
  onEditPickerChange();
}

// -- Edit: fullscreen toggle ---------------------------------------------------
//
// Lifts #tab-edit (toolbar + editor + editLog) to cover the viewport via a
// body-level class (body.edit-fullscreen), rather than moving/re-parenting
// any DOM — the CSS alone describes the two end states. The View Transitions
// API (where supported) morphs between them automatically; browsers without
// it just get an instant toggle, still fully functional.

export const editFullscreenOn = ref(false);
export const editFullscreenTitle = ref('Toggle fullscreen editor');

// Two icon variants for the one button: outward corner-brackets to enter,
// inward ones to exit — the same visual language most fullscreen toggles use.
export const EDIT_FULLSCREEN_ENTER_PATH =
  'M2 5.5V2.75A.75.75 0 0 1 2.75 2h2.75M2 10.5v2.75c0 .414.336.75.75.75h2.75' +
  'M14 5.5V2.75a.75.75 0 0 0-.75-.75h-2.75M14 10.5v2.75a.75.75 0 0 1-.75.75h-2.75';
export const EDIT_FULLSCREEN_EXIT_PATH =
  'M5.5 2v2.75a.75.75 0 0 1-.75.75H2M10.5 2v2.75c0 .414.336.75.75.75H14' +
  'M5.5 14v-2.75a.75.75 0 0 0-.75-.75H2M10.5 14v-2.75c0-.414.336-.75.75-.75H14';
export const editFullscreenIconPath = ref(EDIT_FULLSCREEN_ENTER_PATH);

export function setEditFullscreen(on: boolean) {
  if (on === editFullscreenOn.value) return;
  const apply = () => {
    editFullscreenOn.value = on;
    document.body.classList.toggle('edit-fullscreen', on);
    editFullscreenTitle.value = on ? 'Exit fullscreen editor' : 'Toggle fullscreen editor';
    editFullscreenIconPath.value = on ? EDIT_FULLSCREEN_EXIT_PATH : EDIT_FULLSCREEN_ENTER_PATH;
    // The container's size just changed outside of any window resize, which
    // is the one case automaticLayout's own ResizeObserver can lag behind —
    // an explicit layout() is cheap insurance against a stale-sized canvas.
    state.monacoEditor?.layout();
  };
  // Snapshot-based morph between the two states; falls back to an instant
  // toggle wherever unsupported (Firefox, Safari as of this writing) — still
  // fully correct, just not animated.
  if (document.startViewTransition) document.startViewTransition(apply);
  else apply();
}

export function onEditFullscreenClick() {
  setEditFullscreen(!editFullscreenOn.value);
}

document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && editFullscreenOn.value) setEditFullscreen(false);
});

// -- Save ----------------------------------------------------------------------

export const editSaveBusy = ref(false);

export async function onEditSaveClick() {
  const setStatus = (text, bad) => { editSaveStatusIsError.value = bad; editSaveStatusText.value = text; };

  if (!state.monacoEditor) return;
  let name, origin;
  if (state.editCurrentFile) {
    ({ name, origin } = state.editCurrentFile);
  } else {
    // A file created via "+ New file" has no name yet — ask for one now,
    // rather than expecting it to have been typed somewhere earlier (there
    // is nowhere left to type it in advance; the Open dialog no longer asks).
    const entered = window.prompt('Filename for this new file:', 'untitled.kif');
    if (entered === null) return;   // cancelled — leave the buffer as-is, no status change
    name = entered.trim();
    if (!name) { setStatus('Enter a filename to save.', true); return; }
    origin = 'file';
  }

  editSaveBusy.value = true;   // icon-only button: disable, don't swap the label
  try {
    const r = await updateConstituentText(name, state.monacoEditor.getValue(), origin);
    state.editCurrentFile = { name, origin };
    await populateEditPicker();
    $('editPicker').value = `${name}|${origin}`;
    updateEditActions();
    updateEditFileLabel();
    runEditValidate();
    const saved = origin === 'sumo'
      ? `Saved ${name} locally — it stays here until you push it to GitHub.`
      : `Saved ${name}.`;
    setStatus(r.notices.length ? r.notices.join(' | ') : saved, false);
  } catch (e) {
    setStatus(String(e && e.message || e), true);
  } finally {
    editSaveBusy.value = false;
  }
}
