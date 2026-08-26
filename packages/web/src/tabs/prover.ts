/**
 * Ask/Tell tab: the two Monaco input panes, the prove run, and the proof view.
 *
 * The prover inputs are compact Monaco editors sharing the Edit tab's kif
 * language, theme, and marker pipeline. The textareas in the markup remain the
 * working fallback if the Monaco CDN is unreachable; paneValue() reads from
 * whichever is active.
 */

import { formatTest } from 'sigmakee/sdk';
import { state } from '../state.ts';
import { call } from '../rpc.ts';
import { $, downloadText, isDarkTheme } from '../dom.ts';
import { loadMonaco, diagsToMarkers } from '../editor/monaco.ts';
import { proverConfig, vampireSelected } from '../prover-config.ts';
import { wireProofGraph } from '../proof-graph.ts';
import { renderProofSteps, proseDetails } from '../proof-view.ts';

let assertionsEditor = null;
let queryEditor = null;
let proverEditorsPromise = null;

function paneValue(name) {
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
    // Only the KB's own symbols (via kifCompletionProvider) should be
    // suggested, not arbitrary strings already typed in the buffer.
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

$('saveTq').onclick = () => {
  const query = paneValue('query').trim();
  if (!query) { $('proverCfgSummary').textContent = 'Enter a query first.'; return; }
  downloadText('test.kif.tq', formatTest({
    timeout: proverConfig().timeLimitSecs,
    assertions: paneValue('assertions'),
    query,
    expectedProof: true,
  }));
};

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
    if (vampire) {
      const res = await call('proveVampire', {
        assertions: paneValue('assertions').trim(),
        query: paneValue('query'),
        timeLimitSecs: proverConfig().timeLimitSecs,
        selectionTolerancePct: proverConfig().selectionTolerancePct,
        extraArgs: $('cfgVampireArgs').value.trim(),
      });
      result = res.result;
      lastVampireTptp = res.tptp;
      $('downloadVampireTptp').hidden = false;
    } else {
      ({ result } = await call('prove', {
        assertions: paneValue('assertions').trim(),
        query: paneValue('query'),
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

function renderProof(r, backendLabel: string) {
  $('proverResult').hidden = false;
  $('pStatus').textContent = r.status; $('pStatus').className = 'status ' + r.status;
  $('pBackendBadge').textContent = backendLabel ? `via ${backendLabel}` : '';
  $('pSteps').textContent = r.given_steps != null ? `${r.given_steps} given-clause steps` : '';
  $('pProof').innerHTML = renderProofSteps(r.proof);
  $('pRaw').textContent = r.raw_output || '(none)';
  $('pGraphDot').textContent = r.graphviz || '(none)';
  $('pProseSlot').innerHTML = proseDetails(r.prose, r.prose_missing);
  lastAskProof = r.proof;
  invalidateAskGraph();
}
