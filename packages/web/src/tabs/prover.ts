/**
 * Ask/Tell tab: the two Monaco input panes, the prove run, and the proof view.
 *
 * The prover inputs are compact Monaco editors sharing the Edit tab's kif
 * language, theme, and marker pipeline. The textareas in the markup remain the
 * working fallback if the Monaco CDN is unreachable; paneValue() reads from
 * whichever is active.
 */

import { state } from '../state.ts';
import { call } from '../rpc.ts';
import { $, downloadText, isDarkTheme } from '../dom.ts';
import { loadMonaco, diagsToMarkers } from '../editor/monaco.ts';
import { proverConfig, vampireSelected, proofLanguage, plainProof, useSumo } from '../prover-config.ts';
import { wireProofGraph } from '../proof-graph.ts';
import { renderProofBody, proseDetails } from '../proof-view.ts';

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

async function runScratchValidate() {
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

// -- Prover: tell + ask -------------------------------------------------------
//
// "Open test" / "Load test" / "Save test" (the .kif.tq / .p / .tptp import
// and save-back workflow) are wired in tests.ts, which owns the imported-test
// collection and its OPFS/localStorage persistence; this module only exports
// paneValue() for it to read the current panes.

const ASSERTIONS_LABEL_KIF_HTML = 'Assertions — <code>tell</code> (added to the KB for this query)';
const ASSERTIONS_LABEL_TPTP_HTML = 'TPTP problem — axioms + an embedded <code>conjecture</code>, read as one document';

/** Show the KIF two-pane split (assertions + query) or, in TPTP mode, a
 *  single pane holding the whole problem -- a TPTP problem is naturally one
 *  role-tagged document, not two independently composed pieces (its
 *  conjecture is embedded by role, not a separate box). Also switches the
 *  assertions pane's Monaco language (kif/tptp, for syntax highlighting) and
 *  height (TPTP problems run longer than a handful of assertions) and shows
 *  the "Use SUMO" toggle only where it applies. Called on load and whenever
 *  the proof-language select changes. */
function applyProofLanguageToPanes() {
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
applyProofLanguageToPanes();

// The exact TPTP problem text handed to Vampire for the most recent Ask/Tell
// run — `proveVampire` returns it alongside the result (computed anyway to
// run the query, previously discarded); `downloadVampireTptp` below just
// hands back what's already in memory, no extra worker round-trip.
let lastVampireTptp = null;

$('prove').onclick = async () => {
  const btn = $('prove');
  const vampire = vampireSelected();
  btn.disabled = true; btn.textContent = 'Proving…';
  lastVampireTptp = null;
  $('downloadVampireTptp').hidden = true;
  try {
    let result;
    const tptp = proofLanguage() === 'tptp';
    // TPTP mode reads the single pane as one whole problem (axioms + an
    // embedded conjecture), routed by role rather than split assertions/query
    // boxes -- see applyProofLanguageToPanes. Reuse the existing
    // `parseTptpTest` RPC (built for the test-import workflow) to split that
    // document into KIF text, then run the ordinary KIF prove/proveVampire
    // RPCs against it.
    let assertions = paneValue('assertions').trim();
    let query = tptp ? '' : paneValue('query');
    if (tptp) {
      const { test } = await call('parseTptpTest', {
        name: 'problem',
        text: assertions,
        remap: useSumo(),
      });
      assertions = test.axiomKif;
      query = test.queryKif;
    }
    if (vampire) {
      const res = await call('proveVampire', {
        assertions,
        query,
        timeLimitSecs: proverConfig().timeLimitSecs,
        selectionTolerancePct: proverConfig().selectionTolerancePct,
        extraArgs: $('cfgVampireArgs').value.trim(),
      });
      result = res.result;
      lastVampireTptp = res.tptp;
      $('downloadVampireTptp').hidden = false;
    } else {
      ({ result } = await call('prove', {
        assertions,
        query,
        config: proverConfig(),
        session: 'user-assertions',
      }));
    }
    renderProof(result, vampire ? 'Vampire' : 'SUPr');
  } catch (e) {
    $('proverResult').hidden = false;
    $('pStatus').textContent = 'Error'; $('pStatus').className = 'status InputError';
    $('pBackendBadge').textContent = '';
    $('pSteps').textContent = String(e && e.message || e);
    $('pProof').innerHTML = ''; $('pRaw').textContent = ''; $('pGraphDot').textContent = '';
    $('pProseSlot').innerHTML = '';
    lastAskProof = [];
    invalidateAskGraph();
  } finally {
    btn.disabled = false; btn.textContent = 'Prove';
  }
};

$('downloadVampireTptp').onclick = () => {
  if (!lastVampireTptp) return;
  downloadText('vampire-input.tptp', lastVampireTptp);
};

let lastAskProof = [];
const invalidateAskGraph = wireProofGraph(
  $('pGraphDetails'), $('pGraphContainer'), () => lastAskProof);

// Cached so the proof-language select (see prover-config.ts) can re-render
// the last result in place, without re-running the query.
let lastAskResult = null;
let lastAskBackend = '';

function renderProof(r, backendLabel: string) {
  lastAskResult = r; lastAskBackend = backendLabel;
  $('proverResult').hidden = false;
  $('pStatus').textContent = r.status; $('pStatus').className = 'status ' + r.status;
  $('pBackendBadge').textContent = backendLabel ? `via ${backendLabel}` : '';
  $('pSteps').textContent = r.given_steps != null ? `${r.given_steps} given-clause steps` : '';
  $('pProof').innerHTML = renderProofBody(r.proof, r.proof_tptp_prologue, proofLanguage(), plainProof());
  $('pRaw').textContent = r.raw_output || '(none)';
  $('pGraphDot').textContent = r.graphviz || '(none)';
  $('pProseSlot').innerHTML = proseDetails(r.prose, r.prose_missing);
  lastAskProof = r.proof;
  invalidateAskGraph();
}

$('cfgProofLang').addEventListener('change', () => {
  applyProofLanguageToPanes();
  if (lastAskResult) renderProof(lastAskResult, lastAskBackend);
  runScratchValidate();
});
$('cfgPlainProof').addEventListener('change', () => {
  if (lastAskResult) renderProof(lastAskResult, lastAskBackend);
});
