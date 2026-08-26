/** Edit tab: the in-browser IDE (Monaco) for KIF constituents — file picker,
 *  live validation, save, download menu, and the fullscreen toggle. */

import { state } from '../state.ts';
import { call } from '../rpc.ts';
import { $, esc, escAttr, downloadText, isDarkTheme } from '../dom.ts';
import { loadMonaco, diagsToMarkers } from '../editor/monaco.ts';
import { refreshTptpPane, scheduleTptpRefresh, scheduleTptpFollow } from '../editor/tptp-pane.ts';
import { updateConstituentText } from '../kb.ts';
import { checkStaleOnOpen } from './contribute.ts';
import { navigate, updateParams } from '../router.ts';

let editValidateTimer = null;
let editDiagnostics = [];

/** Render only actionable errors/warnings for the buffer currently in Edit.
 * Monaco still receives all severities as inline markers. */
function renderEditDiagnostics(diags) {
  editDiagnostics = diags.filter((d) => d.severity === 'error' || d.severity === 'warning');
  const errors = editDiagnostics.filter((d) => d.severity === 'error').length;
  const warnings = editDiagnostics.length - errors;
  const parts = [];
  if (errors) parts.push(`${errors} error${errors === 1 ? '' : 's'}`);
  if (warnings) parts.push(`${warnings} warning${warnings === 1 ? '' : 's'}`);
  $('editDiagSummary').textContent = parts.length ? parts.join(', ') : 'No errors or warnings';
  $('editDiagList').innerHTML = editDiagnostics.length
    ? editDiagnostics.map((d, i) => `<button class="edit-diag" type="button" data-i="${i}" data-sev="${esc(d.severity)}">
        <span class="sev ${esc(d.severity)}">${esc(d.severity)}</span>
        <span class="edit-diag-loc">${esc(state.editCurrentFile?.name || 'untitled')}:${Math.max(1, d.line || 1)}:${Math.max(1, d.col || 1)}</span>
        <span class="edit-diag-msg">${esc(d.message)} <span class="edit-diag-code">[${esc(d.kind)}/${esc(d.code)}]</span></span>
      </button>`).join('')
    : '<div id="editDiagEmpty" class="hint">This file has no errors or warnings.</div>';
}

$('editDiagList').addEventListener('click', (e) => {
  const row = e.target.closest('.edit-diag');
  if (!row || !state.monacoEditor) return;
  const d = editDiagnostics[Number(row.dataset.i)];
  if (!d) return;
  const lineNumber = Math.max(1, d.line || 1);
  const column = Math.max(1, d.col || 1);
  state.monacoEditor.setPosition({ lineNumber, column });
  state.monacoEditor.revealPositionInCenter({ lineNumber, column });
  state.monacoEditor.focus();
});

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
  let diags = [];
  try {
    diags = known
      ? (await call('validateBuffer', { file: known.name, text })).diagnostics
      : (await call('validateFormula', { kif: text })).diagnostics;
  } catch (e) { $('editStatus').textContent = 'parse error: ' + (e && e.message || e); return; }
  if (!state.monacoEditor || state.monacoEditor.getModel() !== model
      || model?.getVersionId() !== version
      || validatingFile !== (state.editCurrentFile ? `${state.editCurrentFile.name}|${state.editCurrentFile.origin}` : '')) return;
  state.monaco.editor.setModelMarkers(model, 'sigma', diagsToMarkers(diags));
  renderEditDiagnostics(diags);
  const errs = diags.filter((d) => d.severity === 'error').length;
  if (!diags.length) { $('editStatus').textContent = 'no diagnostics'; return; }
  // Link the count into the Diagnostics tab, filtered to this file and landing
  // on the diagnostic nearest the first problem in the buffer.
  const label = `${diags.length} diagnostic${diags.length === 1 ? '' : 's'}` +
    (errs ? ` (${errs} error${errs === 1 ? '' : 's'})` : '');
  const file = state.editCurrentFile ? state.editCurrentFile.name : '';
  const line = diags[0]?.line || 0;
  $('editStatus').innerHTML = file
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

/** Populate the file picker from the currently loaded constituents, preserving the selection when possible. */
export function populateEditPicker() {
  const sel = $('editPicker');
  if (!sel) return;
  const current = sel.value;
  sel.innerHTML = '<option value="__new__">+ New file…</option>' +
    state.constituents.map((c) => `<option value="${esc(c.name)}|${esc(c.origin)}">${esc(c.name)}</option>`).join('');
  sel.value = [...sel.options].some((o) => o.value === current) ? current : '__new__';
}

/**
 * Save persists wherever the buffer came from and is offered for both writable
 * origins — a `file` upload to OPFS, a `sumo` file to the edit store, where it
 * stays local until it is pushed. A `url` buffer has nowhere to be saved.
 *
 * The GitHub button is always available: it lists every tracked change across
 * the whole KB now, not just an action on the open file.
 */
function updateEditActions() {
  const origin = state.editCurrentFile ? state.editCurrentFile.origin : 'file';  // unsaved new file is local
  $('editSave').hidden = origin === 'url';
  $('editLog').style.color = '';   // clear any prior error styling
  $('editLog').textContent = origin === 'url'
    ? 'Loaded from a URL — it can be edited and downloaded here, but not saved or submitted.'
    : '';
  // A save result from whatever file was open before must not linger once
  // the user has switched to a different one.
  $('editSaveStatus').style.color = '';
  $('editSaveStatus').textContent = '';

  // The TPTP pane resolves cursor positions against a real loaded file — an
  // unsaved new buffer has nothing in the KB to resolve against yet.
  $('editTptpToggle').title = 'Split-screen views…';
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
function updateEditFileLabel() {
  const el = $('editFileName');
  if (!el) return;
  const name = state.editCurrentFile ? state.editCurrentFile.name : 'new file (unsaved)';
  const dirty = isBufferDirty();
  el.textContent = dirty ? `${name} •` : name;
  el.title = dirty ? 'Unsaved changes' : '';
}

// -- Open-file dialog: pick a loaded constituent, or start a new file ---------

$('editOpen').onclick = () => {
  $('openList').innerHTML = state.constituents.length
    ? state.constituents.map((c) =>
        `<li><a class="open-file" data-val="${escAttr(`${c.name}|${c.origin}`)}">${esc(c.name)}</a>
             <span class="hint origin">${esc(c.origin)}</span></li>`).join('')
    : '<li class="hint">no files loaded yet — create one below</li>';
  $('openDialog').showModal();
};

$('openList').addEventListener('click', (e) => {
  const a = e.target.closest('a.open-file');
  if (!a) return;
  $('openDialog').close();
  $('editPicker').value = a.dataset.val;
  onEditPickerChange();
  updateParams(state.editCurrentFile ? { file: state.editCurrentFile.name } : {});
});

$('editCreate').onclick = () => {
  $('openDialog').close();
  $('editPicker').value = '__new__';
  onEditPickerChange();
  updateParams({});
};

$('openCancel').onclick = () => $('openDialog').close();

// Memoized: `showTab('edit')` and `applyRoute()` both ask for the editor, and
// without this the two concurrent calls each get past the `monacoEditor` guard
// (it is only set at the very end, after an await) and build a SECOND editor.
// The loser's `onEditPickerChange()` then resets the buffer, clobbering any
// cursor position a deep link had just set.
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
    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
    fontSize: 13,
    // Only the KB's own symbols (via kifCompletionProvider) should be
    // suggested, not arbitrary strings already typed in the buffer.
    wordBasedSuggestions: 'off',
  });
  state.monacoEditor = editor;
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

  populateEditPicker();
  onEditPickerChange();
}

$('editPicker').addEventListener('change', () => {
  onEditPickerChange();
  updateParams(state.editCurrentFile ? { file: state.editCurrentFile.name } : {});
});

// Delegates to Monaco's own format-document command rather than calling
// formatKif + applying the edit by hand — Monaco's command already runs it
// through the SAME registered provider (defineKifLanguage) and applies the
// result as one undoable edit, preserving cursor/scroll/undo history the way
// a hand-rolled setValue() would not.
$('editFormat').onclick = () => state.monacoEditor?.getAction('editor.action.formatDocument')?.run();

// -- Edit: download menu -------------------------------------------------------
//
// The download button opens a two-entry menu: the current buffer as KIF (a
// real download, independent of the in-browser OPFS/KB state), or the WHOLE
// knowledge base's TPTP translation (same `toTptpIndexed` dump the TPTP pane
// shows — an individual file's translation is meaningless without the rest
// of the KB's declarations, so there is no per-file TPTP option).

function setDownloadMenuOpen(on) {
  const menu = $('editDownloadMenu');
  const btn  = $('editDownload');
  if (on) {
    // Fixed-position under the button (the menu can't live inside
    // .btn-group — overflow:hidden would clip it; see index.html).
    const r = btn.getBoundingClientRect();
    menu.style.left = `${Math.round(r.left)}px`;
    menu.style.top  = `${Math.round(r.bottom + 4)}px`;
  }
  menu.hidden = !on;
  btn.setAttribute('aria-expanded', String(on));
}

$('editDownload').onclick = () => setDownloadMenuOpen($('editDownloadMenu').hidden);
// Close on any click OUTSIDE the button/menu (matching on the event target
// rather than relying on stopPropagation, which breaks under synthesized
// event sequences). The menu-item handlers below close it themselves.
document.addEventListener('click', (e) => {
  if (e.target instanceof Element && e.target.closest('#editDownload, #editDownloadMenu')) return;
  setDownloadMenuOpen(false);
});
document.addEventListener('keydown', (e) => { if (e.key === 'Escape') setDownloadMenuOpen(false); });

$('dlKifBtn').onclick = () => {
  setDownloadMenuOpen(false);
  if (!state.monacoEditor) return;
  downloadText(state.editCurrentFile ? state.editCurrentFile.name : 'untitled.kif', state.monacoEditor.getValue());
};

$('dlTptpBtn').onclick = async () => {
  setDownloadMenuOpen(false);
  const status = $('editStatus');
  const prior = status.textContent;
  status.textContent = 'generating TPTP…';
  try {
    const { text } = await call('toTptpIndexed', {});
    downloadText('knowledge-base.tptp', text);
    status.textContent = prior;
  } catch (e) {
    status.textContent = 'TPTP translation failed: ' + (e && e.message || e);
  }
};

// -- Edit: fullscreen toggle ---------------------------------------------------
//
// Lifts #tab-edit (toolbar + editor + editLog) to cover the viewport via a
// body-level class (body.edit-fullscreen), rather than moving/re-parenting
// any DOM — the CSS alone describes the two end states. The View Transitions
// API (where supported) morphs between them automatically; browsers without
// it just get an instant toggle, still fully functional.

let editFullscreen = false;

// Two icon variants for the one button: outward corner-brackets to enter,
// inward ones to exit — the same visual language most fullscreen toggles use.
const EDIT_FULLSCREEN_ENTER_PATH =
  'M2 5.5V2.75A.75.75 0 0 1 2.75 2h2.75M2 10.5v2.75c0 .414.336.75.75.75h2.75' +
  'M14 5.5V2.75a.75.75 0 0 0-.75-.75h-2.75M14 10.5v2.75a.75.75 0 0 1-.75.75h-2.75';
const EDIT_FULLSCREEN_EXIT_PATH =
  'M5.5 2v2.75a.75.75 0 0 1-.75.75H2M10.5 2v2.75c0 .414.336.75.75.75H14' +
  'M5.5 14v-2.75a.75.75 0 0 0-.75-.75H2M10.5 14v-2.75c0-.414.336-.75.75-.75H14';

export function setEditFullscreen(on) {
  if (on === editFullscreen) return;
  const apply = () => {
    editFullscreen = on;
    document.body.classList.toggle('edit-fullscreen', on);
    const btn = $('editFullscreen');
    btn.setAttribute('aria-pressed', String(on));
    btn.title = on ? 'Exit fullscreen editor' : 'Toggle fullscreen editor';
    btn.querySelector('path').setAttribute('d', on ? EDIT_FULLSCREEN_EXIT_PATH : EDIT_FULLSCREEN_ENTER_PATH);
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

$('editFullscreen').onclick = () => setEditFullscreen(!editFullscreen);

document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && editFullscreen) setEditFullscreen(false);
});

// -- Save ----------------------------------------------------------------------

$('editSave').onclick = async () => {
  const btn = $('editSave');
  const status = $('editSaveStatus');
  const setStatus = (text, bad) => { status.style.color = bad ? 'var(--bad)' : ''; status.textContent = text; };

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

  btn.disabled = true;   // icon-only button: disable, don't swap the label
  try {
    const r = await updateConstituentText(name, state.monacoEditor.getValue(), origin);
    state.editCurrentFile = { name, origin };
    populateEditPicker();
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
    btn.disabled = false;
  }
};
