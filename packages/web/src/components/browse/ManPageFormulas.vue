<script setup lang="ts">
import { computed, ref, watch } from "vue";
import CiteRow from "../CiteRow.vue";
import { ManPage, ManPageRef } from "sigmakee/sdk";

const props = defineProps<{
  /** The worker's `manpage` payload; `references` lists every formula the symbol occurs in. */
  page: ManPage;
}>();

const filter = ref("");
watch(
  () => props.page,
  () => {
    filter.value = "";
  },
);

const refs = computed<ManPageRef[]>(() => props.page.references ?? []);

const note = computed(() => {
  const n = refs.value.length;
  return n
    ? `appears in ${n} formula${n === 1 ? "" : "s"}`
    : "appears in no formulas";
});

interface FilterOption {
  value: string;
  label: string;
}
interface FilterGroup {
  label: string;
  options: FilterOption[];
}

/** Filter options, offering only the categories present among the refs.
 *  Value encoding: '' = all; 'fact' = any plain fact (a relation atom, possibly
 *  under `not`); 'fact:<n>' = plain fact with the symbol at argument n (0 = the
 *  relation itself); '=>' '<=>' 'and' 'or' = a top-level logical operator;
 *  'doc' = documentation / format sentences; 'taxonomy' = subclass / instance /
 *  subrelation / subAttribute sentences. `kind`/`arg_pos` come from the core
 *  classification in `manpage_to_js`. Ungrouped entries are top-level options;
 *  `Rules` and `Plain facts` are `<optgroup>`s. */
const filterGroups = computed<(FilterOption | FilterGroup)[]>(() => {
  const all = refs.value;
  const count = (pred: (r: ManPageRef) => boolean) => all.filter(pred).length;
  const facts = all.filter((r) => r.kind === "fact");
  const positions = [
    ...new Set<number>(facts.map((r) => r.arg_pos).filter((n) => n != null)),
  ].sort((a, b) => a - b);
  const ops = [
    ["=>", "Implications (⇒)"],
    ["<=>", "Biconditionals (⇔)"],
    ["and", "Conjunctions (and)"],
    ["or", "Disjunctions (or)"],
  ].filter(([k]) => all.some((r) => r.kind === k));
  const posLabel = (n: number) =>
    n === 0 ? "symbol as the relation (arg 0)" : `symbol as argument ${n}`;

  const out: (FilterOption | FilterGroup)[] = [
    { value: "", label: `All (${all.length})` },
  ];
  if (ops.length)
    out.push({
      label: "Rules",
      options: ops.map(([k, label]) => ({
        value: k,
        label: `${label} (${count((r) => r.kind === k)})`,
      })),
    });
  if (facts.length)
    out.push({
      label: `Plain facts (${facts.length})`,
      options: [
        { value: "fact", label: `any position (${facts.length})` },
        ...positions.map((n) => ({
          value: `fact:${n}`,
          label: `${posLabel(n)} (${count((r) => r.kind === "fact" && r.arg_pos === n)})`,
        })),
      ],
    });
  const docs = count((r) => r.kind === "doc");
  if (docs)
    out.push({ value: "doc", label: `Documentation & formats (${docs})` });
  const tax = count((r) => r.kind === "taxonomy");
  if (tax) out.push({ value: "taxonomy", label: `Taxonomy (${tax})` });
  return out;
});

const isGroup = (o: FilterOption | FilterGroup): o is FilterGroup =>
  "options" in o;

/** Subset of `refs` matching an encoded filter value (see `filterGroups`). */
function filterRefs(list: ManPageRef[], f: string): ManPageRef[] {
  if (!f) return list;
  if (f === "fact") return list.filter((r) => r.kind === "fact");
  if (f.startsWith("fact:")) {
    const n = Number(f.slice(5));
    return list.filter((r) => r.kind === "fact" && r.arg_pos === n);
  }
  return list.filter((r) => r.kind === f);
}

// Formulas from these files are shown first, in this order, then everything
// else in its existing relative order.
const FILE_SORT_PRIORITY = ["Merge.kif", "Mid-level-ontology.kif"];

function fileSortRank(file: string | null): number {
  const i = file === null ? -1 : FILE_SORT_PRIORITY.indexOf(file);
  return i === -1 ? FILE_SORT_PRIORITY.length : i;
}

/** Stable sort of refs by source-file priority: Merge.kif, then MILO, then
 *  everything else. */
function sortRefsByFile(list: ManPageRef[]): ManPageRef[] {
  return list
    .map((r, i) => [r, i] as [ManPageRef, number])
    .sort(
      ([a, ai], [b, bi]) =>
        fileSortRank(a.file) - fileSortRank(b.file) || ai - bi,
    )
    .map(([r]) => r);
}

const shownRefs = computed(() =>
  sortRefsByFile(filterRefs(refs.value, filter.value)),
);
</script>

<template>
  <div class="formulas">
    <div class="hint refs-note">{{ note }}</div>
    <template v-if="refs.length">
      <label class="ref-filter"
        ><span class="hint">Filter</span>
        <select v-model="filter">
          <template
            v-for="o in filterGroups"
            :key="isGroup(o) ? o.label : o.value"
          >
            <optgroup v-if="isGroup(o)" :label="o.label">
              <option v-for="s in o.options" :key="s.value" :value="s.value">
                {{ s.label }}
              </option>
            </optgroup>
            <option v-else :value="o.value">{{ o.label }}</option>
          </template>
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
  </div>
</template>

<style scoped>
.formulas {
  font-size: 14px;
  margin: 10px 0;
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
