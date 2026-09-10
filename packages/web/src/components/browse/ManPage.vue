<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useSymbolLinks } from "../../composables/useSymbolLinks";
import { useKBStore } from "../../stores/kb";
import { esc } from "../../utils/format";
import Card from "../Card.vue";
import CiteRow from "../CiteRow.vue";
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

const subtab = ref<"overview" | "wordnet">("overview");
const filter = ref("");
watch(
  () => props.page,
  () => {
    subtab.value = "overview";
    filter.value = "";
  },
);

const hasWordnet = computed(() => (props.page?.wordnet?.length ?? 0) > 0);

/** Turn `&%Symbol` cross-reference markers in documentation text into man-page links. */
function linkifyDoc(text: unknown): string {
  return String(text)
    .split(/(&%[A-Za-z0-9_-]+)/)
    .map((part) => {
      const m = part.match(/^&%([A-Za-z0-9_-]+)$/);
      return m
        ? `<a class="open xref" data-sym="${esc(m[1])}">${esc(m[1])}</a>`
        : esc(part);
    })
    .join("");
}

/** Doc entries in the selected language, falling back to English then to all,
 *  so a symbol never renders blank just because it lacks the chosen language. */
function docsForLanguage(entries: any[]): any[] {
  const pick = (lang: string) => entries.filter((d) => d.language === lang);
  return pick(kb.uiLanguage).length
    ? pick(kb.uiLanguage)
    : pick("EnglishLanguage").length
      ? pick("EnglishLanguage")
      : entries;
}

const docs = (entries: any[]) =>
  docsForLanguage(entries).map((d) => ({
    html: linkifyDoc(d.text),
    language: d.language,
  }));

const hasSignature = computed(() => {
  const p = props.page;
  return !!p && (p.arity != null || p.domains.length > 0 || !!p.range);
});

/** The Signature block, one line per entry; `sym` is a class name rendered
 *  as the same man-page link `linkifyDoc` produces. */
const sigParts = computed<{ label: string; sym?: string; suffix?: string }[]>(
  () => {
    const p = props.page;
    if (!p) return [];
    const parts: { label: string; sym?: string; suffix?: string }[] = [];
    if (p.arity != null)
      parts.push({ label: `arity ${p.arity < 0 ? "variable" : p.arity}` });
    for (const d of p.domains) {
      parts.push({
        label: `arg ${d.position}: `,
        sym: d.sort.class,
        suffix: d.sort.subclass ? " (class)" : "",
      });
    }
    if (p.range)
      parts.push({
        label: `${p.range.subclass ? "rangeSubclass" : "range"}: `,
        sym: p.range.class,
      });
    return parts;
  },
);

// -- The reference list --------------------------------------------------------

const refsNote = computed(() => {
  const p = props.page;
  if (!p) return "";
  const shown = p.references.length;
  const omitted = Math.max(0, p.appears_in_count - shown);
  if (!shown) {
    return omitted
      ? `appears only in ${omitted} documentation/taxonomy/format entr${omitted === 1 ? "y" : "ies"} (not shown)`
      : "appears in no formulas";
  }
  const excl = omitted
    ? ` (${omitted} documentation/taxonomy/format entr${omitted === 1 ? "y" : "ies"} omitted)`
    : "";
  return `appears in ${shown} formula${shown === 1 ? "" : "s"}${excl}, listed below`;
});

/** Filter options, offering only the categories present among the refs.
 *  Value encoding: '' = all; 'fact' = any plain fact (a relation atom, possibly
 *  under `not`); 'fact:<n>' = plain fact with the symbol at argument n (0 = the
 *  relation itself); '=>' '<=>' 'and' 'or' = a top-level logical operator.
 *  `kind`/`arg_pos` come from the core classification in `manpage_to_js`. */
const filterOptions = computed<{ value: string; label: string }[]>(() => {
  const refs: any[] = props.page?.references ?? [];
  const facts = refs.filter((r) => r.kind === "fact");
  const positions = [
    ...new Set<number>(facts.map((r) => r.arg_pos).filter((n) => n != null)),
  ].sort((a, b) => a - b);
  const ops = [
    ["=>", "Implications (⇒)"],
    ["<=>", "Biconditionals (⇔)"],
    ["and", "Conjunctions (and)"],
    ["or", "Disjunctions (or)"],
  ].filter(([k]) => refs.some((r) => r.kind === k));
  const posLabel = (n: number) =>
    n === 0 ? "symbol as the relation (arg 0)" : `symbol as argument ${n}`;
  const count = (pred: (r: any) => boolean) => refs.filter(pred).length;
  const opts = [{ value: "", label: `All (${refs.length})` }];
  if (facts.length)
    opts.push({ value: "fact", label: `Plain facts (${facts.length})` });
  for (const n of positions) {
    opts.push({
      value: `fact:${n}`,
      label: `  ${posLabel(n)} (${count((r) => r.kind === "fact" && r.arg_pos === n)})`,
    });
  }
  for (const [k, label] of ops)
    opts.push({ value: k, label: `${label} (${count((r) => r.kind === k)})` });
  return opts;
});

/** Subset of `refs` matching an encoded filter value (see `filterOptions`). */
function filterRefs(refs: any[], f: string): any[] {
  if (!f) return refs;
  if (f === "fact") return refs.filter((r) => r.kind === "fact");
  if (f.startsWith("fact:")) {
    const n = Number(f.slice(5));
    return refs.filter((r) => r.kind === "fact" && r.arg_pos === n);
  }
  return refs.filter((r) => r.kind === f);
}

// Formulas from these files are shown first, in this order, then everything
// else in its existing relative order.
const FILE_SORT_PRIORITY = ["Merge.kif", "Mid-level-ontology.kif"];

function fileSortRank(file: string): number {
  const i = FILE_SORT_PRIORITY.indexOf(file);
  return i === -1 ? FILE_SORT_PRIORITY.length : i;
}

/** Stable sort of refs by source-file priority: Merge.kif, then MILO, then
 *  everything else. */
function sortRefsByFile(refs: any[]): any[] {
  return refs
    .map((r, i) => [r, i] as [any, number])
    .sort(
      ([a, ai], [b, bi]) =>
        fileSortRank(a.file) - fileSortRank(b.file) || ai - bi,
    )
    .map(([r]) => r);
}

const shownRefs = computed(() =>
  sortRefsByFile(filterRefs(props.page?.references ?? [], filter.value)),
);
</script>

<template>
  <Card v-if="!page" class="hint"
    >No man page for <code>{{ symbol }}</code
    >.</Card
  >
  <div v-else ref="root">
    <Card class="man">
      <div class="man-head">
        <a class="hint back" @click.prevent="emit('back')">← back to results</a>
        <h2>{{ page.name }}</h2>
        <div class="kinds">{{ page.kinds.join(" · ") || "symbol" }}</div>
        <div v-if="hasWordnet" class="man-subtabs" role="tablist">
          <button
            type="button"
            :aria-selected="subtab === 'overview'"
            @click="subtab = 'overview'"
          >
            Overview
          </button>
          <button
            type="button"
            :aria-selected="subtab === 'wordnet'"
            @click="subtab = 'wordnet'"
          >
            WordNet <span class="hint">({{ page.wordnet.length }})</span>
          </button>
        </div>
      </div>
      <div v-show="subtab === 'overview'">
        <div v-if="page.documentation.length" class="field">
          <h3>Documentation</h3>
          <div class="val">
            <div v-for="(d, i) in docs(page.documentation)" :key="i">
              <span v-html="d.html"></span>
              <span class="hint">({{ d.language }})</span>
            </div>
          </div>
        </div>
        <div class="field">
          <h3>Taxonomy</h3>
          <div class="val"><TaxonomyGraph :page="page" /></div>
        </div>
        <div v-if="hasSignature" class="field">
          <h3>Signature</h3>
          <div class="val">
            <template v-if="sigParts.length">
              <template v-for="(part, i) in sigParts" :key="i">
                <br v-if="i" />
                {{ part.label
                }}<a v-if="part.sym" class="open xref" :data-sym="part.sym">{{
                  part.sym
                }}</a
                >{{ part.suffix }}
              </template>
            </template>
            <span v-else class="hint">none declared</span>
          </div>
        </div>
        <div v-if="page.term_format.length" class="field">
          <h3>Term format</h3>
          <div class="val">
            <div v-for="(d, i) in docs(page.term_format)" :key="i">
              <span v-html="d.html"></span>
              <span class="hint">({{ d.language }})</span>
            </div>
          </div>
        </div>
        <div v-if="page.format.length" class="field">
          <h3>Format</h3>
          <div class="val">
            <div v-for="(d, i) in docs(page.format)" :key="i">
              <span v-html="d.html"></span>
              <span class="hint">({{ d.language }})</span>
            </div>
          </div>
        </div>
        <div class="field">
          <h3>References</h3>
          <div class="val">
            <template v-if="page.references.length">
              <div class="hint refs-note">{{ refsNote }}</div>
              <label class="ref-filter"
                ><span class="hint">Filter</span>
                <select v-model="filter">
                  <option
                    v-for="o in filterOptions"
                    :key="o.value"
                    :value="o.value"
                  >
                    {{ o.label }}
                  </option>
                </select></label
              >
              <ol v-if="shownRefs.length" class="refs">
                <CiteRow
                  v-for="(r, i) in shownRefs"
                  :key="`${r.file}:${r.line}:${i}`"
                  :kif="r.kif"
                  :file="r.file"
                  :line="r.line"
                  :focus-symbol="page.name"
                />
              </ol>
              <span v-else class="hint">no formulas match this filter</span>
            </template>
            <div v-else class="hint">{{ refsNote }}</div>
          </div>
        </div>
      </div>
      <div v-if="hasWordnet" v-show="subtab === 'wordnet'">
        <WordNetEntry v-for="(m, i) in page.wordnet" :key="i" :entry="m" />
      </div>
    </Card>
  </div>
</template>

<style scoped>
.man h2 {
  font-family: var(--mono);
  font-size: 20px;
  margin: 0 0 2px;
}
/* Sticky header: the symbol name + kinds stay pinned while long
   reference lists scroll underneath. */
.man-head {
  position: sticky;
  top: 0;
  z-index: 5;
  background: var(--card);
  margin: -14px -14px 8px;
  padding: 12px 14px 8px;
  border-bottom: 1px solid var(--line);
  border-radius: 10px 10px 0 0;
}
.man .field {
  margin: 10px 0;
}
/* Man page sub-tabs (Overview / WordNet) -- a lighter-weight local echo of
   nav.tabs, scoped inside the sticky man-head. */
.man-subtabs {
  display: flex;
  gap: 2px;
  margin-top: 8px;
}
.man-subtabs button {
  font: inherit;
  font-size: 13px;
  background: none;
  border: none;
  color: var(--muted);
  cursor: pointer;
  padding: 5px 10px;
  border-bottom: 2px solid transparent;
}
.man-subtabs button[aria-selected="true"] {
  color: var(--fg);
  border-bottom-color: var(--accent);
  font-weight: 600;
}
.man .field h3 {
  font-size: 11px;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: var(--muted);
  margin: 0 0 3px;
}
.man .field .val {
  font-size: 14px;
}
.man .tax code {
  display: inline-block;
  margin: 0 8px 4px 0;
}
.refs-note {
  margin-bottom: 4px;
}
label.ref-filter {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  margin: 2px 0 8px;
}
label.ref-filter select {
  font-size: 12px;
  padding: 2px 4px;
  border: 1px solid var(--line);
  border-radius: 6px;
  background: var(--bg);
  color: var(--fg);
  max-width: 320px;
}
:deep(.xref) {
  border-bottom: 1px dotted var(--accent);
}
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
</style>
