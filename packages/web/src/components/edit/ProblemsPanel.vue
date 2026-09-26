<script setup lang="ts">
import { diagnosticText, type Diagnostic } from "../../stores/kb";
import { computed } from "vue";
import CopyButton from "../CopyButton.vue";

const props = defineProps<{
  /** Every diagnostic for the buffer; only errors and warnings are listed
   *  here (Monaco still gets all severities as inline markers). */
  diags: Diagnostic[];
  /** The buffer's file name, for the `file:line:col` locations. */
  file?: string;
}>();

const emit = defineEmits<{
  /** A row was clicked: move the caret there. */
  jump: [pos: { line: number; col: number }];
}>();

const items = computed(() =>
  props.diags.filter((d) => d.severity === "error" || d.severity === "warning"),
);

const summary = computed(() => {
  const errors = items.value.filter((d) => d.severity === "error").length;
  const warnings = items.value.length - errors;
  const parts: string[] = [];
  if (errors) parts.push(`${errors} error${errors === 1 ? "" : "s"}`);
  if (warnings) parts.push(`${warnings} warning${warnings === 1 ? "" : "s"}`);
  return parts.length ? parts.join(", ") : "No errors or warnings";
});

const line = (d: Diagnostic) => Math.max(1, d.line || 1);
const col = (d: Diagnostic) => Math.max(1, d.col || 1);
const loc = (d: Diagnostic) =>
  `${props.file || "untitled"}:${line(d)}:${col(d)}`;
const text = (d: Diagnostic) => diagnosticText(d, loc(d));
const allText = () => items.value.map(text).join("\n");

/** Row click jumps to the diagnostic -- unless the click ended a text
 *  selection, which is the user copying, not navigating. */
function onRowClick(d: Diagnostic) {
  if (window.getSelection()?.toString()) return;
  emit("jump", { line: line(d), col: col(d) });
}
</script>

<template>
  <details class="card problems" open>
    <summary>
      <span>Problems</span>
      <span class="summary-text">{{ summary }}</span>
      <CopyButton
        v-if="items.length"
        class="copy-all"
        :text="allText"
        title="Copy all errors and warnings"
      />
    </summary>
    <div class="list" aria-live="polite">
      <div v-if="!items.length" class="hint empty">
        This file has no errors or warnings.
      </div>
      <div
        v-for="(d, i) in items"
        :key="i"
        class="edit-diag"
        :data-sev="d.severity"
        @click="onRowClick(d)"
      >
        <span class="sev" :class="d.severity">{{ d.severity }}</span>
        <button
          class="edit-diag-loc"
          type="button"
          title="Go to this line"
          @click.stop="emit('jump', { line: line(d), col: col(d) })"
        >
          {{ loc(d) }}
        </button>
        <span class="edit-diag-msg"
          >{{ d.message }}
          <span class="edit-diag-code">[{{ d.kind }}/{{ d.code }}]</span></span
        >
        <CopyButton :text="text(d)" title="Copy this diagnostic" />
      </div>
    </div>
  </details>
</template>

<style scoped>
/* File-scoped problems panel directly beneath the editor. */
.problems {
  background: var(--card);
  border: 1px solid var(--line);
  border-radius: 10px;
  margin-bottom: 12px;
  padding: 0;
  overflow: hidden;
}
.problems > summary {
  display: flex;
  align-items: center;
  gap: 8px;
  cursor: pointer;
  list-style: none;
  padding: 10px 14px;
  font-size: 13px;
  font-weight: 600;
}
.problems > summary::-webkit-details-marker {
  display: none;
}
.problems > summary::before {
  content: ">";
  color: var(--muted);
  font-size: 15px;
  line-height: 1;
  transition: transform 0.12s ease;
}
.problems[open] > summary::before {
  transform: rotate(90deg);
}
.summary-text {
  color: var(--muted);
  font-weight: 400;
}
.list {
  max-height: 220px;
  overflow-y: auto;
  border-top: 1px solid var(--line);
}
.copy-all {
  margin-left: auto;
}
.edit-diag {
  display: grid;
  grid-template-columns: auto auto 1fr auto;
  align-items: baseline;
  gap: 8px;
  padding: 8px 14px;
  border-bottom: 1px solid var(--line);
  border-left: 3px solid transparent;
  background: var(--bg);
  color: var(--fg);
  cursor: pointer;
}
.edit-diag:last-child {
  border-bottom: 0;
}
.edit-diag:hover {
  background: var(--card);
}
/* Inset outline: the location is the row's keyboard-focusable jump control,
   and an outside outline would get clipped by the list's overflow-y. */
.edit-diag-loc:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: -2px;
}
.edit-diag[data-sev="error"] {
  border-left-color: var(--bad);
}
.edit-diag[data-sev="warning"] {
  border-left-color: var(--warn);
}
.edit-diag-loc {
  padding: 0;
  border: 0;
  background: none;
  cursor: pointer;
  color: var(--accent);
  font-family: var(--mono);
  font-size: 12px;
  white-space: nowrap;
}
.edit-diag-msg {
  min-width: 0;
  font-size: 13px;
}
.edit-diag-code {
  color: var(--muted);
  font-family: var(--mono);
  font-size: 11px;
}
.empty {
  padding: 10px 14px;
}
</style>
