<script setup lang="ts">
import { computed, ref } from "vue";
import type { AuditStep } from "sigmakee/sdk";
import BaseDialog from "../BaseDialog.vue";
import CopyButton from "../CopyButton.vue";
import SourceLoc from "../SourceLoc.vue";
import { diagnosticText, useKBStore, type Diagnostic } from "../../stores/kb";
import { axiomDiagnostics } from "../../utils/contradictionReport";

/** "Diagnose" for one audit contradiction: lists the validation diagnostics
 *  located on its cited source axioms -- a malformed or mistyped axiom is a
 *  common cause of a spurious contradiction. */
const props = defineProps<{ steps: AuditStep[] }>();

const kb = useKBStore();
const open = ref(false);
const includeHints = ref(false);

const shown = (d: Diagnostic) =>
  includeHints.value || (d.severity !== "hint" && d.severity !== "info");

const report = computed(() =>
  open.value
    ? axiomDiagnostics(
        props.steps,
        kb.diagnostics,
        (f) => kb.byFile(f)?.text ?? null,
      )
    : [],
);
const rows = computed(() =>
  report.value.map((a) => ({ ...a, visible: a.diagnostics.filter(shown) })),
);
const counts = computed(() => {
  const all = report.value.flatMap((a) => a.diagnostics);
  const serious = all.filter(
    (d) => d.severity === "error" || d.severity === "warning",
  ).length;
  return { all: all.length, serious, minor: all.length - serious };
});
const summary = computed(() => {
  const n = report.value.length;
  const axioms = `${n} source axiom${n === 1 ? "" : "s"}`;
  const { serious, minor } = counts.value;
  if (!serious && !minor) return `No diagnostics on the ${axioms}.`;
  const parts = [];
  if (serious)
    parts.push(
      `${serious} error${serious === 1 ? "" : "s"} or warning${serious === 1 ? "" : "s"}`,
    );
  if (minor) parts.push(`${minor} hint${minor === 1 ? "" : "s"}`);
  return `${parts.join(" and ")} on the ${axioms}.`;
});

const loc = (d: Diagnostic) => `${d.file}:${d.line}`;
const allText = () =>
  rows.value
    .flatMap((a) => a.visible)
    .map((d) => diagnosticText(d, loc(d)))
    .join("\n");
</script>

<template>
  <button
    class="btn ghost action-btn"
    type="button"
    title="List validation diagnostics on this contradiction's source axioms"
    @click="open = true"
  >
    Diagnose
  </button>

  <BaseDialog
    v-model="open"
    title="Diagnose contradiction"
    width="min(820px, 94vw)"
  >
    <p class="hint">
      Diagnostics on the axioms this contradiction cites. A malformed or
      mistyped axiom is a likely source of a spurious contradiction.
    </p>
    <div class="inline between head">
      <b>{{ summary }}</b>
      <label v-if="counts.minor" class="check"
        ><input v-model="includeHints" type="checkbox" /> Include hints</label
      >
    </div>
    <p v-if="!report.length" class="hint">
      The proof cites no source axioms, so there is nothing to diagnose.
    </p>
    <p v-else-if="!counts.serious && !includeHints" class="hint">
      None of the cited axioms has an error or warning, so the contradiction
      more likely follows from what the axioms say than from a malformed one.
    </p>
    <div class="axioms">
      <section v-for="a in rows" :key="`${a.step.file}:${a.step.line}`">
        <div class="axiom-hd" @click="open = false">
          <SourceLoc :file="a.step.file" :line="a.step.line" variant="loc" />
          <span class="hint">
            {{
              a.visible.length
                ? `${a.visible.length} diagnostic${a.visible.length === 1 ? "" : "s"}`
                : "no diagnostics"
            }}
          </span>
        </div>
        <pre class="kif">{{ a.step.kif }}</pre>
        <div
          v-for="(d, i) in a.visible"
          :key="i"
          class="diag"
          :data-sev="d.severity"
        >
          <span class="sev" :class="d.severity">{{ d.severity }}</span>
          <span class="code"
            >line {{ d.line }} [{{ d.kind }}/{{ d.code }}]</span
          >
          <span class="msg">{{ d.message }}</span>
          <CopyButton
            :text="diagnosticText(d, loc(d))"
            title="Copy this diagnostic"
          />
        </div>
      </section>
    </div>
    <template #actions>
      <CopyButton
        v-if="rows.some((a) => a.visible.length)"
        label="Copy all"
        :text="allText"
        title="Copy every listed diagnostic"
      />
      <span class="spacer"></span>
      <button class="btn" type="button" @click="open = false">Close</button>
    </template>
  </BaseDialog>
</template>

<style scoped>
button.action-btn {
  height: 32px;
  font-size: 13px;
}
p {
  margin: 0 0 10px;
}
.head {
  margin-bottom: 10px;
  font-size: 13px;
}
.axioms {
  max-height: min(56vh, 520px);
  overflow: auto;
}
section {
  border-top: 1px solid var(--line);
  padding: 10px 0;
}
.axiom-hd {
  display: flex;
  gap: 8px;
  align-items: baseline;
}
.kif {
  margin: 6px 0;
  padding: 8px 10px;
  border-radius: 6px;
  background: var(--bg);
  font-size: 12px;
  white-space: pre-wrap;
}
.diag {
  display: grid;
  grid-template-columns: auto auto 1fr auto;
  gap: 8px;
  align-items: baseline;
  padding: 6px 0 6px 8px;
  border-left: 3px solid transparent;
}
.diag[data-sev="error"] {
  border-left-color: var(--bad);
}
.diag[data-sev="warning"] {
  border-left-color: var(--warn);
}
.diag[data-sev="info"],
.diag[data-sev="hint"] {
  border-left-color: var(--accent);
}
.code {
  font-family: var(--mono);
  font-size: 11px;
  color: var(--muted);
  white-space: nowrap;
}
.msg {
  font-size: 13px;
}
</style>
