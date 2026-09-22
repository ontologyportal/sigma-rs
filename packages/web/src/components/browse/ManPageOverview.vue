<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useKBStore } from "../../stores/kb";
import { entriesForLanguage, linkifyDoc } from "../../utils/doc";
import TaxonomyGraph from "./TaxonomyGraph.vue";
import TaxonomyList from "./TaxonomyList.vue";
import { DocBlock, ManPage } from "sigmakee/sdk";
import RelationSignature from "./RelationSignature.vue";

const props = defineProps<{
  /** The worker's `manpage` payload. */
  page: ManPage;
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

const docs = (entries: DocBlock[]) =>
  entriesForLanguage(entries, kb.uiLanguage, kb.symbols.defaultLanguage).map(
    (d) => ({
      html: linkifyDoc(d.text),
      language: d.language,
    }),
  );

// -- Signature -----------------------------------------------------------------

const hasSignature = computed(() => {
  const p = props.page;
  return p.arity != null || p.domains.length > 0 || !!p.range;
});

/** The `format` string in the UI language, for the signature's example line. */
const formatString = computed(
  () =>
    entriesForLanguage(
      props.page.format,
      kb.uiLanguage,
      kb.symbols.defaultLanguage,
    )[0]?.text,
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
        <RelationSignature
          :name="page.name"
          :kinds="page.kinds"
          :args="page.domains"
          :range="page.range ?? undefined"
          :variable-arity="page.arity != null && page.arity < 0"
          :format="formatString"
        />
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
