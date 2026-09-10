<script setup lang="ts">
import { computed } from "vue";
import { useProverStore } from "../stores/prover";
import { highlightTptp } from "../utils/highlight-tptp";
import CiteRow from "./CiteRow.vue";
import ProofGraph from "./ProofGraph.vue";

const props = defineProps<{
  /** The `{index, rule, premises, kif, tptp, file, line}[]` transcript. */
  steps: any[];
  /** Whole-proof TPTP material that belongs to no single step, e.g. TFF's
   *  `$i`-monomorphic type-declaration preamble. */
  prologue?: string;
  /** The plain-English rendering of the transcript. */
  prose?: string;
  /** Symbols the prose had to show by bare name (no format/termFormat). */
  proseMissing?: string[];
  /** The engine's graphviz (DOT) source for the proof graph toggle. */
  graphviz?: string;
  /** The engine's raw textual output. */
  rawOutput?: string;
}>();

const prover = useProverStore();

const showPrologue = computed(
  () => prover.proofLang === "tptp" && !!props.prologue,
);
const prologueHtml = computed(() =>
  showPrologue.value
    ? highlightTptp(props.prologue, { linkSymbols: true }).replace(/\n$/, "")
    : "",
);

/** The whole proof as unstyled text, one formula per line (the TPTP
 *  preamble first, when present) -- the settings panel's "plain proof". */
const plainText = computed(() => {
  const lines: string[] = [];
  if (showPrologue.value) lines.push(props.prologue);
  for (const s of props.steps)
    lines.push(prover.proofLang === "tptp" && s.tptp != null ? s.tptp : s.kif);
  return lines.join("\n");
});

/** Step indices are 0-based on the wire; the list and the graph both label
 *  from 1, so shift for display. `pos` is the fallback when a step carries
 *  no explicit `index`. */
const stepNumber = (s: any, pos: number) =>
  (s.index != null ? s.index : pos) + 1;
const premiseRefs = (s: any) => {
  if (!s.premises || !s.premises.length) return "";
  const label = s.premises.length === 1 ? "step" : "steps";
  return `(from ${label} ${s.premises.map((p: number) => p + 1).join(", ")})`;
};

const missingNote = computed(() =>
  props.proseMissing && props.proseMissing.length
    ? `${props.proseMissing.length} symbol(s) shown by bare name (no format/termFormat in EnglishLanguage): ${props.proseMissing.join(", ")}`
    : "",
);
</script>

<template>
  <div>
    <ol class="refs">
      <li v-if="prover.plainProof">
        <pre class="ref-kif">{{ plainText }}</pre>
      </li>
      <template v-else>
        <li v-if="showPrologue">
          <div class="hint">type declarations</div>
          <pre class="ref-kif" v-html="prologueHtml"></pre>
        </li>
        <CiteRow
          v-for="(s, i) in steps"
          :key="i"
          :kif="s.kif"
          :tptp="s.tptp"
          :lang="prover.proofLang"
          :file="s.file"
          :line="s.line"
        >
          <template #header>
            <span class="step-num">{{ stepNumber(s, i) }}.</span> {{ s.rule
            }}<span v-if="premiseRefs(s)" class="hint premises">{{
              premiseRefs(s)
            }}</span>
          </template>
        </CiteRow>
      </template>
    </ol>
    <details class="prose-details">
      <summary class="hint">proof in plain English</summary>
      <div class="prose">{{ prose || "" }}</div>
      <div v-if="missingNote" class="hint missing-note">{{ missingNote }}</div>
    </details>
    <ProofGraph :steps="steps" :dot="graphviz" />
    <details v-if="rawOutput !== undefined">
      <summary class="hint">raw engine output</summary>
      <pre>{{ rawOutput || "(none)" }}</pre>
    </details>
  </div>
</template>

<style scoped>
ol.refs {
  list-style: none;
  margin: 4px 0 0;
  padding: 0;
  max-height: 420px;
  overflow-y: auto;
}
ol.refs li {
  padding: 8px 2px;
  border-bottom: 1px solid var(--line);
}
ol.refs li:last-child {
  border-bottom: none;
}
.premises {
  margin-left: 4px;
}
ol.refs .step-num {
  font-weight: 700;
  color: var(--fg);
  font-variant-numeric: tabular-nums;
}
.ref-kif {
  margin: 0;
  font-family: var(--mono);
  font-size: 12px;
  white-space: pre;
  overflow-x: auto;
}
/* Plain-English proof */
.prose-details {
  margin-top: 10px;
}
.prose {
  font-size: 14px;
  line-height: 1.65;
  white-space: pre-wrap;
}
.prose:empty::before {
  content: "(no prose available)";
  color: var(--muted);
}
.missing-note {
  margin-top: 6px;
}
details pre {
  font-family: var(--mono);
  font-size: 12px;
  white-space: pre-wrap;
  background: var(--bg);
  border: 1px solid var(--line);
  border-radius: 7px;
  padding: 10px;
  overflow-x: auto;
}
</style>
