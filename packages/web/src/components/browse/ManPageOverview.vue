<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useKBStore } from "../../stores/kb";
import { entriesForLanguage, linkifyDoc } from "../../utils/doc";
import TaxonomyGraph from "./TaxonomyGraph.vue";
import TaxonomyList from "./TaxonomyList.vue";

const props = defineProps<{
  /** The worker's `manpage` payload. */
  page: any;
}>();

const kb = useKBStore();

// -- Taxonomy view -------------------------------------------------------------

const TAXONOMY_VIEW_KEY = "sumoBrowserTaxonomyView";
type TaxonomyView = "graph" | "list";

function storedTaxonomyView(): TaxonomyView {
  try {
    return localStorage.getItem(TAXONOMY_VIEW_KEY) === "list"
      ? "list"
      : "graph";
  } catch {
    return "graph";
  }
}

const taxonomyView = ref<TaxonomyView>(storedTaxonomyView());
watch(taxonomyView, (v) => {
  try {
    localStorage.setItem(TAXONOMY_VIEW_KEY, v);
  } catch {
    // Storage may be unavailable (private mode, quota); the toggle still works.
  }
});

// -- Documentation -------------------------------------------------------------

const docs = (entries: any[]) =>
  entriesForLanguage(entries, kb.uiLanguage).map((d) => ({
    html: linkifyDoc(d.text),
    language: d.language,
  }));

// -- Signature -----------------------------------------------------------------

const hasSignature = computed(() => {
  const p = props.page;
  return p.arity != null || p.domains.length > 0 || !!p.range;
});

/** The Signature block, one line per entry; `sym` is a class name rendered
 *  as the same man-page link `linkifyDoc` produces. */
const sigParts = computed<{ label: string; sym?: string; suffix?: string }[]>(
  () => {
    const p = props.page;
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
</script>

<template>
  <div class="overview">
    <div class="field">
      <div class="field-head">
        <h3>Taxonomy</h3>
        <div
          class="inline tight segments"
          role="group"
          aria-label="Taxonomy view"
        >
          <button
            type="button"
            class="btn ghost"
            :aria-pressed="taxonomyView === 'graph'"
            @click="taxonomyView = 'graph'"
          >
            Graph
          </button>
          <button
            type="button"
            class="btn ghost"
            :aria-pressed="taxonomyView === 'list'"
            @click="taxonomyView = 'list'"
          >
            List
          </button>
        </div>
      </div>
      <div class="val">
        <TaxonomyGraph v-if="taxonomyView === 'graph'" :page="page" />
        <TaxonomyList v-else :page="page" />
      </div>
    </div>
    <div v-if="page.documentation.length" class="field">
      <h3>Documentation</h3>
      <div class="val">
        <div v-for="(d, i) in docs(page.documentation)" :key="i">
          <span v-html="d.html"></span>
          <span class="hint">({{ d.language }})</span>
        </div>
      </div>
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
  </div>
</template>

<style scoped>
.field {
  margin: 10px 0;
}
.field h3 {
  font-size: 11px;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: var(--muted);
  margin: 0 0 3px;
}
.field .val {
  font-size: 14px;
}
.field-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}
.segments > button {
  height: 28px;
  padding: 0 10px;
  font-size: 12px;
}
.segments > button[aria-pressed="true"] {
  color: var(--accent);
  border-color: var(--accent);
}
:deep(.xref) {
  border-bottom: 1px dotted var(--accent);
}
</style>
