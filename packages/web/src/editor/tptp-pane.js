/**
 * Edit: split TPTP preview.
 *
 * A second, read-only Monaco pane showing the WHOLE knowledge base's TPTP
 * translation (not just this file — an individual file's translation is
 * meaningless without the rest of the KB's declarations/context), scrolled
 * to follow this file's content as you edit and move the cursor. The pane
 * itself works for any buffer; only cursor-follow needs a real loaded file
 * to resolve positions against (see `tptpPaneUsable`).
 */

import { state } from '../state.js';
import { call } from '../rpc.js';
import { $, isDarkTheme } from '../dom.js';
import { loadMonaco } from './monaco.js';

// Retranslating the whole KB is the heavy half of this feature, so it gets a
// longer debounce than edit-validate's 400ms rather than riding along with it.
const TPTP_REFRESH_DELAY_MS = 1200;

let tptpEditor = null;
let tptpPaneOpen = false;
let tptpDecorationIds = [];
let tptpText = null;

/** Lazily create the read-only TPTP editor the first time the pane opens. */
async function ensureTptpPane() {
  if (tptpEditor) return;
  const m = await loadMonaco();
  const dark = isDarkTheme();
  tptpEditor = m.editor.create($('tptpContainer'), {
    value: '',
    language: 'tptp',
    theme: dark ? 'kif-dark' : 'kif-light',
    readOnly: true,
    automaticLayout: true,
    minimap: { enabled: false },
    lineNumbers: 'on',
    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
    fontSize: 13,
  });
}

/** Whether the current buffer is backed by a real loaded file — cursor-follow
 * needs one to resolve positions against the live KB; the pane itself doesn't. */
function tptpPaneUsable() {
  const f = state.editCurrentFile;
  return !!(f && state.constituents.find((c) => c.name === f.name && c.origin === f.origin));
}

// The toggle is pure show/hide; PLACEMENT is not its concern — the pane sits
// below the editor normally and beside it in fullscreen, driven entirely by
// the body.edit-fullscreen CSS (see index.html).
async function setTptpPaneOpen(on) {
  if (on === tptpPaneOpen) return;
  tptpPaneOpen = on;
  const btn = $('editTptpToggle');
  btn.setAttribute('aria-pressed', String(on));
  $('editPane').classList.toggle('split', on);
  $('tptpPane').hidden = !on;
  state.monacoEditor?.layout();
  if (on) {
    await ensureTptpPane();
    tptpEditor?.layout();
    await refreshTptpPane();
    followTptpCursor();
  }
}

// The split-screen button opens a small menu (same pattern as the download
// menu) rather than toggling directly — its one entry for now is the TPTP
// translation pane; more split views can slot in later.
function setTptpMenuOpen(on) {
  const menu = $('editTptpMenu');
  const btn  = $('editTptpToggle');
  if (on) {
    const r = btn.getBoundingClientRect();
    menu.style.left = `${Math.round(r.left)}px`;
    menu.style.top  = `${Math.round(r.bottom + 4)}px`;
  }
  menu.hidden = !on;
  btn.setAttribute('aria-expanded', String(on));
}

// While a split view is open, the button acts as a plain dismiss — the menu
// only appears when there is something to choose.
$('editTptpToggle').onclick = () => {
  if (tptpPaneOpen) { setTptpPaneOpen(false); return; }
  setTptpMenuOpen($('editTptpMenu').hidden);
};
document.addEventListener('click', (e) => {
  if (e.target instanceof Element && e.target.closest('#editTptpToggle, #editTptpMenu')) return;
  setTptpMenuOpen(false);
});
document.addEventListener('keydown', (e) => { if (e.key === 'Escape') setTptpMenuOpen(false); });

$('tptpTranslationBtn').onclick = () => {
  setTptpMenuOpen(false);
  setTptpPaneOpen(!tptpPaneOpen);
};

/** Retranslate the whole KB into the pane. Only while it is actually open —
 *  every caller can fire unconditionally. */
export async function refreshTptpPane() {
  if (!tptpPaneOpen || !tptpEditor) return;
  $('tptpStatus').textContent = 'generating…';
  try {
    const { text } = await call('toTptpIndexed', {});
    if (!tptpEditor) return;
    // Most edits leave the dump untouched; setValue on a KB-sized model resets
    // its tokenization, folding and scroll position, so skip an identical one.
    if (text !== tptpText) {
      tptpText = text;
      tptpEditor.setValue(text);
    }
    $('tptpStatus').textContent = `${tptpEditor.getModel().getLineCount()} lines`;
    followTptpCursor();
  } catch (e) {
    $('tptpStatus').textContent = 'translation failed: ' + (e && e.message || e);
  }
}

let tptpRefreshTimer = 0;

/** Queue a retranslation. Used by the typing path, which must not wait on it. */
export function scheduleTptpRefresh() {
  if (!tptpPaneOpen) return;
  clearTimeout(tptpRefreshTimer);
  tptpRefreshTimer = setTimeout(refreshTptpPane, TPTP_REFRESH_DELAY_MS);
}

// Cursor-follow: cheap (cache lookups against the last generated dump, no
// retranslation), so it can run on every cursor move with only a light
// debounce — mainly to avoid flooding postMessage while arrow-keying/scrolling.
let tptpFollowTimer = 0;

export function scheduleTptpFollow() {
  clearTimeout(tptpFollowTimer);
  tptpFollowTimer = setTimeout(followTptpCursor, 120);
}

async function followTptpCursor() {
  const editor = state.monacoEditor;
  if (!tptpPaneOpen || !tptpEditor || !editor || !tptpPaneUsable()) return;
  const model = editor.getModel();
  const pos = editor.getPosition();
  if (!model || !pos) return;
  // Rust's offsets are UTF-8 BYTE offsets; Monaco's are UTF-16 code-unit
  // offsets — re-encode the prefix rather than assuming they coincide (true
  // only for pure-ASCII text, which SUMO's documentation strings aren't
  // always).
  const prefix = model.getValueInRange({
    startLineNumber: 1, startColumn: 1,
    endLineNumber: pos.lineNumber, endColumn: pos.column,
  });
  const offset = new TextEncoder().encode(prefix).length;
  let line;
  try {
    ({ line } = await call('tptpLineForPosition', { file: state.editCurrentFile.name, offset }));
  } catch { return; }
  if (line == null || !tptpEditor) return;
  const lineNumber = line + 1; // 0-based from Rust -> Monaco's 1-based
  tptpEditor.revealLineInCenterIfOutsideViewport(lineNumber);
  tptpDecorationIds = tptpEditor.deltaDecorations(tptpDecorationIds, [{
    range: new state.monaco.Range(lineNumber, 1, lineNumber, 1),
    options: { isWholeLine: true, className: 'tptp-follow-line' },
  }]);
}
