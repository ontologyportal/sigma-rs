<script setup lang="ts">
import { computed } from "vue";
import { SUMO } from "../constants";
import { navigate } from "../router";
import { useKBStore } from "../stores/kb";

const props = withDefaults(
  defineProps<{
    /** The cited constituent's name; nothing renders without one. */
    file?: string;
    /** 1-based line inside `file`. */
    line?: number;
    /** `ref` (man-page / proof citations) or `loc` (diagnostics rows). */
    variant?: "ref" | "loc";
    /** Also render the GitHub blame link for a `sumo`-origin file. */
    blame?: boolean;
  }>(),
  { variant: "ref", blame: true },
);

const kb = useKBStore();

const label = computed(() => `${props.file}:${props.line}`);
const locClass = computed(() =>
  props.variant === "loc" ? "loc" : "hint ref-loc",
);
/** Only a loaded constituent can be opened in the editor -- a proof can
 *  cite a synthetic/CNF source, or an axiom from a file the user has since
 *  removed, and neither is openable. */
const openable = computed(() => !!props.file && kb.isLoaded(props.file));

/**
 * GitHub *blame* deep-link for a SUMO-sourced constituent, else null.
 *
 * Blame rather than blob: it lands on the same line but with per-line author,
 * date and commit attribution -- "who last changed this axiom" -- for free. The
 * API route to that data is GraphQL `Blob.blame`, which requires a token even
 * for public repos and so is unusable from a static, unauthenticated page.
 */
const blameUrl = computed(() => {
  if (!props.blame || !props.file) return null;
  if (kb.find(props.file)?.origin.kind !== "sumo") return null;
  return `https://github.com/${SUMO.owner}/${SUMO.repo}/blame/${SUMO.ref}/${props.file}#L${props.line}`;
});

/** Open the file in the Edit tab with the caret on the line. Routed through
 *  the URL so the jump is a real history entry and the view is shareable. */
function open(e: Event) {
  e.preventDefault();
  navigate("edit", { file: props.file, l: props.line > 0 ? props.line : null });
}
</script>

<template>
  <template v-if="file">
    <a v-if="openable" :class="[locClass, 'jump-src']" @click="open">{{
      label
    }}</a>
    <span v-else :class="locClass">{{ label }}</span>
    <a
      v-if="blameUrl"
      class="hint gh"
      :href="blameUrl"
      target="_blank"
      rel="noopener"
      title="Who last changed this line (GitHub blame)"
      >blame ↗</a
    >
  </template>
</template>

<style scoped>
.ref-loc {
  font-family: var(--mono);
  color: var(--accent);
}
.loc {
  font-family: var(--mono);
  font-size: 12px;
  color: var(--accent);
  cursor: pointer;
}
.loc:hover {
  text-decoration: underline;
}
/* Only citations we can actually open are rendered as <a class="jump-src">;
   unopenable ones stay as plain <span> and must not look clickable. */
a.jump-src {
  cursor: pointer;
}
a.jump-src:hover {
  text-decoration: underline;
}
span.ref-loc,
span.loc {
  color: var(--muted);
  cursor: default;
}
</style>
