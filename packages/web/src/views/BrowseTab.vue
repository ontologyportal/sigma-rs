<script setup lang="ts">
/** Browse (Home) tab: search -> results -> man page. The route query is the
 *  source of truth: `?sym=` shows a man page, `?q=` search results, neither
 *  the welcome card + KB stats. User actions only rewrite the query; the
 *  query handler does the work. */

import {
  computed,
  nextTick,
  onActivated,
  onBeforeUnmount,
  onDeactivated,
  ref,
  shallowRef,
  watch,
} from "vue";
import { updateParams } from "../router";
import { call } from "../services/sigma";
import { useKBStore } from "../stores/kb";
import { useShellStore } from "../stores/shell";
import { useTabQuery } from "../composables/useTabQuery";
import { errMsg } from "../utils/format";
import Card from "../components/Card.vue";
import Disclosure from "../components/Disclosure.vue";
import HomeStats from "../components/browse/HomeStats.vue";
import ManPage from "../components/browse/ManPage.vue";
import SearchResults from "../components/browse/SearchResults.vue";

const kb = useKBStore();
const shell = useShellStore();
const { query, onQuery, str } = useTabQuery(["home", "browse"]);

const q = computed(() => str(query.value.q).trim());
const sym = computed(() => str(query.value.sym));

const inputEl = ref<HTMLInputElement | null>(null);
const input = ref("");
const wordnetOnly = ref(false);
const hits = shallowRef<any[] | null>(null);
const langNote = ref("");
const searchError = ref("");
const page = shallowRef<any | null>(null);
const pageMissing = ref("");
const active = ref(false);

// -- Search --------------------------------------------------------------------

// Only the newest in-flight search may render: typing "proc" fires four
// requests and the slower ones must not clobber the freshest results.
let searchSeq = 0;
/** What the current `hits` answer, so the same query arriving twice does the work once. */
let lastSearch: {
  query: string;
  wordnetOnly: boolean;
  language: string;
} | null = null;

function clearResults() {
  searchSeq += 1;
  lastSearch = null;
  hits.value = null;
  langNote.value = "";
  searchError.value = "";
}

async function runSearch(text: string) {
  if (!text) {
    clearResults();
    return;
  }
  const seq = ++searchSeq;
  const language = kb.uiLanguage;
  const opts = { wordnetOnly: wordnetOnly.value };
  lastSearch = { query: text, language, ...opts };
  searchError.value = "";
  try {
    let { hits: found } = await call("search", {
      query: text,
      limit: 100,
      language,
      ...opts,
    });
    if (seq !== searchSeq) return;
    // The language filter is a preference, not a wall: a KB documented in
    // another language must stay searchable on the default (English) setting.
    let note = "";
    if (found.length === 0) {
      ({ hits: found } = await call("search", {
        query: text,
        limit: 100,
        ...opts,
      }));
      if (seq !== searchSeq) return;
      if (found.length)
        note = ` (no ${kb.langLabel(language)} matches — showing all languages)`;
    }
    hits.value = found;
    langNote.value = note;
  } catch (e) {
    if (seq !== searchSeq) return;
    lastSearch = null;
    hits.value = [];
    searchError.value = errMsg(e);
  }
}

function searchIsCurrent(text: string): boolean {
  const l = lastSearch;
  return (
    !!l &&
    l.query === text &&
    l.wordnetOnly === wordnetOnly.value &&
    l.language === kb.uiLanguage
  );
}

// -- Man page ------------------------------------------------------------------

let pageSeq = 0;
/** The symbol the shown (or missing) man page is for. */
let shownSym = "";

function clearPage() {
  pageSeq += 1;
  shownSym = "";
  page.value = null;
  pageMissing.value = "";
}

async function openManPage(symbol: string) {
  const seq = ++pageSeq;
  shownSym = symbol;
  try {
    const { page: p } = await call("manpage", { symbol });
    if (seq !== pageSeq) return;
    page.value = p ?? null;
    pageMissing.value = p ? "" : symbol;
  } catch {
    if (seq !== pageSeq) return;
    shownSym = "";
    page.value = null;
    pageMissing.value = symbol;
  }
}

// -- Query -> view --------------------------------------------------------------

let debounce = 0;

/** Bring the view in line with the query. Idempotent: whatever is already
 *  showing for this query is left alone. */
function sync() {
  if (sym.value) {
    if (sym.value !== shownSym) openManPage(sym.value);
    return;
  }
  if (shownSym) clearPage();
  if (!debounce && input.value.trim() !== q.value) input.value = q.value;
  if (q.value) {
    if (!searchIsCurrent(q.value)) runSearch(q.value);
  } else {
    clearResults();
  }
}

onQuery(sync);
onActivated(() => {
  active.value = true;
  sync();
  document.addEventListener("keydown", onKeydown);
});
onDeactivated(() => {
  active.value = false;
  document.removeEventListener("keydown", onKeydown);
});
onBeforeUnmount(() => document.removeEventListener("keydown", onKeydown));

// Search hits depend on the language; the man page's documentation follows
// it through ManPage's own computed, so only the search is re-run.
watch(
  () => kb.uiLanguage,
  () => {
    if (active.value && q.value && !sym.value) runSearch(q.value);
  },
);

// -- User actions ---------------------------------------------------------------

/** Search-as-you-type: the URL updates off a short debounce, and the query
 *  watcher runs the search; Enter (submit) skips the wait. */
function onInput() {
  clearTimeout(debounce);
  debounce = window.setTimeout(() => {
    debounce = 0;
    updateParams({ q: input.value.trim() });
  }, 150);
}

function onSubmit() {
  clearTimeout(debounce);
  debounce = 0;
  updateParams({ q: input.value.trim() });
}

function onClear() {
  clearTimeout(debounce);
  debounce = 0;
  input.value = "";
  updateParams({});
  inputEl.value?.focus();
}

/** The WordNet-only toggle is not in the URL, so it re-runs the search directly. */
function onWordnetOnlyChange() {
  if (q.value && !sym.value) runSearch(q.value);
}

function tryQuery(text: string) {
  input.value = text;
  updateParams({ q: text });
}

/** Leave the man page for the results (or the welcome state). A man page
 *  reached from a result carries no `q`, so the search box's text stands in. */
function backToResults() {
  updateParams({ q: q.value || input.value.trim() });
}

function onKeydown(e: KeyboardEvent) {
  if (e.key !== "Escape") return;
  const t = e.target;
  const typing =
    t instanceof HTMLInputElement ||
    t instanceof HTMLTextAreaElement ||
    t instanceof HTMLSelectElement ||
    (t instanceof HTMLElement && t.isContentEditable);
  if (typing && t !== inputEl.value) return;
  if (sym.value) backToResults();
  else if (t === inputEl.value && input.value) onClear();
}

// The request can arrive while this view is still deactivated, or before it
// exists at all (the navigation to Browse is in flight), so activation
// honours any request newer than the last one handled.
let handledFocus = 0;
async function focusSearch() {
  handledFocus = shell.searchFocusRequest;
  await nextTick();
  inputEl.value?.focus();
  inputEl.value?.select();
}
watch(
  () => shell.searchFocusRequest,
  () => {
    if (active.value) focusSearch();
  },
);
onActivated(() => {
  if (shell.searchFocusRequest > handledFocus) focusSearch();
});
</script>

<template>
  <Card>
    <form @submit.prevent="onSubmit">
      <label for="q"
        >Search symbols &amp; documentation
        <span class="hint kbd-hint">(press <kbd>/</kbd> anywhere)</span></label
      >
      <div class="search-box">
        <input
          id="q"
          ref="inputEl"
          v-model="input"
          type="search"
          placeholder="e.g. Human, Process, part…"
          autocomplete="off"
          @input="onInput"
        />
        <button
          v-show="input"
          type="button"
          class="search-clear"
          aria-label="Clear search"
          @click="onClear"
        >
          &times;
        </button>
      </div>
    </form>
    <Disclosure summary="Advanced search">
      <label class="check wordnet-only">
        <input
          v-model="wordnetOnly"
          type="checkbox"
          @change="onWordnetOnlyChange"
        />
        WordNet only
        <span class="hint"
          >— show only WordNet synonym-expansion hits (requires the WordNet
          lexicon; see the
          <router-link :to="{ name: 'kb' }">Knowledge base</router-link>
          tab)</span
        >
      </label>
    </Disclosure>
  </Card>

  <ManPage
    v-if="page || pageMissing"
    :page="page"
    :symbol="pageMissing || page.name"
    @back="backToResults"
  />
  <template v-else-if="q || sym">
    <Card v-if="searchError" class="hint"
      >Search failed: {{ searchError }}</Card
    >
    <Card v-else-if="hits && !hits.length" class="hint"
      >No matches for <code>{{ q }}</code
      ><template v-if="wordnetOnly">
        (WordNet only — try unchecking Advanced search, or confirm the lexicon
        is enabled under
        <router-link :to="{ name: 'kb' }">Knowledge base</router-link
        >)</template
      >.</Card
    >
    <SearchResults
      v-else-if="hits"
      :hits="hits"
      :query="q"
      :lang-note="langNote"
    />
  </template>
  <template v-else>
    <Card>
      <h2 class="welcome-h">SUMO in your browser</h2>
      <p class="hint welcome-p">
        The <code>sigmakee-rs</code> native prover compiled to WebAssembly.
        Search above to explore the ontology — try
        <a class="try-q" @click.prevent="tryQuery('Human')">Human</a>,
        <a class="try-q" @click.prevent="tryQuery('Process')">Process</a> or
        <a class="try-q" @click.prevent="tryQuery('part')">part</a> — manage
        what is loaded under
        <router-link :to="{ name: 'kb' }">Knowledge base</router-link>, prove
        things under
        <router-link :to="{ name: 'prover' }">Ask/Tell</router-link>, and edit
        or contribute changes under
        <router-link :to="{ name: 'edit' }">Edit</router-link>. Everything runs
        locally: <strong>no server</strong>.
      </p>
    </Card>
    <HomeStats />
  </template>
</template>

<style scoped>
.search-box {
  position: relative;
}
.search-box input[type="search"] {
  padding-right: 30px;
}
.search-clear {
  position: absolute;
  top: 50%;
  right: 4px;
  transform: translateY(-50%);
  width: 24px;
  height: 24px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: none;
  border: none;
  border-radius: 6px;
  padding: 0;
  font-size: 18px;
  line-height: 1;
  color: var(--muted);
  cursor: pointer;
}
.search-clear:hover {
  color: var(--fg);
  background: var(--card);
}
.kbd-hint {
  font-size: 11px;
}
label.check.wordnet-only {
  margin-top: 8px;
}
/* Home: welcome + KB summary */
.welcome-h {
  font-size: 17px;
  margin: 0;
}
.welcome-p {
  margin: 6px 0 0;
}
</style>
