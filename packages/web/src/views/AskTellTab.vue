<script setup>
import { ref, onMounted, watch } from 'vue';
import { call } from '../../../../rpc.ts';
import { downloadText } from '../../../../dom.ts';
import {
  proverConfig, vampireSelected, proofLang, plainProofFlag, useSumoFlag,
  backend, settingsOpen, toggleProverSettings, cfgSummary, vampireArgs,
} from '../../../../prover-config.ts';
import { wireProofGraph } from '../../../../proof-graph.ts';
import { renderProofBody, proseDetails } from '../../../../proof-view.ts';
import {
  paneValue, ensureProverEditors, setProverPanes,
  applyProofLanguageToPanes, runScratchValidate,
} from '../../../../tabs/prover.ts';

const proving = ref(false);

// The exact TPTP problem text handed to Vampire for the most recent Ask/Tell
// run -- `proveVampire` returns it alongside the result (computed anyway to
// run the query, previously discarded); the download button just hands back
// what's already in memory, no extra worker round-trip.
let lastVampireTptp = null;
const showDownloadTptp = ref(false);

const resultVisible = ref(false);
const pStatus = ref('');
const pStatusClass = ref('status');
const pBackendBadge = ref('');
const pSteps = ref('');
const pProofHtml = ref('');
const pRaw = ref('');
const pGraphDot = ref('');
const pProseHtml = ref('');

// Cached so the proof-language/plain-proof toggles can re-render the last
// result in place, without re-running the query.
let lastAskResult = null;
let lastAskBackend = '';
let lastAskProof = [];

const pGraphDetails = ref(null);
const pGraphContainer = ref(null);
let invalidateAskGraph = () => {};

onMounted(() => {
  ensureProverEditors();
  applyProofLanguageToPanes();
  invalidateAskGraph = wireProofGraph(pGraphDetails.value, pGraphContainer.value, () => lastAskProof);
});

// Backend and pane-language changes each invalidate state owned by the
// *other* concern (a stale download, stale validation markers) that has no
// natural single owner otherwise.
watch(backend, () => { showDownloadTptp.value = false; });
watch(proofLang, () => {
  applyProofLanguageToPanes();
  runScratchValidate();
  if (lastAskResult) renderProof(lastAskResult, lastAskBackend);
});
watch(plainProofFlag, () => {
  if (lastAskResult) renderProof(lastAskResult, lastAskBackend);
});

function renderProof(r, backendLabel) {
  lastAskResult = r; lastAskBackend = backendLabel;
  resultVisible.value = true;
  pStatus.value = r.status;
  pStatusClass.value = 'status ' + r.status;
  pBackendBadge.value = backendLabel ? `via ${backendLabel}` : '';
  pSteps.value = r.given_steps != null ? `${r.given_steps} given-clause steps` : '';
  pProofHtml.value = renderProofBody(r.proof, r.proof_tptp_prologue, proofLang.value, plainProofFlag.value);
  pRaw.value = r.raw_output || '(none)';
  pGraphDot.value = r.graphviz || '(none)';
  pProseHtml.value = proseDetails(r.prose, r.prose_missing);
  lastAskProof = r.proof;
  invalidateAskGraph();
}

async function prove() {
  const vampire = vampireSelected();
  proving.value = true;
  lastVampireTptp = null;
  showDownloadTptp.value = false;
  try {
    let result;
    const tptp = proofLang.value === 'tptp';
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
        remap: useSumoFlag.value,
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
        extraArgs: vampireArgs.value.trim(),
      });
      result = res.result;
      lastVampireTptp = res.tptp;
      showDownloadTptp.value = true;
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
    resultVisible.value = true;
    pStatus.value = 'Error'; pStatusClass.value = 'status InputError';
    pBackendBadge.value = '';
    pSteps.value = String(e && e.message || e);
    pProofHtml.value = ''; pRaw.value = ''; pGraphDot.value = '';
    pProseHtml.value = '';
    lastAskProof = [];
    invalidateAskGraph();
  } finally {
    proving.value = false;
  }
}

function downloadVampireTptp() {
  if (!lastVampireTptp) return;
  downloadText('vampire-input.tptp', lastVampireTptp);
}
</script>

<template>
<div class="card">
  <label for="assertions" id="assertionsLabel"
    >Assertions — <code>tell</code> (added to the KB for this
    query)</label
  >
  <!-- Monaco mounts replace the textareas once the editor loads; the
     textareas stay as the working fallback when the CDN is unreachable.
     In TPTP mode this single pane holds the whole problem (axioms +
     an embedded conjecture) instead of just the assertions -- see
     tabs/prover.ts's applyProofLanguageToPanes -- so the query pane below
     is hidden rather than being a second, artificially split entry;
     a TPTP problem is naturally one role-tagged document read as a
     whole, not composed from separate pieces. -->
  <div id="assertionsEd" class="pane-editor" hidden></div>
  <textarea id="assertions" rows="4" spellcheck="false">
(instance Rex Dog)
(subclass Dog Mammal)</textarea>
  <div id="queryGroup">
    <label for="pquery" style="margin-top: 10px"
      >Query — <code>ask</code></label
    >
    <div id="pqueryEd" class="pane-editor pane-editor-sm" hidden></div>
    <textarea id="pquery" rows="2" spellcheck="false">
(instance Rex Animal)</textarea>
  </div>
  <div id="useSumoGroup" hidden>
    <label class="check"
      ><input type="checkbox" id="cfgUseSumo" v-model="useSumoFlag" /> Use SUMO
      <span class="hint"
        >— prove against all of SUMO as background axioms and decode
        SUMO-mangled symbol names; off proves the file standalone,
        unmangled</span
      ></label
    >
  </div>
  <div
    class="inline"
    style="margin-top: 10px; gap: 8px; align-items: center"
  >
    <button class="btn" id="prove" type="button" :disabled="proving" @click="prove">{{ proving ? 'Proving…' : 'Prove' }}</button>
    <button
      class="btn ghost"
      id="openTestBtn"
      type="button"
      aria-expanded="false"
      aria-controls="testsPanel"
      title="Open a previously imported test"
    >
      Open test
    </button>
    <button
      class="btn ghost"
      id="loadTestBtn"
      type="button"
      title="Import a new .kif.tq or .p/.tptp test file"
    >
      Load test
    </button>
    <input
      type="file"
      id="loadTestFile"
      accept=".tq,.p,.tptp,text/plain"
      hidden
    />
    <button
      class="btn ghost"
      id="saveTestBtn"
      type="button"
      title="Save the current assertions/query as a test"
    >
      Save test
    </button>
    <button
      class="btn ghost"
      id="downloadVampireTptp"
      type="button"
      v-show="showDownloadTptp"
      title="Download the exact TPTP problem text handed to Vampire for the last run"
      @click="downloadVampireTptp"
    >
      Download TPTP input
    </button>
    <button
      class="cog"
      id="proverSettingsBtn"
      type="button"
      title="Prover settings"
      aria-label="Prover settings"
      :aria-expanded="settingsOpen"
      aria-controls="proverSettings"
      @click="toggleProverSettings()"
    >
      ⚙
    </button>
    <span id="proverCfgSummary" class="hint">{{ cfgSummary }}</span>
  </div>
  <div id="openTestHint" class="hint" style="margin-top: 6px"></div>
</div>

<div id="testsPanel" class="card" hidden>
  <label>Tests — <code>.kif.tq</code> / <code>.p</code> / <code>.tptp</code></label>
  <div id="testsEmpty" class="hint">
    No tests imported. Use "Load test" above to import a
    <code>.kif.tq</code>, <code>.p</code>, or <code>.tptp</code>
    file — tests run against the loaded KB instead of joining it.
  </div>
  <ul id="testsList" class="results"></ul>
  <div class="inline" style="margin-top: 8px; align-items: center">
    <button class="btn" id="runAllTests" type="button" hidden>
      Run all
    </button>
    <span id="testsLog" class="hint"></span>
  </div>
</div>

<div id="proverResult" class="card" v-show="resultVisible">
  <div class="inline" style="justify-content: space-between">
    <div>
      <span id="pStatus" :class="pStatusClass">{{ pStatus }}</span>
      <span id="pBackendBadge" class="hint">{{ pBackendBadge }}</span>
      <span id="pSteps" class="hint">{{ pSteps }}</span>
    </div>
    <label class="check"
      ><input type="checkbox" id="cfgPlainProof" v-model="plainProofFlag" /> Plain Proof</label
    >
  </div>
  <ol id="pProof" class="refs" v-html="pProofHtml"></ol>
  <div id="pProseSlot" v-html="pProseHtml"></div>
  <details id="pGraphDetails" ref="pGraphDetails" style="margin-top: 10px">
    <summary class="hint">proof graph</summary>
    <div id="pGraphContainer" ref="pGraphContainer" class="graph-container"></div>
    <div class="pg-legend">
      <span class="pg-legend-item" data-kind="axiom"
        ><span class="pg-legend-swatch"></span>axiom</span
      >
      <span class="pg-legend-item" data-kind="conjecture"
        ><span class="pg-legend-swatch"></span>conjecture</span
      >
      <span class="pg-legend-item" data-kind="lemma"
        ><span class="pg-legend-swatch"></span>derived lemma</span
      >
    </div>
    <details class="graph-dot-toggle">
      <summary>graphviz (DOT) source</summary>
      <pre id="pGraphDot">{{ pGraphDot }}</pre>
    </details>
  </details>
  <details style="margin-top: 10px">
    <summary class="hint">raw engine output</summary>
    <pre id="pRaw">{{ pRaw }}</pre>
  </details>
</div>
</template>
