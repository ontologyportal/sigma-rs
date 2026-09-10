<script setup>
import { ref, computed, nextTick, watch } from 'vue';
import { call } from '../../../../rpc.ts';
import {
  proverConfig, vampireSelected, proofLang, plainProofFlag,
  backend, settingsOpen, toggleProverSettings, vampireArgs,
} from '../../../../prover-config.ts';
import { wireProofGraph, proofGraphLegendHtml } from '../../../../proof-graph.ts';
import { renderProofBody, proseDetails } from '../../../../proof-view.ts';

const auditTimeSecs = ref(15);
const auditLimitVal = ref(5);
const auditing = ref(false);
const errorMsg = ref('');

// The raw result + backend label, cached so the proof-language/plain-proof
// toggles can re-render in place without re-running the audit.
const lastResult = ref(null);
const lastBackend = ref('');

const badgeClass = computed(() => `audit-status ${lastResult.value?.status || ''}`);
const backendText = computed(() => lastBackend.value ? `via ${lastBackend.value}` : '');
const stepsText = computed(() => {
  const r = lastResult.value;
  return r?.given_steps != null ? `${r.given_steps} given-clause steps` : '';
});
const verdict = computed(() => {
  const r = lastResult.value;
  if (!r) return '';
  if (r.status === 'Consistent') return 'No contradiction found — the loaded KB saturated cleanly.';
  // Vampire's one-shot run yields at most a single contradiction (no
  // enumerator, unlike the native audit's driver) — this covers the case
  // where even that single witness failed to parse; say so without
  // implying "zero".
  if (r.inconsistent && !r.contradictions.length) return 'Contradiction found — see raw engine output for the derivation.';
  if (r.inconsistent) return `${r.contradictions.length} distinct contradiction${r.contradictions.length === 1 ? '' : 's'} found.`;
  return 'No contradiction found within budget — inconclusive (raise the time limit and try again).';
});
const rawOutput = computed(() => lastResult.value?.raw_output || '(none)');

const cards = computed(() => (lastResult.value?.contradictions || []).map((c, i) => ({
  heading: `Contradiction #${i + 1} — ${c.steps.length} step${c.steps.length === 1 ? '' : 's'}`,
  proofHtml: renderProofBody(c.steps, c.proof_tptp_prologue, proofLang.value, plainProofFlag.value),
  proseHtml: proseDetails(c.prose, c.prose_missing),
  graphviz: c.graphviz || '(none)',
  steps: c.steps,
})));
const legendHtml = proofGraphLegendHtml();

let detailsEls = [];
let containerEls = [];

// Rebuilt fresh on every render (a new audit run, or the same result
// re-rendered for a language/plain-proof change) -- mirrors the old
// document.querySelectorAll pass over freshly templated markup.
watch(cards, async () => {
  await nextTick();
  cards.value.forEach((c, i) => {
    const details = detailsEls[i];
    const container = containerEls[i];
    if (details && container) wireProofGraph(details, container, () => c.steps);
  });
});

async function runAudit() {
  const vampire = vampireSelected();
  auditing.value = true;
  errorMsg.value = '';
  try {
    // Audit inherits the Ask/Tell prover settings (including backend, via
    // the shared #proverSettings panel both tabs toggle), but keeps its own
    // time limit.
    const { result } = vampire
      ? await call('auditVampire', {
          timeLimitSecs: auditTimeSecs.value,
          extraArgs: vampireArgs.value.trim(),
        })
      : await call('audit', {
          config: proverConfig({ timeLimitSecs: auditTimeSecs.value }),
          limit: Math.max(1, Number(auditLimitVal.value) || 5),
        });
    lastResult.value = result;
    lastBackend.value = vampire ? 'Vampire' : 'SUPr';
  } catch (e) {
    errorMsg.value = String(e && e.message || e);
  } finally {
    auditing.value = false;
  }
}
</script>

<template>
<div class="card">
  <div class="inline" style="justify-content: space-between">
    <p class="hint" style="margin: 0">
      Saturates the loaded KB looking for a logical contradiction and
      prints the proofs for said contradictions.
    </p>
    <div class="inline" style="gap: 10px">
      <div style="width: 110px">
        <label for="auditTime">time limit (s)</label
        ><input type="number" id="auditTime" v-model.number="auditTimeSecs" min="0" />
      </div>
      <div style="width: 110px" v-if="!vampireSelected()">
        <label for="auditLimit">max found</label
        ><input type="number" id="auditLimit" v-model.number="auditLimitVal" min="1" />
      </div>
      <button class="btn" id="runAudit" type="button" :disabled="auditing" @click="runAudit">{{ auditing ? 'Auditing…' : 'Run audit' }}</button>
      <!-- Toggles the SAME #proverSettings panel Ask/Tell uses (see its
         own cog button) — one shared component, not a copy, so
         backend/knobs can't drift out of sync between the two tabs. -->
      <button
        class="cog"
        id="auditSettingsBtn"
        type="button"
        title="Prover settings"
        aria-label="Prover settings"
        :aria-expanded="settingsOpen"
        aria-controls="proverSettings"
        @click="toggleProverSettings()"
      >
        ⚙
      </button>
    </div>
  </div>
</div>
<div id="auditResult">
  <div v-if="errorMsg" class="card hint" style="color:var(--bad)">{{ errorMsg }}</div>
  <template v-else-if="lastResult">
    <div class="card">
      <div class="inline" style="gap:10px">
        <span :class="badgeClass">{{ lastResult.status }}</span>
        <span class="hint">{{ backendText }}</span>
        <span class="hint">{{ stepsText }}</span>
      </div>
      <div class="hint" style="margin-top:8px">{{ verdict }}</div>
      <details style="margin-top:10px"><summary class="hint">raw engine output</summary><pre>{{ rawOutput }}</pre></details>
    </div>
    <div class="card" v-for="(c, i) in cards" :key="i">
      <div class="contradiction-hd">{{ c.heading }}</div>
      <ol class="refs" v-html="c.proofHtml"></ol>
      <div v-html="c.proseHtml"></div>
      <details class="proof-graph-details" style="margin-top:10px" :ref="el => detailsEls[i] = el">
        <summary class="hint">proof graph</summary>
        <div class="graph-container" :ref="el => containerEls[i] = el"></div>
        <div class="hint graph-tip"></div>
        <div v-html="legendHtml"></div>
        <details class="graph-dot-toggle"><summary>graphviz (DOT) source</summary><pre>{{ c.graphviz }}</pre></details>
      </details>
    </div>
  </template>
</div>
</template>
