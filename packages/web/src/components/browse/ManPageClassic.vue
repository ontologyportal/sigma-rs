<script setup lang="ts">
/** The classic SigmaKEE Browse.jsp look: a full-width white page where every
 *  fact about the symbol is a formula row (formula / source / paraphrase),
 *  grouped by the symbol's role in the sentence. */

import { computed, ref, watch } from "vue";
import { useSymbolLinks } from "../../composables/useSymbolLinks";
import { call } from "../../services/sigma";
import { useKBStore } from "../../stores/kb";
import { entriesForLanguage, linkifyDoc } from "../../utils/doc";
import { esc } from "../../utils/format";
import { highlightKif } from "../../utils/highlight-kif";
import Card from "../Card.vue";
import Disclosure from "../Disclosure.vue";
import SourceLoc from "../SourceLoc.vue";
import TaxonomyGraph from "./TaxonomyGraph.vue";
import WordNetEntry from "./WordNetEntry.vue";

const props = defineProps<{
  /** The worker's `manpage` payload, or null when the symbol has none. */
  page: any | null;
  /** The symbol asked for, named in the missing-page card. */
  symbol: string;
}>();

const emit = defineEmits<{ back: [] }>();

const kb = useKBStore();
const root = ref<HTMLElement | null>(null);
useSymbolLinks(root);

/** Rows per section before the "Show next" link. */
const PAGE_SIZE = 50;
/** Concurrent `renderNl` requests. */
const NL_BATCH = 8;

// -- Header ---------------------------------------------------------------------

const termFormat = computed<string>(() => {
  const entries: { language: string; text: string }[] =
    props.page?.term_format ?? [];
  return entriesForLanguage(entries, kb.uiLanguage)[0]?.text ?? "";
});

const taxOpen = ref(false);

/** The head table shows this many WordNet synsets as a compact word list;
 *  "show all" expands to the full glossed entries (a common class can have
 *  thousands). */
const WN_COMPACT = 12;
const wnAll = ref(false);
watch(
  () => props.page,
  () => {
    wnAll.value = false;
  },
);

// -- Sections -------------------------------------------------------------------

/** A reference row: the payload entry plus its index into `page.references`,
 *  which keys the paraphrase cache (a rule can sit in two sections). */
interface Row {
  idx: number;
  ref: any;
}

interface Section {
  id: string;
  label: string;
  rows: Row[];
}

const isRule = (r: any) => r.kind === "=>" || r.kind === "<=>";
const isArgKind = (r: any) =>
  r.kind === "fact" || r.kind === "doc" || r.kind === "taxonomy";

const sections = computed<Section[]>(() => {
  const refs: any[] = props.page?.references ?? [];
  const rows: Row[] = refs.map((ref, idx) => ({ idx, ref }));
  const byArg = new Map<number, Row[]>();
  for (const row of rows) {
    const r = row.ref;
    if (isArgKind(r) && r.arg_pos != null && r.arg_pos >= 1) {
      let list = byArg.get(r.arg_pos);
      if (!list) byArg.set(r.arg_pos, (list = []));
      list.push(row);
    }
  }
  const out: Section[] = [...byArg.keys()]
    .sort((a, b) => a - b)
    .map((n) => ({
      id: `arg${n}`,
      label: `appearance as argument number ${n}`,
      rows: byArg.get(n)!,
    }));
  const push = (id: string, label: string, pred: (r: any) => boolean) => {
    const list = rows.filter((row) => pred(row.ref));
    if (list.length) out.push({ id, label, rows: list });
  };
  push(
    "antecedent",
    "antecedent",
    (r) => isRule(r) && r.roles?.includes("antecedent"),
  );
  push(
    "consequent",
    "consequent",
    (r) => isRule(r) && r.roles?.includes("consequent"),
  );
  push(
    "statement",
    "statement",
    (r) =>
      r.kind === "and" ||
      r.kind === "or" ||
      r.kind === "other" ||
      (isRule(r) && !(r.roles?.length > 0)),
  );
  push("arg0", "appearance as argument number 0", (r) => r.arg_pos === 0);
  return out;
});

// -- Paging ---------------------------------------------------------------------

/** Rows revealed per section id; absent means the first page. */
const shown = ref<Record<string, number>>({});

const limit = (s: Section) => shown.value[s.id] ?? PAGE_SIZE;
const visibleRows = (s: Section) => s.rows.slice(0, limit(s));

function showMore(s: Section) {
  shown.value = { ...shown.value, [s.id]: limit(s) + PAGE_SIZE };
  fetchVisible();
}

// -- Paraphrases ----------------------------------------------------------------

/** Paraphrase HTML per reference index: "..." while pending, the rendered
 *  text when done, "" on failure; absent when not yet requested. */
const nl = ref<Record<number, string>>({});
let nlSeq = 0;
/** Reference indices scheduled under the current `nlSeq`. */
let requested = new Set<number>();

const docString = (kif: string): string | null => {
  const strings = kif.match(/"(?:[^"\\]|\\.)*"/g);
  if (!strings) return null;
  return strings[strings.length - 1].slice(1, -1).replace(/\\(.)/g, "$1");
};

const isDocRow = (r: any) => r.kind === "doc" && r.head === "documentation";

function cellHtml(row: Row): string {
  const r = row.ref;
  if (isDocRow(r)) {
    const s = docString(r.kif);
    return s == null ? "" : linkifyDoc(s);
  }
  return esc(nl.value[row.idx] ?? "");
}

const capitalize = (s: string) => (s ? s[0].toUpperCase() + s.slice(1) : s);

/** Request paraphrases for every visible, not-yet-requested non-doc row,
 *  section by section, `NL_BATCH` at a time. Results stamped with an older
 *  sequence (page or language changed meanwhile) are dropped. */
async function fetchVisible() {
  const mine = nlSeq;
  const language = kb.uiLanguage;
  const genericVars = kb.genericVars;
  const queue: Row[] = [];
  for (const s of sections.value) {
    for (const row of visibleRows(s)) {
      if (isDocRow(row.ref) || requested.has(row.idx)) continue;
      requested.add(row.idx);
      queue.push(row);
    }
  }
  if (!queue.length) return;
  const pending: Record<number, string> = {};
  for (const row of queue) pending[row.idx] = "…";
  nl.value = { ...nl.value, ...pending };

  const worker = async () => {
    for (;;) {
      const row = queue.shift();
      if (!row || mine !== nlSeq) return;
      let text: string;
      try {
        const res = await call("renderNl", {
          kif: row.ref.kif,
          language,
          genericVars,
        });
        text = capitalize((res?.text ?? "").trim());
      } catch {
        text = "";
      }
      if (mine !== nlSeq) return;
      nl.value = { ...nl.value, [row.idx]: text };
    }
  };
  await Promise.all(
    Array.from({ length: Math.min(NL_BATCH, queue.length) }, worker),
  );
}

function resetNl() {
  nlSeq += 1;
  requested = new Set();
  nl.value = {};
}

watch(
  () => props.page,
  () => {
    shown.value = {};
    taxOpen.value = false;
    resetNl();
    fetchVisible();
  },
  { immediate: true },
);
watch([() => kb.uiLanguage, () => kb.genericVars], () => {
  resetNl();
  fetchVisible();
});

const formulaHtml = (r: any) =>
  highlightKif(r.kif, {
    focusSymbol: props.page.name,
    linkSymbols: true,
  }).replace(/\n$/, "");
</script>

<template>
  <Card v-if="!page" class="hint"
    >No man page for <code>{{ symbol }}</code
    >.</Card
  >
  <div v-else ref="root" class="classic-root">
    <a class="hint back" @click.prevent="emit('back')">← back to results</a>
    <table class="classic head">
      <tbody>
        <tr>
          <td class="term-cell">
            <span class="term">{{ page.name }}</span>
            <span v-if="termFormat" class="tf">({{ termFormat }})</span>
          </td>
          <td class="wn-cell">
            <template v-if="page.wordnet?.length">
              <div class="hint wn-head">
                WordNet ({{ page.wordnet.length }})
                <a
                  v-if="page.wordnet.length > WN_COMPACT"
                  @click.prevent="wnAll = !wnAll"
                  >{{ wnAll ? "show fewer" : "show all" }}</a
                >
              </div>
              <template v-if="wnAll">
                <WordNetEntry
                  v-for="(m, i) in page.wordnet"
                  :key="i"
                  :entry="m"
                />
              </template>
              <div v-else class="wn-compact">
                <span
                  v-for="(m, i) in page.wordnet.slice(0, WN_COMPACT)"
                  :key="i"
                  class="wn-word"
                  >{{ m.words }} <span class="hint">({{ m.pos }})</span></span
                >
              </div>
            </template>
          </td>
        </tr>
      </tbody>
    </table>
    <Disclosure summary="taxonomy" @toggle="taxOpen = $event">
      <TaxonomyGraph v-if="taxOpen" :page="page" />
    </Disclosure>
    <template v-for="s in sections" :key="s.id">
      <div class="divider">{{ s.label }}</div>
      <table class="classic">
        <tbody>
          <tr v-for="row in visibleRows(s)" :key="row.idx">
            <td class="formula" v-html="formulaHtml(row.ref)"></td>
            <td class="source">
              <SourceLoc
                :file="row.ref.file"
                :line="row.ref.line"
                variant="ref"
              />
            </td>
            <td class="nl" v-html="cellHtml(row)"></td>
          </tr>
        </tbody>
      </table>
      <div v-if="s.rows.length > limit(s)" class="more">
        Display limited to {{ limit(s) }} items.
        <a class="show-more" @click.prevent="showMore(s)"
          >Show next {{ PAGE_SIZE }}</a
        >
      </div>
    </template>
  </div>
</template>

<style scoped>
.classic-root {
  --classic-bar: #a8bacf;
  --classic-cell: #b8cadf;
  font-family: Arial, Helvetica, sans-serif;
  font-size: 12px;
  line-height: 1.35;
}
@media (prefers-color-scheme: dark) {
  :global(:root:not([data-theme="light"])) .classic-root {
    --classic-bar: #4a5a6e;
    --classic-cell: #3a4757;
  }
}
:global(:root[data-theme="dark"]) .classic-root {
  --classic-bar: #4a5a6e;
  --classic-cell: #3a4757;
}
/* Break out of main's 900px gutter only when the viewport is wider than it. */
@media (min-width: 940px) {
  .classic-root {
    width: 95vw;
    margin-left: calc(50% - 47.5vw);
  }
}
.back {
  display: inline-block;
  margin-bottom: 8px;
  cursor: pointer;
}
table.classic {
  width: 100%;
  border-collapse: collapse;
  table-layout: fixed;
}
table.classic td {
  vertical-align: top;
  padding: 4px 6px;
  overflow-wrap: anywhere;
}
table.head {
  margin-bottom: 6px;
}
.term-cell {
  width: 60%;
}
.wn-head {
  margin-bottom: 4px;
}
.wn-head a {
  margin-left: 6px;
}
.wn-compact {
  font-size: 12px;
  line-height: 1.6;
}
.wn-word + .wn-word::before {
  content: " · ";
  color: var(--muted);
}
.wn-cell {
  width: 40%;
  font-size: 12px;
}
.term {
  font-size: 22px;
  font-weight: bold;
  margin-right: 6px;
}
.tf {
  font-size: 13px;
}
.divider {
  width: 50%;
  border-bottom: 2px solid var(--classic-bar);
  font-weight: bold;
  font-size: 13px;
  margin: 14px 0 4px;
  padding-bottom: 2px;
}
td.formula {
  width: 45%;
  white-space: pre;
  overflow-x: auto;
}
td.source {
  width: 17%;
  background: var(--classic-cell);
  font-size: 12px;
  overflow-wrap: normal;
  word-break: break-word;
}
td.source :deep(a),
td.source :deep(span) {
  display: block;
}
td.nl {
  width: 38%;
  font-size: 13px;
}
.more {
  margin: 4px 6px 8px;
  font-size: 12px;
}
.show-more {
  cursor: pointer;
}
:deep(.xref) {
  border-bottom: 1px dotted var(--accent);
}
</style>
