<script setup>
import {
  cfg, backend, vampireArgs, backendHint,
  selectionPctLabel, resetProverConfig, proofLang,
} from '../../../../prover-config.ts';

const vampire = () => backend.value === 'vampire';

function reset() {
  resetProverConfig();
}
</script>

<template>
<div class="settings-grid">
  <div>
    <label for="proverBackend">backend</label>
    <select id="proverBackend" v-model="backend">
      <option value="native">SUPr</option>
      <option value="vampire">Vampire</option>
    </select>
    <div class="hint sub" id="proverBackendHint">{{ backendHint }}</div>
  </div>
  <div>
    <label for="cfgProofLang">proof language</label>
    <select id="cfgProofLang" v-model="proofLang">
      <option value="kif">SUO-KIF</option>
      <option value="tptp">TPTP</option>
    </select>
    <div class="hint sub">proof/contradiction display format; on Ask/Tell, TPTP also parses the assertions/query as TPTP</div>
  </div>
  <div>
    <label for="cfgTimeLimit">time limit (s)</label>
    <input type="number" id="cfgTimeLimit" min="0" step="1" v-model.number="cfg.timeLimitSecs" />
    <div class="hint sub">0 = no wall-clock limit</div>
  </div>
  <div v-if="!vampire()">
    <label for="cfgMaxSteps">max steps</label>
    <input type="number" id="cfgMaxSteps" min="0" step="100" v-model.number="cfg.maxSteps" />
    <div class="hint sub">given-clause loop budget</div>
  </div>
  <div v-if="!vampire()">
    <label for="cfgMaxLits">max literals</label>
    <input type="number" id="cfgMaxLits" min="0" step="1" v-model.number="cfg.maxLits" />
    <div class="hint sub">per retained clause</div>
  </div>
  <div>
    <label for="cfgSelectionPct">selection budget</label>
    <input type="range" id="cfgSelectionPct" min="0" max="100" step="1" v-model.number="cfg.selectionTolerancePct" />
    <div class="hint sub" id="cfgSelectionPctVal">{{ selectionPctLabel }}</div>
  </div>
  <div v-if="vampire()">
    <label for="cfgVampireArgs">extra CLI args</label>
    <input type="text" id="cfgVampireArgs" placeholder="e.g. --avatar off" spellcheck="false" v-model="vampireArgs" />
    <div class="hint sub">
      appended to Vampire's command line (advanced)
    </div>
  </div>
</div>
<div class="settings-checks" v-if="!vampire()">
  <label class="check"
    ><input type="checkbox" id="cfgForwardClose" v-model="cfg.forwardClose" /> forward
    closure
    <span class="hint"
      >— saturate the theory before the loop</span
    ></label
  >
  <label class="check"
    ><input type="checkbox" id="cfgWantProof" v-model="cfg.wantProof" /> want proof
    <span class="hint"
      >— populate the proof, graph and prose</span
    ></label
  >
  <label class="check"
    ><input type="checkbox" id="cfgProfile" v-model="cfg.profile" /> profile
    <span class="hint"
      >— emit phase timings into raw output</span
    ></label
  >
</div>
<div class="inline" style="justify-content: flex-end; margin-top: 10px">
  <button class="btn ghost" id="cfgReset" type="button" @click="reset">
    Reset to defaults
  </button>
</div>
</template>
