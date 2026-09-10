<script setup lang="ts">
import { ref, shallowRef, computed } from "vue";
import { call } from "../services/sigma";
import { errMsg } from "../utils/format";
import { useProverStore } from "../stores/prover";
import BusyButton from "../components/BusyButton.vue";
import Card from "../components/Card.vue";
import Disclosure from "../components/Disclosure.vue";
import ProofView from "../components/ProofView.vue";
import ProverSettings from "../components/ProverSettings.vue";

const prover = useProverStore();

const auditTimeSecs = ref(15);
const auditLimit = ref(5);
const auditing = ref(false);
const error = ref("");

// The raw result + backend label, kept so the proof-language/plain-proof
// toggles re-render in place without re-running the audit.
const result = shallowRef<any | null>(null);
const backendLabel = ref("");

const badgeClass = computed(() => `audit-status ${result.value?.status || ""}`);
const backendText = computed(() =>
  backendLabel.value ? `via ${backendLabel.value}` : "",
);
const stepsText = computed(() => {
  const r = result.value;
  return r?.given_steps != null ? `${r.given_steps} given-clause steps` : "";
});
const verdict = computed(() => {
  const r = result.value;
  if (!r) return "";
  if (r.status === "Consistent")
    return "No contradiction found — the loaded KB saturated cleanly.";
  // Vampire's one-shot run yields at most a single contradiction (no
  // enumerator, unlike the native audit's driver) -- this covers the case
  // where even that single witness failed to parse; say so without
  // implying "zero".
  if (r.inconsistent && !r.contradictions.length)
    return "Contradiction found — see raw engine output for the derivation.";
  if (r.inconsistent)
    return `${r.contradictions.length} distinct contradiction${r.contradictions.length === 1 ? "" : "s"} found.`;
  return "No contradiction found within budget — inconclusive (raise the time limit and try again).";
});
const rawOutput = computed(() => result.value?.raw_output || "(none)");
const contradictions = computed<any[]>(
  () => result.value?.contradictions || [],
);

const heading = (c: any, i: number) =>
  `Contradiction #${i + 1} — ${c.steps.length} step${c.steps.length === 1 ? "" : "s"}`;

async function runAudit() {
  const vampire = prover.vampireSelected;
  auditing.value = true;
  error.value = "";
  try {
    // Audit inherits the Ask/Tell prover settings (including backend, via
    // the shared settings panel both tabs toggle), but keeps its own time
    // limit.
    const res = vampire
      ? await call("auditVampire", {
          timeLimitSecs: auditTimeSecs.value,
          extraArgs: prover.vampireArgs.trim(),
        })
      : await call("audit", {
          config: prover.config({ timeLimitSecs: auditTimeSecs.value }),
          limit: Math.max(1, Number(auditLimit.value) || 5),
        });
    result.value = res.result;
    backendLabel.value = vampire ? "Vampire" : "SUPr";
  } catch (e) {
    error.value = errMsg(e);
  } finally {
    auditing.value = false;
  }
}
</script>

<template>
  <div>
    <Card>
      <div class="inline between">
        <div class="hint">
          Saturates the loaded KB looking for a logical contradiction and prints
          the proofs for said contradictions.
        </div>
        <div class="inline">
          <div class="num-field">
            <label for="auditTime">time limit (s)</label
            ><input
              type="number"
              id="auditTime"
              v-model.number="auditTimeSecs"
              min="0"
            />
          </div>
          <div class="num-field" v-if="!prover.vampireSelected">
            <label for="auditLimit">max found</label
            ><input
              type="number"
              id="auditLimit"
              v-model.number="auditLimit"
              min="1"
            />
          </div>
          <BusyButton
            :busy="auditing"
            label="Run audit"
            busy-label="Auditing…"
            @click="runAudit"
          />
          <button
            class="cog"
            type="button"
            title="Prover settings"
            aria-label="Prover settings"
            :aria-expanded="prover.settingsOpen"
            @click="prover.toggleSettings()"
          >
            ⚙
          </button>
        </div>
      </div>
    </Card>
    <ProverSettings />
    <div>
      <Card v-if="error" class="hint bad">{{ error }}</Card>
      <template v-else-if="result">
        <Card>
          <div class="inline">
            <span :class="badgeClass">{{ result.status }}</span>
            <span class="hint">{{ backendText }}</span>
            <span class="hint">{{ stepsText }}</span>
          </div>
          <div class="hint verdict">{{ verdict }}</div>
          <Disclosure summary="raw engine output">
            <pre>{{ rawOutput }}</pre>
          </Disclosure>
        </Card>
        <Card v-for="(c, i) in contradictions" :key="i">
          <div class="contradiction-hd">{{ heading(c, i) }}</div>
          <ProofView
            :steps="c.steps"
            :prologue="c.proof_tptp_prologue"
            :prose="c.prose"
            :prose-missing="c.prose_missing"
            :graphviz="c.graphviz"
          />
        </Card>
      </template>
    </div>
  </div>
</template>

<style scoped>
.num-field {
  width: 110px;
}
.verdict {
  margin-top: 8px;
}
.contradiction-hd {
  font-weight: 600;
  margin-bottom: 6px;
}
</style>
