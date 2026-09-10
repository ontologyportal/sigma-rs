<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { call } from "../services/sigma";
import { useKBStore } from "../stores/kb";
import { highlightKif } from "../utils/highlight-kif";
import { highlightTptp } from "../utils/highlight-tptp";
import { useSymbolLinks } from "../composables/useSymbolLinks";
import SourceLoc from "./SourceLoc.vue";

const props = defineProps<{
  /** The formula; the paraphrase is always derived from this, whatever is displayed. */
  kif: string;
  /** The step's TPTP rendering, displayed when `lang` is `tptp`. */
  tptp?: string;
  /** Displayed dialect: `tptp` shows `tptp` (falling back to `kif` when absent), anything else `kif`. */
  lang?: "kif" | "tptp";
  /** Source constituent of the formula, for the `file:line` + blame footer. */
  file?: string;
  /** 1-based source line inside `file`. */
  line?: number;
  /** The viewed symbol: highlighted rather than linked inside the formula. */
  focusSymbol?: string;
}>();

const kb = useKBStore();
const root = ref<HTMLElement | null>(null);
useSymbolLinks(root);

const highlighted = computed(() =>
  props.lang === "tptp" && props.tptp != null
    ? highlightTptp(props.tptp, {
        focusSymbol: props.focusSymbol,
        linkSymbols: true,
      }).replace(/\n$/, "")
    : highlightKif(props.kif, {
        focusSymbol: props.focusSymbol,
        linkSymbols: true,
      }).replace(/\n$/, ""),
);

const isOpen = ref(false);
const nlText = ref("");
/** The `language|genericVars|kif` stamp the shown paraphrase was rendered
 *  for; '' when nothing (or a failure) is showing, so the next open retries. */
const nlLang = ref("");
const nlError = ref(false);
let seq = 0;

const nlKey = computed(() => `${kb.uiLanguage}|${kb.genericVars}|${props.kif}`);

async function renderNl() {
  const key = nlKey.value;
  if (nlLang.value === key) return;
  nlLang.value = key;
  nlError.value = false;
  nlText.value = "rendering…";
  const mine = ++seq;
  try {
    const { text } = await call("renderNl", {
      kif: props.kif,
      language: kb.uiLanguage,
      genericVars: kb.genericVars,
    });
    if (mine !== seq) return;
    nlText.value = text && text.trim() ? text : "no paraphrase available";
  } catch {
    if (mine !== seq) return;
    // A failed call is not a verdict -- clear the stamp so the next
    // expand retries instead of treating the failure as a cached answer.
    nlLang.value = "";
    nlError.value = true;
    nlText.value = "paraphrase failed — reopen to retry";
  }
}

function onToggle(e: Event) {
  isOpen.value = (e.target as HTMLDetailsElement).open;
}

watch([isOpen, nlKey], ([open]) => {
  if (open) renderNl();
});
</script>

<template>
  <li ref="root">
    <details class="cite" @toggle="onToggle">
      <summary>
        <div v-if="$slots.header" class="hint"><slot name="header" /></div>
        <pre class="ref-kif" v-html="highlighted"></pre>
      </summary>
      <div class="nl">{{ nlText }}</div>
    </details>
    <div v-if="file" class="ref-meta">
      <SourceLoc :file="file" :line="line" />
    </div>
  </li>
</template>

<style scoped>
details.cite > summary {
  cursor: pointer;
  list-style: none;
}
details.cite > summary::-webkit-details-marker {
  display: none;
}
details.cite > summary::after {
  content: "⌄ paraphrase";
  display: inline-block;
  margin-top: 4px;
  color: var(--muted);
  font-size: 11px;
}
details.cite[open] > summary::after {
  content: "⌃ hide paraphrase";
}
details.cite > .nl {
  margin-top: 6px;
  font-size: 12px;
  color: var(--muted);
  font-style: italic;
}
.ref-kif {
  margin: 0;
  font-family: var(--mono);
  font-size: 12px;
  white-space: pre;
  overflow-x: auto;
}
.ref-meta {
  display: flex;
  gap: 8px;
  align-items: baseline;
  flex-wrap: wrap;
  margin-top: 4px;
}
</style>
