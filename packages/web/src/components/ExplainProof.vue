<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import type { AuditStep } from "sigmakee/sdk";
import {
  EXPLAIN_LIMITS,
  canonicalJson,
  explainAvailable,
  toExplainRequest,
  validateExplainRequest,
  type ExplainKind,
} from "../api/explain";
import { useExplain } from "../composables/useExplain";
import { fmtSecs } from "../composables/useElapsed";
import { useAuthStore } from "../stores/auth";
import { renderMarkdown } from "../utils/markdown";
import BusyButton from "./BusyButton.vue";
import Disclosure from "./Disclosure.vue";

/** "Explain with AI" for one proof: asks the deployment's model for a short
 *  English account of an Ask/Tell proof, or of a contradiction plus possible
 *  fixes. Click-driven only, signed-in only, hidden where the deployment has
 *  no model. */
const props = defineProps<{
  kind: ExplainKind;
  steps: AuditStep[];
  /** Ask/Tell: the query the proof answers. */
  query?: string;
}>();

const auth = useAuthStore();
const available = ref(false);
onMounted(async () => {
  available.value = await explainAvailable();
});

const request = computed(() =>
  toExplainRequest(props.kind, props.steps, props.query),
);
const tooLarge = computed(() => {
  if (props.steps.length > EXPLAIN_LIMITS.maxSteps)
    return `Too long to explain (${props.steps.length} steps; at most ${EXPLAIN_LIMITS.maxSteps}).`;
  const checked = validateExplainRequest(request.value);
  return checked.ok ? "" : `Can't explain this proof: ${checked.reason}.`;
});

const { result, error, busy, explain, show } = useExplain();
watch(
  () => canonicalJson(request.value),
  () => show(request.value),
  { immediate: true },
);

const html = computed(() =>
  result.value ? renderMarkdown(result.value.explanation) : "",
);

const title = computed(() => {
  if (!auth.signedIn) return "Sign in with GitHub to explain proofs";
  return props.kind === "contradiction"
    ? "Ask an AI model which axioms conflict, why, and how they might be fixed"
    : "Ask an AI model to explain how this proof reaches the query";
});

const errorText = computed(() => {
  const e = error.value;
  if (!e) return "";
  switch (e.code) {
    case "unauthorized":
      return "Your sign-in has expired. Sign in with GitHub again to explain proofs.";
    case "rate_limited":
      return e.retryAfter
        ? `Too many explanations right now. Try again in ${fmtSecs(e.retryAfter)}.`
        : "Too many explanations right now. Try again shortly.";
    case "paused":
      return "Explanations are paused until 00:00 UTC: today's free AI allowance is used up.";
    case "invalid":
      return `Can't explain this proof: ${e.message}`;
    default:
      return `${e.message} Try again.`;
  }
});

function onClick() {
  if (!auth.signedIn) {
    auth.openLoginDialog();
    return;
  }
  void explain(request.value).then(() => {
    if (error.value?.code === "unavailable") available.value = false;
    else if (error.value?.code === "unauthorized") auth.openLoginDialog();
  });
}
</script>

<template>
  <div v-if="available && steps.length" class="explain">
    <div v-if="!result" class="explain-hd">
      <BusyButton
        ghost
        class="explain-btn"
        label="Explain with AI"
        busy-label="Explaining…"
        :busy="busy"
        :disabled="!!tooLarge"
        :title="tooLarge || title"
        @click="onClick"
      />
      <span v-if="!auth.signedIn" class="hint">Sign in to use</span>
      <span v-else-if="tooLarge" class="hint">{{ tooLarge }}</span>
      <span v-if="errorText" class="hint bad" role="alert">{{
        errorText
      }}</span>
    </div>
    <Disclosure
      v-if="result"
      :summary="
        kind === 'contradiction'
          ? 'AI explanation and possible fixes'
          : 'AI explanation'
      "
      open
    >
      <div class="explain-md" v-html="html"></div>
      <div class="hint explain-note">
        Written by {{ result.model.replace(/^@cf\//, "")
        }}{{ result.cached ? " (cached)" : "" }}. It can be wrong; check it
        against the proof steps.
      </div>
    </Disclosure>
  </div>
</template>

<style scoped>
.explain {
  margin-top: 10px;
}
.explain-hd {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
}
.explain-btn {
  height: 32px;
  font-size: 13px;
}
.explain-md {
  margin-top: 6px;
  padding: 10px 12px;
  border: 1px solid var(--line);
  border-left: 3px solid var(--accent);
  border-radius: 7px;
  font-size: 14px;
  line-height: 1.6;
  overflow-wrap: anywhere;
}
.explain-md :deep(p),
.explain-md :deep(ul),
.explain-md :deep(ol) {
  margin: 0 0 8px;
}
.explain-md > :deep(:last-child) {
  margin-bottom: 0;
}
.explain-md :deep(code) {
  font-family: var(--mono);
  font-size: 12px;
}
.explain-md :deep(pre) {
  white-space: pre-wrap;
}
.explain-note {
  margin-top: 6px;
}
</style>
