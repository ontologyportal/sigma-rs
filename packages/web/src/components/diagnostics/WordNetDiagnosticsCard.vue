<script setup lang="ts">
/** WordNet<->KB diagnostics: a port of Java Sigma's WordNet diagnostics
 *  page (ontologyportal/sigma-rs#64), shown in its own sub-tab of
 *  DiagnosticsTab. Not counted in the tab's diagnostic-count badge — this
 *  is a lexicon/KB coverage report, not a KIF validation finding. */
import type { WordNetDiagnostics } from "sigmakee/sdk";
import Card from "../Card.vue";
import SourceLoc from "../SourceLoc.vue";
import { useShellStore } from "../../stores/shell.ts";
import { computed } from "vue";

defineProps<{
  diag: WordNetDiagnostics;
}>();

/** Thousands-separated -- these counts run into the tens of thousands over
 *  the full WordNet lexicon, and a bare `82115` is slower to parse at a
 *  glance than `82,115`. */
const fmt = (n: number) => n.toLocaleString();

const shell = useShellStore();
const colDirection = computed(() =>
  shell.effectiveLayout == "comfortable" ? "column" : "row",
);

/** A capped report's own "N more" tail -- true whenever the total exceeds
 *  what the worker sent back (see `wordnetDiagnostics`'s `limit`). */
function remaining(report: { items: unknown[]; total: number }): number {
  return Math.max(0, report.total - report.items.length);
}

function wordnetSuff(suffix: string) {
  if (suffix == "=") {
    return "equivalent";
  } else if (suffix == "+") {
    return "subsuming";
  } else if (suffix == "@") {
    return "instance";
  } else if (suffix == ":") {
    return "anti-equivalent";
  } else if (suffix == "[") {
    return "anti-subsuming";
  } else if (suffix == "]") {
    return "anti-instance";
  } else {
    return "unknown";
  }
}

/** Mapping-kind stat tiles, in the mapping files' own = / + / @ order, then
 *  their three negated ("anti-") counterparts -- kept as a visually
 *  quieter second row rather than interleaved, since they're the rare,
 *  exception-case counts. */
const MAPPING_STATS: {
  key: keyof WordNetDiagnostics["counts"];
  label: string;
}[] = [
  { key: "equivalent", label: "equivalent" },
  { key: "subsuming", label: "subsuming" },
  { key: "instance", label: "instance" },
];
const ANTI_MAPPING_STATS: {
  key: keyof WordNetDiagnostics["counts"];
  label: string;
}[] = [
  { key: "anti_subsuming", label: "anti-subsuming" },
  { key: "anti_instance", label: "anti-instance" },
  { key: "anti_equivalent", label: "anti-equivalent" },
];
const POS_STATS: { key: keyof WordNetDiagnostics["counts"]; label: string }[] =
  [
    { key: "nouns", label: "nouns" },
    { key: "verbs", label: "verbs" },
    { key: "adjectives", label: "adjectives" },
    { key: "adverbs", label: "adverbs" },
  ];
</script>

<template>
  <Card title="WordNet diagnostics">
    <template #description
      >WordNet-to-KB mapping coverage over the installed lexicon.
    </template>

    <div class="wn-stats">
      <div v-for="s in POS_STATS" :key="s.key" class="wn-stat">
        <span class="wn-stat-value">{{ fmt(diag.counts[s.key]) }}</span>
        <span class="wn-stat-label">{{ s.label }}</span>
      </div>
    </div>
    <div class="wn-stats">
      <div v-for="s in MAPPING_STATS" :key="s.key" class="wn-stat">
        <span class="wn-stat-value">{{ fmt(diag.counts[s.key]) }}</span>
        <span class="wn-stat-label">{{ s.label }}</span>
      </div>
    </div>
    <div class="wn-stats muted">
      <div v-for="s in ANTI_MAPPING_STATS" :key="s.key" class="wn-stat">
        <span class="wn-stat-value">{{ fmt(diag.counts[s.key]) }}</span>
        <span class="wn-stat-label">{{ s.label }}</span>
      </div>
    </div>

    <details class="wn-report">
      <summary>
        Synsets without a SUMO mapping
        <span class="wn-count">{{ fmt(diag.unmapped_synsets.total) }}</span>
      </summary>
      <div class="wn-rows">
        <div
          v-for="(row, i) in diag.unmapped_synsets.items"
          :key="i"
          class="wn-row"
        >
          <span class="wn-row-main">
            <span class="wn-pos">{{ row.pos }}</span>
            {{ row.words }}
          </span>
          <SourceLoc
            :file="row.file"
            :line="row.line"
            :blame="false"
            variant="loc"
          />
        </div>
      </div>
      <div v-if="remaining(diag.unmapped_synsets)" class="hint wn-more">
        … {{ fmt(remaining(diag.unmapped_synsets)) }} more
      </div>
    </details>

    <details class="wn-report">
      <summary>
        Synsets mapped to a term not in the loaded KB
        <span class="wn-count">{{ fmt(diag.missing_terms.total) }}</span>
      </summary>
      <div class="wn-rows">
        <div
          v-for="(row, i) in diag.missing_terms.items"
          :key="i"
          class="wn-row"
        >
          <span class="wn-row-main">
            <span class="wn-pos">{{ row.pos }}</span>
            {{ row.words }}
            <span class="wn-pos">{{ wordnetSuff(row.suffix) }}</span>
            <code>{{ row.term }}</code>
          </span>
          <SourceLoc
            :file="row.file"
            :line="row.line"
            :blame="false"
            variant="loc"
          />
        </div>
      </div>
      <div v-if="remaining(diag.missing_terms)" class="hint wn-more">
        … {{ fmt(remaining(diag.missing_terms)) }} more
      </div>
    </details>

    <details class="wn-report">
      <summary>
        Loaded KB terms with no WordNet synset
        <span class="wn-count">{{
          fmt(diag.terms_without_synsets.total)
        }}</span>
      </summary>
      <div class="wn-rows">
        <div
          v-for="(t, i) in diag.terms_without_synsets.items"
          :key="i"
          class="wn-row"
        >
          <span class="wn-row-main">
            <code>{{ t.symbol }}</code>
            <span v-if="t.kinds.length" class="hint"
              >({{ t.kinds.join(", ") }})</span
            >
          </span>
        </div>
      </div>
      <div v-if="remaining(diag.terms_without_synsets)" class="hint wn-more">
        … {{ fmt(remaining(diag.terms_without_synsets)) }} more
      </div>
    </details>

    <details class="wn-report">
      <summary>
        Hypernym / taxonomy mismatches
        <span class="wn-count">{{ fmt(diag.taxonomy_mismatches.total) }}</span>
      </summary>
      <div class="wn-rows">
        <div
          v-for="(m, i) in diag.taxonomy_mismatches.items"
          :key="i"
          class="wn-row"
        >
          <span class="wn-row-main">
            {{ m.word }} (<code>{{ m.term }}</code
            >)
            <span class="wn-arrow">&rarr;</span>
            {{ m.hypernym_word }} (<code>{{ m.hypernym_term }}</code
            >)
            <span class="hint"
              >— {{ m.hypernym_term }} is not an ancestor of {{ m.term }}</span
            >
          </span>
          <SourceLoc
            :file="m.file"
            :line="m.line"
            :blame="false"
            variant="loc"
          />
        </div>
      </div>
      <div v-if="remaining(diag.taxonomy_mismatches)" class="hint wn-more">
        … {{ fmt(remaining(diag.taxonomy_mismatches)) }} more
      </div>
    </details>
  </Card>
</template>

<style scoped>
/* Stat tiles: value over label, in a wrapping auto-fill grid rather than a
   run-on prose sentence -- each number gets its own scannable cell. The
   negated ("anti-") row is visually quieter (.muted): these are the rare
   exception counts, not the headline totals. */
.wn-stats {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(90px, 1fr));
  gap: 10px;
  padding: 8px 0;
  border-bottom: 1px solid var(--line);
}
.wn-stats:last-of-type {
  border-bottom: none;
  margin-bottom: 4px;
}
.wn-stats.muted {
  opacity: 0.7;
}
.wn-stat {
  display: flex;
  flex-direction: column;
  gap: 2px;
  border-radius: 5px;
  background-color: var(--line);
  padding: 5px 10px;
}
.wn-stat-value {
  font-size: 17px;
  font-weight: 600;
  font-variant-numeric: tabular-nums;
}
.wn-stat-label {
  font-size: 11px;
  color: var(--muted);
  text-transform: uppercase;
  letter-spacing: 0.04em;
}

.wn-report {
  border-top: 1px solid var(--line);
  padding: 8px 0;
}
.wn-report summary {
  cursor: pointer;
  font-size: 13px;
  font-weight: 600;
  display: flex;
  align-items: center;
  gap: 8px;
}
.wn-count {
  display: inline-block;
  min-width: 15px;
  padding: 0 5px;
  border-radius: 999px;
  font-size: 10px;
  font-weight: 700;
  line-height: 15px;
  text-align: center;
  background: color-mix(in srgb, var(--accent) 20%, transparent);
  color: var(--accent);
}

/* One row per finding: main content left, source location right --
   mirrors DiagnosticsTab's .diag/.diag-head rows rather than a bullet
   list, so a long run of similar entries stays scannable. */
.wn-rows {
  margin-top: 8px;
}
.wn-row {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  flex-wrap: wrap;
  gap: 4px 14px;
  padding: 6px 2px;
  border-bottom: 1px solid var(--line);
  font-size: 13px;
  flex-direction: v-bind(colDirection);
}
.wn-row:last-child {
  border-bottom: none;
}
.wn-row-main {
  display: flex;
  align-items: baseline;
  flex-wrap: wrap;
  gap: 6px;
  min-width: 0;
}
.wn-row-main code {
  font-family: var(--mono);
  font-size: 12px;
}
.wn-arrow {
  color: var(--muted);
}
.wn-pos {
  display: inline-block;
  min-width: 14px;
  padding: 0 4px;
  border-radius: 4px;
  font-family: var(--mono);
  font-size: 10px;
  font-weight: 700;
  text-align: center;
  background: color-mix(in srgb, var(--muted) 18%, transparent);
  color: var(--muted);
}
.wn-more {
  margin-top: 6px;
}
</style>
