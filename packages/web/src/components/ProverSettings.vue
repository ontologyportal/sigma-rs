<script setup lang="ts">
import { useProverStore } from "../stores/prover";

/** The shared prover settings panel (Ask/Tell + Audit), bound to the prover store. */
const prover = useProverStore();
</script>

<template>
  <div v-show="prover.settingsOpen" class="settings">
    <div class="settings-grid">
      <div>
        <label for="proverBackend">backend</label>
        <select id="proverBackend" v-model="prover.backend">
          <option value="native">SUPr</option>
          <option value="vampire">Vampire</option>
        </select>
        <div class="hint sub">{{ prover.backendHint }}</div>
      </div>
      <div>
        <label for="cfgProofLang">proof language</label>
        <select id="cfgProofLang" v-model="prover.proofLang">
          <option value="kif">SUO-KIF</option>
          <option value="tptp">TPTP</option>
        </select>
        <div class="hint sub">
          proof/contradiction display format; on Ask/Tell, TPTP also parses the
          assertions/query as TPTP
        </div>
      </div>
      <div>
        <label for="cfgTimeLimit">time limit (s)</label>
        <input
          id="cfgTimeLimit"
          type="number"
          min="0"
          step="1"
          v-model.number="prover.cfg.timeLimitSecs"
        />
        <div class="hint sub">0 = no wall-clock limit</div>
      </div>
      <div v-if="!prover.vampireSelected">
        <label for="cfgMaxSteps">max steps</label>
        <input
          id="cfgMaxSteps"
          type="number"
          min="0"
          step="100"
          v-model.number="prover.cfg.maxSteps"
        />
        <div class="hint sub">given-clause loop budget</div>
      </div>
      <div v-if="!prover.vampireSelected">
        <label for="cfgMaxLits">max literals</label>
        <input
          id="cfgMaxLits"
          type="number"
          min="0"
          step="1"
          v-model.number="prover.cfg.maxLits"
        />
        <div class="hint sub">per retained clause</div>
      </div>
      <div>
        <label for="cfgSelectionPct">selection budget</label>
        <input
          id="cfgSelectionPct"
          type="range"
          min="0"
          max="100"
          step="1"
          v-model.number="prover.cfg.selectionTolerancePct"
        />
        <div class="hint sub">{{ prover.selectionPctLabel }}</div>
      </div>
      <div v-if="prover.vampireSelected">
        <label for="cfgVampireArgs">extra CLI args</label>
        <input
          id="cfgVampireArgs"
          type="text"
          placeholder="e.g. --avatar off"
          spellcheck="false"
          v-model="prover.vampireArgs"
        />
        <div class="hint sub">
          appended to Vampire's command line (advanced)
        </div>
      </div>
    </div>
    <div v-if="!prover.vampireSelected" class="settings-checks">
      <label class="check"
        ><input
          id="cfgForwardClose"
          type="checkbox"
          v-model="prover.cfg.forwardClose"
        />
        forward closure
        <span class="hint">— saturate the theory before the loop</span></label
      >
      <label class="check"
        ><input
          id="cfgWantProof"
          type="checkbox"
          v-model="prover.cfg.wantProof"
        />
        want proof
        <span class="hint">— populate the proof, graph and prose</span></label
      >
      <label class="check"
        ><input id="cfgProfile" type="checkbox" v-model="prover.cfg.profile" />
        profile
        <span class="hint">— emit phase timings into raw output</span></label
      >
    </div>
    <div class="inline" style="justify-content: flex-end; margin-top: 10px">
      <button class="btn ghost" type="button" @click="prover.reset()">
        Reset to defaults
      </button>
    </div>
  </div>
</template>

<style scoped></style>
