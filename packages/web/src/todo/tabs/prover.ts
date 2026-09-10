/**
 * Ask/Tell's assertions/query editors: the two Monaco input panes and the
 * pane-language switch. Monaco is inherently imperative (not a Vue-owned
 * widget), and this state is shared with `tabs/tests.ts`'s "Open test" /
 * "Load test" / "Save test" workflow and `router.ts`'s tab-entry hook -- so
 * it stays a plain module rather than folding into `AskTellTab.vue`'s
 * script, which owns the Prove workflow and result rendering instead.
 *
 * The prover inputs are compact Monaco editors sharing the Edit tab's kif
 * language, theme, and marker pipeline. The textareas in the markup remain the
 * working fallback if the Monaco CDN is unreachable; paneValue() reads from
 * whichever is active.
 */

import { state } from '../state.ts';
import { call } from '../rpc.ts';
import { $, isDarkTheme } from '../dom.ts';
import { loadMonaco, diagsToMarkers } from '../editor/monaco.ts';
import { proofLanguage } from '../prover-config.ts';

let assertionsEditor = null;
let queryEditor = null;
let proverEditorsPromise = null;

/** Current text of the `assertions`/`query` pane -- exported for `tests.ts`'s
 *  "Save test" (kept one-way: tests.ts already depends on this module for
 *  `ensureProverEditors`/`setProverPanes`, so this avoids a back-import). */
export function paneValue(name) {
  if (name === 'assertions') return assertionsEditor ? assertionsEditor.getValue() : $('assertions').value;
  return queryEditor ? queryEditor.getValue() : $('pquery').value;
}

/** Load both panes with the given text, whichever backing widget is live —
 *  used by the .tq tests tab's "open" action. */
export function setProverPanes(assertions, query) {
  if (assertionsEditor) {
    assertionsEditor.setValue(assertions);
    queryEditor.setValue(query);
  } else {
    $('assertions').value = assertions;
    $('pquery').value = query;
  }
}

export function ensureProverEditors() {
  if (!proverEditorsPromise) {
    proverEditorsPromise = createProverEditors().catch((e) => {
      proverEditorsPromise = null;
      throw e;
    });
  }
  return proverEditorsPromise;
}

async function createProverEditors() {
  if (assertionsEditor) return;
  const m = await loadMonaco();
  const dark = isDarkTheme();
  const opts = {
    language: 'kif',
    theme: dark ? 'kif-dark' : 'kif-light',
    automaticLayout: true,
    minimap: { enabled: false },
    lineNumbers: 'off',
    folding: false,
    glyphMargin: false,
    lineDecorationsWidth: 6,
    scrollBeyondLastLine: false,
    overviewRulerLanes: 0,
    hideCursorInOverviewRuler: true,
    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
    fontSize: 13,
    // These scratch panes have no LSP document identity, so
    // `lspCompletionProvider` never serves them (see its own guard) --
    // Monaco's default word-based suggestions (any string already typed in
    // the buffer) would be the only fallback, and that has no notion of
    // what's actually a SUMO term, so it stays off rather than suggesting
    // noise.
    wordBasedSuggestions: 'off',
  };
  const mount = (mountId, taId) => {
    const ta = $(taId);
    const box = $(mountId);
    const ed = m.editor.create(box, { ...opts, value: ta.value });
    ta.hidden = true;
    box.hidden = false;
    ed.onDidChangeModelContent(scheduleScratchValidate);
    return ed;
  };
  assertionsEditor = mount('assertionsEd', 'assertions');
  queryEditor = mount('pqueryEd', 'pquery');
  applyProofLanguageToPanes();
  runScratchValidate();
}

let scratchValidateTimer = 0;
let scratchValidateBusy = false;
let scratchValidateQueued = false;

function scheduleScratchValidate() {
  clearTimeout(scratchValidateTimer);
  scratchValidateTimer = setTimeout(runScratchValidate, 400);
}

export async function runScratchValidate() {
  if (!assertionsEditor) return;
  // `validateScratch` only understands SUO-KIF -- in TPTP mode it would
  // paint the editors with spurious "parse error" squiggles under
  // perfectly valid TPTP text, so just clear any stale markers instead.
  if (proofLanguage() === 'tptp') {
    state.monaco.editor.setModelMarkers(assertionsEditor.getModel(), 'sigma', []);
    state.monaco.editor.setModelMarkers(queryEditor.getModel(), 'sigma', []);
    return;
  }
  if (scratchValidateBusy) { scratchValidateQueued = true; return; }
  scratchValidateBusy = true;
  try {
    const r = await call('validateScratch', {
      assertions: assertionsEditor.getValue(),
      query: queryEditor.getValue(),
    });
    if (!assertionsEditor) return;
    state.monaco.editor.setModelMarkers(assertionsEditor.getModel(), 'sigma', diagsToMarkers(r.assertions));
    state.monaco.editor.setModelMarkers(queryEditor.getModel(), 'sigma', diagsToMarkers(r.query));
  } catch (e) { console.warn('validateScratch:', e.message || e); }
  finally {
    scratchValidateBusy = false;
    if (scratchValidateQueued) { scratchValidateQueued = false; runScratchValidate(); }
  }
}

const ASSERTIONS_LABEL_KIF_HTML = 'Assertions — <code>tell</code> (added to the KB for this query)';
const ASSERTIONS_LABEL_TPTP_HTML = 'TPTP problem — axioms + an embedded <code>conjecture</code>, read as one document';

/** Show the KIF two-pane split (assertions + query) or, in TPTP mode, a
 *  single pane holding the whole problem -- a TPTP problem is naturally one
 *  role-tagged document, not two independently composed pieces (its
 *  conjecture is embedded by role, not a separate box). Also switches the
 *  assertions pane's Monaco language (kif/tptp, for syntax highlighting) and
 *  height (TPTP problems run longer than a handful of assertions) and shows
 *  the "Use SUMO" toggle only where it applies. Called once the editors
 *  exist and whenever `AskTellTab.vue`'s proof-language watcher fires. */
export function applyProofLanguageToPanes() {
  const tptp = proofLanguage() === 'tptp';
  $('queryGroup').hidden = tptp;
  $('useSumoGroup').hidden = !tptp;
  $('assertionsLabel').innerHTML = tptp ? ASSERTIONS_LABEL_TPTP_HTML : ASSERTIONS_LABEL_KIF_HTML;
  $('assertionsEd').classList.toggle('pane-editor-tall', tptp);
  (($('assertions') as HTMLTextAreaElement)).rows = tptp ? 16 : 4;
  if (assertionsEditor) {
    state.monaco.editor.setModelLanguage(assertionsEditor.getModel(), tptp ? 'tptp' : 'kif');
  }
}
