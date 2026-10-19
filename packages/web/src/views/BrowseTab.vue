<script setup lang="ts">
/** Browse (Home) tab: search -> results -> man page. The route query is the
 *  source of truth: `?sym=` shows a man page, `?q=` search results (a `q`
 *  starting with `(` is a formula, answered with the axioms matching it),
 *  neither the welcome card + KB stats. User actions only rewrite the query;
 *  the query handler does the work. */

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
import { navigate, updateParams } from "../router";
import { call } from "../services/sigma";
import { useKBStore } from "../stores/kb";
import { useShellStore } from "../stores/shell";
import { useTabQuery } from "../composables/useTabQuery";
import { errMsg } from "../utils/format";
import {
  acceptCompletion,
  closeParen,
  deletePair,
  type Edit,
  isBlankFormula,
  isFormula,
  openParen,
  parensBalanced,
  rankCompletions,
  termToFormula,
  wordAt,
} from "../utils/kifInput";
import Card from "../components/Card.vue";
import CiteRow from "../components/CiteRow.vue";
import Disclosure from "../components/Disclosure.vue";
import HomeStats from "../components/browse/HomeStats.vue";
import ManPage from "../components/browse/ManPage.vue";
import SearchResults from "../components/browse/SearchResults.vue";
import { AuditFocus, ManPage as ManPageType, SearchHit } from "sigmakee/sdk";

const kb = useKBStore();
const shell = useShellStore();
const { query, onQuery, str } = useTabQuery(["browse"]);

const q = computed(() => str(query.value.q).trim());
const sym = computed(() => str(query.value.sym));
const view = computed(() => str(query.value.view));

const inputEl = ref<HTMLInputElement | null>(null);
const input = ref("");
const wordnetOnly = ref(false);
const hits = shallowRef<SearchHit[] | null>(null);
/** Keyboard-highlighted result row, -1 for none. */
const selected = ref(-1);
const langNote = ref("");
const searchError = ref("");
const page = shallowRef<ManPageType | null>(null);
const pageMissing = ref("");
const active = ref(false);

type Mode = "term" | "axiom";
const MODES: { value: Mode; label: string }[] = [
  { value: "term", label: "Term" },
  { value: "axiom", label: "Axiom" },
];
/** The search box holds a formula exactly when its text starts with `(`. */
const mode = computed<Mode>(() => (isFormula(input.value) ? "axiom" : "term"));
/** Axioms matching the formula query; `null` before the first result. */
const axioms = shallowRef<AuditFocus[] | null>(null);
const matchError = ref("");
/** Cap on rendered matches; a general formula can match thousands. */
const MAX_SHOWN_AXIOMS = 200;
const shownAxioms = computed(() =>
  (axioms.value ?? []).slice(0, MAX_SHOWN_AXIOMS),
);

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
  selected.value = -1;
  langNote.value = "";
  searchError.value = "";
  axioms.value = null;
  matchError.value = "";
}

async function runSearch(text: string) {
  if (!text) {
    clearResults();
    return;
  }
  if (isFormula(text)) {
    runMatch(text);
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
    axioms.value = null;
    matchError.value = "";
    selected.value = -1;
    langNote.value = note;
  } catch (e) {
    if (seq !== searchSeq) return;
    lastSearch = null;
    hits.value = [];
    selected.value = -1;
    searchError.value = errMsg(e);
  }
}

async function runMatch(text: string) {
  const seq = ++searchSeq;
  lastSearch = {
    query: text,
    language: kb.uiLanguage,
    wordnetOnly: wordnetOnly.value,
  };
  hits.value = null;
  searchError.value = "";
  matchError.value = "";
  if (isBlankFormula(text)) {
    axioms.value = null;
    return;
  }
  try {
    const { axioms: found } = await call("matchingAxioms", { formula: text });
    if (seq !== searchSeq) return;
    axioms.value = found;
  } catch (e) {
    if (seq !== searchSeq) return;
    lastSearch = null;
    axioms.value = null;
    matchError.value = errMsg(e);
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
watch(view, () => {
  if (sym.value && view.value === "wordnet") openManPage(sym.value);
});
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
    if (active.value && q.value && !sym.value && !isFormula(q.value))
      runSearch(q.value);
  },
);

// -- User actions ---------------------------------------------------------------

/** Search-as-you-type: the URL updates off a short debounce, and the query
 *  watcher runs the search; Enter (submit) skips the wait. */
function onInput(suggest = true) {
  if (suggest && mode.value === "axiom") updateSuggestions();
  else closeSuggestions();
  scheduleRun();
}

/** Debounce pushing the box's text to `?q=`. A formula waits longer and is
 *  held while its parens are unbalanced or a word is mid-completion. */
function scheduleRun() {
  clearTimeout(debounce);
  debounce = window.setTimeout(
    () => {
      debounce = 0;
      const text = input.value.trim();
      if (isFormula(text) && (!parensBalanced(text) || completing())) return;
      updateParams({ q: text });
    },
    isFormula(input.value) ? 600 : 150,
  );
}

/** Put `edit` in the box, then run the usual input handling at its caret. */
function applyEdit(edit: Edit, suggest = true) {
  input.value = edit.text;
  nextTick(() => {
    inputEl.value?.setSelectionRange(edit.caret, edit.caret);
    onInput(suggest);
  });
}

/** Editor-style parens: `(` pairs (and in Term mode turns the box into a
 *  formula), `)` steps over a closing paren, Backspace deletes an empty pair. */
function onInputKeydown(e: KeyboardEvent) {
  const el = inputEl.value;
  if (!el || e.ctrlKey || e.metaKey || e.altKey) return;
  const text = input.value;
  const start = el.selectionStart ?? text.length;
  const end = el.selectionEnd ?? start;
  let edit: Edit | null = null;
  if (e.key === "(")
    edit =
      mode.value === "term" ? termToFormula(text) : openParen(text, start, end);
  else if (mode.value === "axiom" && start === end) {
    if (e.key === ")") edit = closeParen(text, start);
    else if (e.key === "Backspace") edit = deletePair(text, start);
  }
  if (!edit) return;
  e.preventDefault();
  applyEdit(edit);
}

/** The Term/Axiom switch: Axiom starts an empty `()` formula, Term an empty search. */
function setMode(next: Mode) {
  if (next === mode.value) return;
  if (next === "axiom") setModeText("()", { q: "()" });
  else setModeText("", {});
}

// -- Term completion (Axiom mode) ---------------------------------------------

const suggestions = ref<string[]>([]);
const suggestSel = ref(0);
let suggestSeq = 0;
let suggestPending = false;
/** The word the shown suggestions would replace. */
let suggestRange: { start: number; end: number; word: string } | null = null;

function closeSuggestions() {
  suggestSeq += 1;
  suggestPending = false;
  suggestions.value = [];
  suggestRange = null;
}

/** A completion is in flight, or open for a word that is not yet a symbol. */
function completing(): boolean {
  return (
    suggestPending ||
    (!!suggestRange && !suggestions.value.includes(suggestRange.word))
  );
}

/** Complete the symbol word at the caret from `search`. */
async function updateSuggestions() {
  const el = inputEl.value;
  const text = input.value;
  const word = el ? wordAt(text, el.selectionStart ?? text.length) : null;
  if (!word) {
    closeSuggestions();
    return;
  }
  const seq = ++suggestSeq;
  suggestPending = true;
  try {
    const { hits: found } = await call("search", {
      query: word.word,
      limit: 50,
    });
    if (seq !== suggestSeq) return;
    const ranked = rankCompletions(
      found.map((h) => h.symbol),
      word.word,
    );
    if (!ranked.length || (ranked.length === 1 && ranked[0] === word.word))
      closeSuggestions();
    else {
      suggestPending = false;
      suggestions.value = ranked;
      suggestRange = word;
      suggestSel.value = 0;
    }
  } catch {
    if (seq !== suggestSeq) return;
    closeSuggestions();
  }
  scheduleRun();
}

function acceptSuggestion(i: number) {
  const symbol = suggestions.value[i];
  const range = suggestRange;
  closeSuggestions();
  if (!symbol || !range) return;
  applyEdit(
    acceptCompletion(input.value, range.start, range.end, symbol),
    false,
  );
}

/** Results are showing for the search box's text (not a man page). */
function resultsShowing(): boolean {
  return (
    !sym.value &&
    !isFormula(input.value) &&
    searchIsCurrent(input.value.trim()) &&
    !!hits.value?.length
  );
}

/** Open the highlighted result, or the first when none is highlighted. */
function openSelected() {
  const list = hits.value;
  if (!list?.length) return;
  const hit = list[Math.max(selected.value, 0)];
  navigate("browse", { q: q.value, sym: hit.symbol });
}

function moveSelection(delta: number) {
  const n = hits.value?.length ?? 0;
  if (!n) return;
  selected.value = Math.min(n - 1, Math.max(0, selected.value + delta));
}

/** Enter: open a result when this text's results are already showing,
 *  otherwise run the search without waiting for the debounce. */
function onSubmit() {
  clearTimeout(debounce);
  debounce = 0;
  if (resultsShowing()) openSelected();
  else updateParams({ q: input.value.trim() });
}

/** Empty the box, staying in the current mode. */
function onClear() {
  if (mode.value === "axiom") {
    setModeText("()", { q: "()" });
    return;
  }
  setModeText("", {});
}

function setModeText(text: string, params: { q?: string }) {
  clearTimeout(debounce);
  debounce = 0;
  closeSuggestions();
  input.value = text;
  updateParams(params);
  nextTick(() => {
    inputEl.value?.focus();
    if (text === "()") inputEl.value?.setSelectionRange(1, 1);
  });
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
  const t = e.target;
  const typing =
    t instanceof HTMLInputElement ||
    t instanceof HTMLTextAreaElement ||
    t instanceof HTMLSelectElement ||
    (t instanceof HTMLElement && t.isContentEditable);
  if (typing && t !== inputEl.value) return;
  if (t === inputEl.value && suggestions.value.length) {
    const n = suggestions.value.length;
    switch (e.key) {
      case "ArrowDown":
      case "ArrowUp":
        e.preventDefault();
        suggestSel.value =
          (suggestSel.value + (e.key === "ArrowDown" ? 1 : n - 1)) % n;
        return;
      case "Enter":
      case "Tab":
        e.preventDefault();
        acceptSuggestion(suggestSel.value);
        return;
      case "Escape":
        e.preventDefault();
        onInput(false);
        return;
    }
  }
  switch (e.key) {
    case "Escape":
      if (sym.value) backToResults();
      else if (t === inputEl.value && input.value) {
        e.preventDefault();
        onClear();
      }
      break;
    case "ArrowDown":
    case "ArrowUp":
      if (!resultsShowing()) return;
      e.preventDefault();
      moveSelection(e.key === "ArrowDown" ? 1 : -1);
      break;
    case "Enter":
      // In the search box Enter is the form submit; elsewhere it opens the
      // highlighted result the same way.
      if (t === inputEl.value || !resultsShowing()) return;
      e.preventDefault();
      openSelected();
      break;
  }
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
        >{{
          mode === "axiom"
            ? "Find axioms matching a formula"
            : "Search symbols & documentation"
        }}
        <span class="hint kbd-hint">(press <kbd>/</kbd> anywhere)</span></label
      >
      <div class="search-box" :class="{ formula: mode === 'axiom' }">
        <div class="mode-seg seg" role="radiogroup" aria-label="Search mode">
          <button
            v-for="m in MODES"
            :key="m.value"
            type="button"
            role="radio"
            :aria-checked="mode === m.value"
            @click="setMode(m.value)"
          >
            <span v-if="m.value === 'axiom'" class="paren-glyph">()</span>
            {{ m.label }}
          </button>
        </div>
        <div class="search-input">
          <input
            id="q"
            ref="inputEl"
            v-model="input"
            type="search"
            :placeholder="
              mode === 'axiom'
                ? 'e.g. (instance ?X Human)'
                : 'e.g. Human, Process, part…'
            "
            autocomplete="off"
            :spellcheck="mode !== 'axiom'"
            role="combobox"
            aria-autocomplete="list"
            aria-controls="q-suggest"
            :aria-expanded="suggestions.length > 0"
            @input="onInput()"
            @keydown="onInputKeydown"
            @blur="closeSuggestions"
          />
          <ul
            v-if="suggestions.length"
            id="q-suggest"
            class="suggest"
            role="listbox"
          >
            <li
              v-for="(s, i) in suggestions"
              :key="s"
              role="option"
              :aria-selected="i === suggestSel"
              @mousedown.prevent="acceptSuggestion(i)"
            >
              {{ s }}
            </li>
          </ul>
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
      </div>
    </form>
    <Disclosure v-if="mode === 'term'" summary="Advanced search">
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
    :symbol="pageMissing || page?.name || ''"
    :view="view"
    @back="backToResults"
    @update:view="(v) => updateParams({ q, sym, view: v })"
  />
  <template v-else-if="isFormula(q)">
    <Card v-if="matchError" class="match-error">
      <strong>Could not parse formula</strong>
      <pre>{{ matchError }}</pre>
    </Card>
    <Card v-else-if="axioms && !axioms.length" class="hint"
      >No axioms match <code>{{ q }}</code
      >.</Card
    >
    <Card v-else-if="axioms">
      <div class="hint">
        {{ axioms.length }} matching axiom{{ axioms.length === 1 ? "" : "s"
        }}<template v-if="axioms.length > MAX_SHOWN_AXIOMS">
          (showing the first {{ MAX_SHOWN_AXIOMS }})</template
        >
      </div>
      <ol class="refs">
        <CiteRow
          v-for="(a, i) in shownAxioms"
          :key="`${a.file}:${a.line}:${i}`"
          :kif="a.kif"
          :file="a.file"
          :line="a.line"
        />
      </ol>
    </Card>
    <Card v-else class="hint"
      >Type a formula: axioms with a literal that unifies with one of yours are
      listed, whatever its sign. Variables (<code>?X</code>) match
      anything.</Card
    >
  </template>
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
      :selected="selected"
      :lang-note="langNote"
    />
  </template>
  <template v-else>
    <Card>
      <h2 class="welcome-h">SUMO in your browser</h2>
      <p class="hint welcome-p">
        The <code>Sigma Knowledge Engineering Environment (SigmaKEE)</code>
        for the Suggested Upper Merged Ontology (SUMO). Search above to explore
        the ontology — try
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
/* The mode switch sits inside the input's border, so the box owns the
   border and focus ring and the input itself is bare. */
.search-box {
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 3px;
  background: var(--bg);
  border: 1px solid var(--line);
  border-radius: 9px;
}
.search-box:focus-within {
  border-color: var(--accent);
  box-shadow: 0 0 0 1px var(--accent);
}
.mode-seg {
  display: flex;
  flex: none;
  border-radius: 6px;
}
.mode-seg button {
  border-radius: 4px;
  padding: 4px 10px;
}
.paren-glyph {
  font-family: var(--mono);
  font-weight: 400;
  letter-spacing: -0.5px;
}
.search-input {
  position: relative;
  flex: 1;
  min-width: 0;
}
.search-input input[type="search"] {
  border: none;
  outline: none;
  background: none;
  padding: 5px 30px 5px 6px;
}
.search-box.formula input[type="search"] {
  font-family: var(--mono);
  font-size: 13px;
}
/* Phones: the switch is a full-width bar above a separately bordered input. */
@media (max-width: 620px) {
  .search-box {
    flex-direction: column;
    align-items: stretch;
    gap: 8px;
    padding: 0;
    background: none;
    border: none;
  }
  .search-box:focus-within {
    box-shadow: none;
  }
  .mode-seg button {
    flex: 1;
    justify-content: center;
    padding: 6px 10px;
  }
  .search-input {
    padding: 3px;
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: 9px;
  }
  .search-input:focus-within {
    border-color: var(--accent);
    box-shadow: 0 0 0 1px var(--accent);
  }
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
.suggest {
  position: absolute;
  top: 100%;
  left: 0;
  z-index: 10;
  min-width: 220px;
  max-width: 100%;
  margin: 8px 0 0;
  padding: 4px;
  list-style: none;
  background: var(--bg);
  border: 1px solid var(--line);
  border-radius: 8px;
  box-shadow: 0 4px 14px rgb(0 0 0 / 0.12);
}
.suggest li {
  padding: 4px 8px;
  border-radius: 6px;
  font-family: var(--mono, monospace);
  font-size: 13px;
  cursor: pointer;
}
.suggest li[aria-selected="true"],
.suggest li:hover {
  background: color-mix(in srgb, var(--accent) 14%, transparent);
  color: var(--accent);
}
.match-error {
  color: var(--bad);
}
.match-error pre {
  margin: 6px 0 0;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}
ol.refs {
  list-style: none;
  margin: 8px 0 0;
  padding: 0;
}
ol.refs li {
  padding: 8px 2px;
  border-bottom: 1px solid var(--line);
}
ol.refs li:last-child {
  border-bottom: none;
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
