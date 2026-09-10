<script setup lang="ts">
import { computed } from "vue";

const props = defineProps<{
  /** Every diagnostic for the buffer; only errors and warnings are listed
   *  here (Monaco still gets all severities as inline markers). */
  diags: any[];
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

const line = (d: any) => Math.max(1, d.line || 1);
const col = (d: any) => Math.max(1, d.col || 1);
const loc = (d: any) => `${props.file || "untitled"}:${line(d)}:${col(d)}`;
</script>

<template>
  <details class="card problems" open>
    <summary>
      <span>Problems</span>
      <span class="summary-text">{{ summary }}</span>
    </summary>
    <div class="list" aria-live="polite">
      <div v-if="!items.length" class="hint empty">
        This file has no errors or warnings.
      </div>
      <button
        v-for="(d, i) in items"
        :key="i"
        class="edit-diag"
        type="button"
        :data-sev="d.severity"
        @click="emit('jump', { line: line(d), col: col(d) })"
      >
        <span class="sev" :class="d.severity">{{ d.severity }}</span>
        <span class="edit-diag-loc">{{ loc(d) }}</span>
        <span class="edit-diag-msg"
          >{{ d.message }}
          <span class="edit-diag-code">[{{ d.kind }}/{{ d.code }}]</span></span
        >
      </button>
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
.edit-diag {
  width: 100%;
  display: grid;
  grid-template-columns: auto auto 1fr;
  align-items: baseline;
  gap: 8px;
  padding: 8px 14px;
  border: 0;
  border-bottom: 1px solid var(--line);
  border-left: 3px solid transparent;
  background: var(--bg);
  color: var(--fg);
  font: inherit;
  text-align: left;
  cursor: pointer;
}
.edit-diag:last-child {
  border-bottom: 0;
}
.edit-diag:hover {
  background: var(--card);
}
/* Inset outline, not the ~1-2% luminance hover swap: this row sits inside a
   scrolling list, and it's the interactive control here a keyboard user is
   most likely to arrow/tab through. An outside outline would also get
   clipped by the list's overflow-y. */
.edit-diag:focus-visible {
  background: var(--card);
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
