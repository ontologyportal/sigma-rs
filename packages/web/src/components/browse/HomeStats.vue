<script setup lang="ts">
import { computed, onActivated, onMounted, ref, watch } from "vue";
import { fetchLastCommitInfo } from "../../api/github";
import { useKBStore } from "../../stores/kb";
import { errMsg, fmtDate, fmtNum } from "../../utils/format";
import StatPopover, { type PopRow } from "./StatPopover.vue";

const kb = useKBStore();

const statsError = ref("");
const commit = ref("—");
const commitTooltip = ref("");

/** A count from the stats payload; a stale engine (older wasm) omits newer
 *  fields, which should show as a dash rather than NaN. */
const num = (v: unknown) => (Number.isFinite(v as number) ? fmtNum(v) : "—");

const files = computed(() => num(kb.stats?.files));
const symbols = computed(() => num(kb.stats?.symbols));
const axioms = computed(() => num(kb.stats?.axioms));
const rules = computed(() => num(kb.stats?.rules));
const classes = computed(() => num(kb.stats?.classes));
const instances = computed(() => num(kb.stats?.instances));
const relations = computed(() => num(kb.stats?.relations));

/** Documentation coverage: % of symbols carrying a documentation string. */
const docPct = computed<number | null>(() => {
  const s = kb.stats;
  return s?.symbols && Number.isFinite(s.documented)
    ? Math.round((100 * s.documented) / s.symbols)
    : null;
});
const doc = computed(() => (docPct.value == null ? "—" : `${docPct.value}%`));
const docTooltip = computed(() => {
  const s = kb.stats;
  if (!s) return "Coverage by language";
  return (
    `${fmtNum(s.documented)} of ${fmtNum(s.symbols)} symbols have documentation; ` +
    `${fmtNum(s.labeled)} have a termFormat label`
  );
});

/** The only part of Home derived from `promoting` -- cheap, no RPC. */
const note = computed(() =>
  kb.promoting
    ? "Post-processing — counts will settle once axiomatization finishes."
    : statsError.value,
);

/** KB counts (skipped by the store unless something invalidated them), then
 *  the upstream commit date, best effort -- the rest of the page is useful
 *  without it. */
async function refresh() {
  try {
    await kb.refreshStats();
    statsError.value = "";
  } catch (e) {
    statsError.value = `Could not read KB stats: ${errMsg(e)}`;
  }
  try {
    const { date } = await fetchLastCommitInfo();
    commit.value = date ? fmtDate(date) : "unknown";
    commitTooltip.value = date ? date.toString() : "";
  } catch (e) {
    commit.value = "—";
    commitTooltip.value = `Could not reach GitHub: ${errMsg(e)}`;
  }
}

onMounted(refresh);
onActivated(refresh);
watch(
  () => kb.statsStale,
  (stale) => {
    if (stale) refresh();
  },
);

// -- Stat-tile popovers -------------------------------------------------------
//
// Clickable tiles (marked with the corner glyph) open a small animated overlay
// anchored beneath the tile. One popover at a time; outside click, Esc, or
// re-clicking the tile dismisses it.

type PopKind = "doc" | "relations";
const popover = ref<{ kind: PopKind; anchor: HTMLElement } | null>(null);

function toggle(kind: PopKind, e: Event) {
  const s = kb.stats;
  if (!s) return;
  if (kind === "relations" && !Number.isFinite(s.relations)) return;
  if (popover.value?.kind === kind) {
    popover.value = null;
    return;
  }
  popover.value = { kind, anchor: e.currentTarget as HTMLElement };
}

/** Sub-1% coverage still deserves a signal, not a rounded-to-zero 0%. */
function pctTxt(n: number | undefined, total: number): string {
  if (!n || !total) return "—";
  const p = (100 * n) / total;
  return p >= 1 ? `${Math.round(p)}%` : `${p.toFixed(1)}%`;
}

/** Union of both coverage kinds: many languages ship termFormat labels
 *  without a single documentation string (SUMO's German, French, ...). */
const docRows = computed<PopRow[]>(() => {
  const s = kb.stats;
  if (!s) return [];
  const docs = new Map<string, number>(
    (s.doc_languages ?? []).map((l: any) => [l.language, l.documented]),
  );
  const terms = new Map<string, number>(
    (s.term_languages ?? []).map((l: any) => [l.language, l.documented]),
  );
  const langs = [...new Set([...docs.keys(), ...terms.keys()])].sort(
    (a, b) =>
      (docs.get(b) ?? 0) - (docs.get(a) ?? 0) ||
      (terms.get(b) ?? 0) - (terms.get(a) ?? 0),
  );
  return langs.map((lang) => ({
    label: kb.langLabel(lang),
    value: pctTxt(terms.get(lang), s.symbols),
    extra: pctTxt(docs.get(lang), s.symbols),
    title: `docs: ${fmtNum(docs.get(lang) ?? 0)} · labels: ${fmtNum(terms.get(lang) ?? 0)} · of ${fmtNum(s.symbols)} symbols`,
  }));
});

const relationRows = computed<PopRow[]>(() => {
  const s = kb.stats;
  if (!s) return [];
  const other = Math.max(
    0,
    s.relations - (s.predicates ?? 0) - (s.functions ?? 0),
  );
  return [
    { label: "predicates", value: fmtNum(s.predicates ?? 0) },
    { label: "functions", value: fmtNum(s.functions ?? 0) },
    { label: "other relations", value: fmtNum(other) },
  ];
});
</script>

<template>
  <div class="stats">
    <div class="stat">
      <div class="stat-n">{{ files }}</div>
      <div class="stat-l">constituent files</div>
    </div>
    <div class="stat">
      <div class="stat-n">{{ symbols }}</div>
      <div class="stat-l">symbols</div>
    </div>
    <div class="stat">
      <div class="stat-n">{{ axioms }}</div>
      <div class="stat-l">axioms</div>
    </div>
    <div class="stat">
      <div class="stat-n">{{ rules }}</div>
      <div class="stat-l">rules</div>
    </div>
    <div class="stat">
      <div class="stat-n">{{ classes }}</div>
      <div class="stat-l">classes</div>
    </div>
    <div class="stat">
      <div class="stat-n">{{ instances }}</div>
      <div class="stat-l">instances</div>
    </div>
    <div
      class="stat clickable"
      title="Predicates vs functions"
      @click="toggle('relations', $event)"
    >
      <div class="stat-n">{{ relations }}</div>
      <div class="stat-l">relations</div>
    </div>
    <div
      class="stat clickable"
      :title="docTooltip"
      @click="toggle('doc', $event)"
    >
      <div class="stat-n">{{ doc }}</div>
      <div class="meter">
        <div :style="{ width: (docPct ?? 0) + '%' }"></div>
      </div>
      <div class="stat-l">symbols documented</div>
    </div>
    <div class="stat">
      <div class="stat-n stat-date" :title="commitTooltip">{{ commit }}</div>
      <div class="stat-l">last upstream commit</div>
    </div>
  </div>
  <div class="hint">{{ note }}</div>
  <StatPopover
    v-if="popover?.kind === 'doc'"
    :anchor="popover.anchor"
    title="Coverage by language"
    :rows="docRows"
    :header="{ label: 'language', value: 'labels', extra: 'docs' }"
    empty="no documentation or labels loaded"
    @close="popover = null"
  />
  <StatPopover
    v-else-if="popover?.kind === 'relations'"
    :anchor="popover.anchor"
    title="Relations"
    :rows="relationRows"
    @close="popover = null"
  />
</template>

<style scoped>
.stats {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
  gap: 10px;
  margin-bottom: 12px;
}
.stat {
  background: var(--card);
  border: 1px solid var(--line);
  border-radius: 10px;
  padding: 14px;
  text-align: center;
}
.stat-n {
  font-size: 24px;
  font-weight: 700;
  font-variant-numeric: tabular-nums;
}
.stat-n.stat-date {
  font-size: 16px;
}
.stat-l {
  font-size: 12px;
  color: var(--muted);
  margin-top: 3px;
}
/* Coverage meter inside a stat tile */
.stat .meter {
  height: 4px;
  background: var(--line);
  border-radius: 999px;
  overflow: hidden;
  margin: 6px 10px 0;
}
.stat .meter > div {
  height: 100%;
  width: 0%;
  background: var(--ok);
  border-radius: 999px;
  transition: width 0.4s ease;
}
/* Clickable tiles: pointer + a universal "more info" glyph top-right. */
.stat.clickable {
  cursor: pointer;
  position: relative;
  transition: border-color 0.15s ease;
}
.stat.clickable::after {
  content: "ⓘ";
  position: absolute;
  top: 5px;
  right: 8px;
  font-size: 12px;
  color: var(--muted);
  opacity: 0.6;
  transition:
    opacity 0.15s ease,
    color 0.15s ease;
}
.stat.clickable:hover {
  border-color: var(--accent);
}
.stat.clickable:hover::after {
  opacity: 1;
  color: var(--accent);
}
</style>
