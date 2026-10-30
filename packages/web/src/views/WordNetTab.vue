<script setup lang="ts">
/**
 * The WordNet mapping editor: look up a word sense (by word, synset id, or the
 * SUMO term it maps to), read its definition and synonyms, and change which
 * SUMO terms it is anchored to. Definitions and synonyms are read-only; only
 * the `&%Term<kind>` anchor run of a record is ever rewritten.
 *
 * The engine answers lookups from its installed lexicon, while the mappings
 * shown always come from the page's own copy of the record's line -- that copy
 * changes the moment an edit saves, the engine's only once its lexicon is
 * rebuilt. Edits are tracked changes (stores/changes.ts), so the shared
 * Contribute panel proposes them upstream exactly as it does edited KIF files.
 */
import { computed, ref, watch } from "vue";
import BaseDialog from "../components/BaseDialog.vue";
import Card from "../components/Card.vue";
import Segmented from "../components/Segmented.vue";
import StatusLine from "../components/StatusLine.vue";
import ContributePanel from "../components/edit/ContributePanel.vue";
import DiffDialog from "../components/edit/DiffDialog.vue";
import { useStatus } from "../composables/useStatus";
import { useTabQuery } from "../composables/useTabQuery";
import { navigate, updateParams } from "../router";
import { call } from "../services/sigma";
import { useAuthStore } from "../stores/auth";
import {
  isActionable,
  useChangesStore,
  type ChangeRow,
} from "../stores/changes";
import { isWordNetFile, useWordNetStore, wordnetPath } from "../stores/wordnet";
import { SUMO } from "../constants";
import { rankCompletions } from "../utils/kifInput";
import {
  MAPPING_KINDS,
  diffMappings,
  isValidTerm,
  parseAnchors,
  synsetKey,
  type Anchor,
  type MappingChange,
} from "../utils/wordnetMappings";
import type { WordNetMappingKind, WordNetSense } from "sigmakee/sdk";

const PAGE_SIZE = 100;
const POS_LABEL: Record<string, string> = {
  n: "noun",
  v: "verb",
  a: "adj",
  r: "adv",
};
const POS_OPTIONS = [
  { value: "", label: "All parts of speech" },
  { value: "n", label: "Nouns" },
  { value: "v", label: "Verbs" },
  { value: "a", label: "Adjectives" },
  { value: "r", label: "Adverbs" },
];
const KIND_LABEL: Record<string, string> = Object.fromEntries(
  MAPPING_KINDS.map((k) => [k.suffix, k.label]),
);
/** The mapping-kind filter's checkboxes; "negated" covers the rare `:`,
 *  `[`, `]` forms and any other suffix. */
const KIND_FILTERS: {
  value: string;
  label: string;
  kinds: WordNetMappingKind[];
}[] = [
  { value: "unmapped", label: "Unmapped", kinds: ["unmapped"] },
  { value: "equivalent", label: "= equivalent", kinds: ["equivalent"] },
  { value: "subsuming", label: "+ subsuming", kinds: ["subsuming"] },
  { value: "instance", label: "@ instance", kinds: ["instance"] },
  {
    value: "negated",
    label: "negated (: [ ])",
    kinds: ["anti_equivalent", "anti_subsuming", "anti_instance", "other"],
  },
];
const BY_OPTIONS: { value: "word" | "term"; label: string; title: string }[] = [
  {
    value: "word",
    label: "Word",
    title: "A word or synset id (02084071, 02084071-n)",
  },
  {
    value: "term",
    label: "SUMO term",
    title: "Every sense mapped to a SUMO term",
  },
];

const wordnet = useWordNetStore();
const changes = useChangesStore();
const auth = useAuthStore();
const { query, onQuery, str, num } = useTabQuery(["wordnet"]);
const status = useStatus();

const input = ref("");
const by = ref<"word" | "term">("word");
const senses = ref<WordNetSense[]>([]);
const total = ref(0);
/** The lookup the results came from, so a selection change never re-queries. */
const searched = ref("");
const searching = ref(false);

/** The `?kinds=` filter values that are ticked; none in the URL means all. */
const kindFilter = computed(() => {
  const listed = str(query.value.kinds).split(",").filter(Boolean);
  return new Set(listed.length ? listed : KIND_FILTERS.map((k) => k.value));
});
const posFilter = computed(() => str(query.value.pos));
const pageNo = computed(() => num(query.value.page) ?? 1);
const pageStart = computed(() => (pageNo.value - 1) * PAGE_SIZE);

const selected = computed(
  () =>
    senses.value.find((s) => s.id === str(query.value.s)) ??
    senses.value[0] ??
    null,
);

// -- Lookup -------------------------------------------------------------------

async function ensureLoaded() {
  if (wordnet.enabled && !wordnet.installed) await wordnet.install();
}

async function enable() {
  wordnet.setEnabled(true);
  await ensureLoaded();
  searched.value = "";
  await runSearch();
}

/** Re-run the current lookup even if nothing in it changed. */
function refresh() {
  searched.value = "";
  return runSearch();
}

async function runSearch() {
  if (!wordnet.installed) {
    senses.value = [];
    total.value = 0;
    searched.value = "";
    return;
  }
  const ticked = KIND_FILTERS.filter((k) => kindFilter.value.has(k.value));
  const kinds =
    ticked.length === KIND_FILTERS.length ? [] : ticked.flatMap((k) => k.kinds);
  const lookup = {
    query: input.value.trim(),
    byTerm: by.value === "term",
    kinds,
    pos: posFilter.value,
    offset: pageStart.value,
    limit: PAGE_SIZE,
  };
  const key = JSON.stringify(lookup);
  if (key === searched.value) return;
  if (!ticked.length) {
    senses.value = [];
    total.value = 0;
    searched.value = key;
    return;
  }
  searching.value = true;
  try {
    const { senses: found } = await call("wordnetSenses", lookup);
    senses.value = found.items;
    total.value = found.total;
    searched.value = key;
  } catch (e) {
    status.fail(e);
  } finally {
    searching.value = false;
  }
}

onQuery(async (q) => {
  input.value = str(q.q);
  by.value = str(q.by) === "term" ? "term" : "word";
  await ensureLoaded();
  await runSearch();
});

/** Update the URL's lookup params, back on page 1 with no selection. */
function setLookup(params: Record<string, unknown>) {
  status.clear();
  updateParams({ ...query.value, s: "", page: "", ...params });
}

function submit() {
  setLookup({
    q: input.value.trim(),
    by: by.value === "term" ? "term" : "",
  });
}

function setBy(value: "word" | "term") {
  by.value = value;
  if (input.value.trim()) submit();
}

function toggleKind(value: string, e: Event) {
  const next = new Set(kindFilter.value);
  if ((e.target as HTMLInputElement).checked) next.add(value);
  else next.delete(value);
  const all = KIND_FILTERS.every((k) => next.has(k.value));
  setLookup({
    kinds: all
      ? ""
      : KIND_FILTERS.filter((k) => next.has(k.value))
          .map((k) => k.value)
          .join(",") || "none",
  });
}

function setPos(e: Event) {
  setLookup({ pos: (e.target as HTMLSelectElement).value });
}

function goPage(n: number) {
  status.clear();
  updateParams({ ...query.value, s: "", page: n > 1 ? n : "" });
}

const pageCount = computed(() =>
  Math.max(1, Math.ceil(total.value / PAGE_SIZE)),
);
const rangeText = computed(() => {
  if (!total.value) return "No matching senses.";
  const from = pageStart.value + 1;
  const to = pageStart.value + senses.value.length;
  const n = total.value.toLocaleString();
  return `${from.toLocaleString()}-${to.toLocaleString()} of ${n} sense${total.value === 1 ? "" : "s"}`;
});

function select(s: WordNetSense) {
  status.clear();
  updateParams({ ...query.value, s: s.id });
}

/** Look up and select a synset by id (from the change list). */
function openSynset(key: string) {
  setLookup({ q: key, by: "", s: key });
}

// -- File preview ---------------------------------------------------------------

const PREVIEW_CONTEXT = 8;
const previewOpen = ref(false);
const preview = computed(() => {
  const s = selected.value;
  if (!s || !previewOpen.value) return [];
  void wordnet.revision;
  const out: { n: number; text: string }[] = [];
  for (
    let n = Math.max(1, s.line - PREVIEW_CONTEXT);
    n <= s.line + PREVIEW_CONTEXT;
    n++
  ) {
    const text = wordnet.lineAt(s.file, n);
    if (text === null) break;
    out.push({ n, text: text.replace(/\r$/, "") });
  }
  return out;
});
const upstreamUrl = computed(() => {
  const s = selected.value;
  return s
    ? `https://github.com/${SUMO.owner}/${SUMO.repo}/blob/${SUMO.branch}/${wordnetPath(s.file)}#L${s.line}`
    : "";
});

// -- Mappings -----------------------------------------------------------------

/** The sense's anchors as the page's copy of its record has them, falling
 *  back to the engine's when that line no longer holds the synset. */
function mappingsOf(s: WordNetSense): Anchor[] {
  void wordnet.revision;
  const line = wordnet.lineAt(s.file, s.line);
  return line !== null && synsetKey(line) === s.id
    ? parseAnchors(line)
    : s.mappings;
}

const anchors = computed(() =>
  selected.value ? mappingsOf(selected.value) : [],
);
const edited = computed(() => {
  const s = selected.value;
  if (!s) return false;
  const now = anchors.value.map((a) => a.term + a.suffix).join(" ");
  return now !== s.mappings.map((a) => a.term + a.suffix).join(" ");
});

const summary = (list: Anchor[]) =>
  list.map((a) => `${a.term}${a.suffix}`).join(" ") || "unmapped";

const kindOptions = (suffix: string) =>
  KIND_LABEL[suffix]
    ? MAPPING_KINDS
    : [...MAPPING_KINDS, { suffix, label: `other (${suffix})` }];

const busy = ref(false);

async function apply(next: Anchor[]) {
  const s = selected.value;
  if (!s || busy.value) return;
  busy.value = true;
  try {
    await wordnet.setMappings(s.file, s.line, s.id, next);
    status.set(
      "Saved in this browser. Use the GitHub button to propose it upstream.",
    );
  } catch (e) {
    status.fail(e);
  } finally {
    busy.value = false;
  }
}

function setKind(i: number, e: Event) {
  const suffix = (e.target as HTMLSelectElement).value;
  apply(anchors.value.map((a, j) => (j === i ? { ...a, suffix } : a)));
}

function removeAt(i: number) {
  apply(anchors.value.filter((_, j) => j !== i));
}

const draftTerm = ref("");
const draftKind = ref<string>("=");

function addMapping() {
  const term = draftTerm.value.trim();
  if (!isValidTerm(term)) {
    status.set(
      "A SUMO term is letters, digits, '_' and '-' only (e.g. DomesticDog).",
      true,
    );
    return;
  }
  if (anchors.value.some((a) => a.term === term)) {
    status.set(`${term} is already mapped -- change its kind instead.`, true);
    return;
  }
  apply([...anchors.value, { term, suffix: draftKind.value }]);
  draftTerm.value = "";
}

async function restore() {
  const s = selected.value;
  if (s) await apply(s.mappings);
}

// Terms in the loaded KB, for the "not in the loaded KB" warning; filled
// lazily from `search`, which also feeds the add-term suggestions.
const known = ref<Record<string, boolean>>({});

watch(
  anchors,
  async (list) => {
    for (const { term } of list) {
      if (term in known.value) continue;
      try {
        const { hits } = await call("search", { query: term, limit: 20 });
        known.value[term] = hits.some((h) => h.symbol === term);
      } catch {
        /* the warning is advisory */
      }
    }
  },
  { immediate: true },
);

const suggestions = ref<string[]>([]);
let suggestSeq = 0;
watch(draftTerm, async (word) => {
  const seq = ++suggestSeq;
  const w = word.trim();
  if (w.length < 2) {
    suggestions.value = [];
    return;
  }
  try {
    const { hits } = await call("search", { query: w, limit: 50 });
    if (seq !== suggestSeq) return;
    suggestions.value = rankCompletions(
      hits.map((h) => h.symbol),
      w,
      12,
    );
  } catch {
    suggestions.value = [];
  }
});

// -- Local changes --------------------------------------------------------------

const wordnetRows = computed(() =>
  changes.rows.filter((r) => isWordNetFile(r.name)),
);
const pending = ref<{ path: string; file: string; list: MappingChange[] }[]>(
  [],
);
const pendingCount = computed(() =>
  pending.value.reduce((n, f) => n + f.list.length, 0),
);
let pendingSeq = 0;
let pendingTimer: ReturnType<typeof setTimeout> | null = null;

async function refreshPending() {
  const seq = ++pendingSeq;
  const out: typeof pending.value = [];
  for (const row of wordnetRows.value) {
    const current = wordnet.fileText(row.name);
    if (current === null) continue;
    try {
      const base = await wordnet.baseText(row.name);
      out.push({
        path: row.name,
        file: row.name.split("/").pop() ?? row.name,
        list: diffMappings(base, current),
      });
    } catch {
      /* upstream unreachable: the Contribute panel still lists the file */
    }
  }
  if (seq === pendingSeq) pending.value = out.filter((f) => f.list.length);
}

watch(
  () => [wordnet.revision, wordnetRows.value.map((r) => r.name).join()],
  () => {
    if (pendingTimer) clearTimeout(pendingTimer);
    pendingTimer = setTimeout(refreshPending, 300);
  },
  { immediate: true },
);

async function revert(file: string, c: MappingChange) {
  try {
    await wordnet.setMappings(file, c.line, c.key, parseAnchors(c.before));
  } catch (e) {
    status.fail(e);
  }
}

// -- GitHub -------------------------------------------------------------------

const ghOpen = ref(false);
const diffOpen = ref(false);
const diffRow = ref<ChangeRow | null>(null);
const ghCount = computed(() => changes.actionableCount);
const wnActionable = computed(
  () => wordnetRows.value.filter(isActionable).length,
);

function onPropose() {
  if (!auth.signedIn) {
    auth.openLoginDialog();
    return;
  }
  ghOpen.value = !ghOpen.value;
}

function openDiff(row: ChangeRow) {
  diffRow.value = row;
  diffOpen.value = true;
}

/** Upstream replaced a file: its line numbers may have moved, so look up the
 *  results again rather than trusting the old ones. */
function onTaken() {
  refresh();
}

function onKeep(row: ChangeRow) {
  changes.acknowledgeUpstream(row.name, row.origin);
}
</script>

<template>
  <div class="wordnet-tab">
    <Card
      title="WordNet mappings"
      description="Find a word sense and choose the SUMO terms it maps to. Changes are saved in this browser until you propose them as a pull request."
    >
      <template #header>
        <button
          class="btn ghost gh-btn"
          type="button"
          :aria-expanded="ghOpen"
          :title="
            ghCount
              ? `${ghCount} file${ghCount === 1 ? '' : 's'} changed and not yet pushed`
              : 'Push changes or create a pull request'
          "
          @click="onPropose"
        >
          GitHub
          <span v-if="ghCount" class="count-badge">{{ ghCount }}</span>
        </button>
      </template>

      <form class="wn-search" @submit.prevent="submit">
        <Segmented
          :model-value="by"
          :options="BY_OPTIONS"
          label="Search by"
          @update:model-value="setBy"
        />
        <input
          v-model="input"
          type="search"
          spellcheck="false"
          :placeholder="
            by === 'term'
              ? 'SUMO term, e.g. Canine'
              : 'Word or synset id, e.g. dog or 02084071-n'
          "
          aria-label="Search WordNet"
        />
        <button class="btn" type="submit" :disabled="!wordnet.installed">
          Search
        </button>
      </form>
      <div class="wn-filters">
        <span class="hint">Mappings:</span>
        <label v-for="k in KIND_FILTERS" :key="k.value" class="check">
          <input
            type="checkbox"
            :checked="kindFilter.has(k.value)"
            @change="toggleKind(k.value, $event)"
          />
          {{ k.label }}
        </label>
        <select
          :value="posFilter"
          aria-label="Part of speech"
          class="wn-pos-filter"
          @change="setPos"
        >
          <option v-for="p in POS_OPTIONS" :key="p.value" :value="p.value">
            {{ p.label }}
          </option>
        </select>
      </div>

      <p v-if="!wordnet.enabled" class="hint mt">
        WordNet is turned off.
        <button type="button" class="btn ghost" @click="enable">
          Turn on and load WordNet
        </button>
      </p>
      <p v-else-if="wordnet.loading" class="hint mt">Loading WordNet...</p>
      <p v-else-if="!wordnet.installed" class="hint mt">
        WordNet is not loaded.
        <button type="button" class="btn ghost" @click="enable">Retry</button>
      </p>

      <ContributePanel
        v-show="ghOpen"
        :open="ghOpen"
        :current="null"
        :dirty="false"
        @diff="openDiff"
      />
      <DiffDialog
        v-model="diffOpen"
        :row="diffRow"
        @taken="onTaken"
        @keep="onKeep"
      />
    </Card>

    <div v-if="wordnet.installed" class="wn-body">
      <Card class="wn-results">
        <div class="wn-results-head hint">
          <span>{{ searching ? "Searching..." : rangeText }}</span>
          <span v-if="pageCount > 1" class="wn-pager">
            <button
              type="button"
              class="btn ghost"
              :disabled="pageNo <= 1 || searching"
              @click="goPage(pageNo - 1)"
            >
              ‹ Prev
            </button>
            <button
              type="button"
              class="btn ghost"
              :disabled="pageNo >= pageCount || searching"
              @click="goPage(pageNo + 1)"
            >
              Next ›
            </button>
          </span>
        </div>
        <ul class="results">
          <li
            v-for="s in senses"
            :key="s.id"
            :class="{ active: selected?.id === s.id }"
          >
            <button type="button" class="wn-hit" @click="select(s)">
              <span class="wn-hit-head">
                <span class="wn-pos">{{ s.pos }}</span>
                <b>{{ s.label || s.words[0] }}</b>
                <span class="mono wn-map">{{ summary(mappingsOf(s)) }}</span>
              </span>
              <span class="hint wn-gloss">{{ s.gloss }}</span>
            </button>
          </li>
        </ul>
      </Card>

      <Card v-if="selected" class="wn-detail">
        <div class="wn-detail-head">
          <h2>{{ selected.words.join(", ") }}</h2>
          <span class="hint">
            {{ POS_LABEL[selected.pos] ?? selected.pos }} ·
            <span class="mono">{{ selected.id }}</span>
            <template v-if="selected.label">
              · <span class="mono">{{ selected.label }}</span></template
            >
          </span>
        </div>
        <p class="wn-def">{{ selected.gloss }}</p>
        <a
          class="mono wn-src"
          title="Preview the raw record in its mapping file"
          @click="previewOpen = true"
          >{{ selected.file }}:{{ selected.line }}</a
        >
        <BaseDialog
          v-model="previewOpen"
          :title="`${selected.file}:${selected.line}`"
          width="min(1100px, 94vw)"
        >
          <p class="hint">
            The record as this browser holds it, including unpushed edits.
          </p>
          <div class="wn-preview mono">
            <div
              v-for="l in preview"
              :key="l.n"
              class="wn-preview-line"
              :class="{ hit: l.n === selected.line }"
            >
              <span class="wn-preview-n">{{ l.n }}</span
              ><span>{{ l.text }}</span>
            </div>
          </div>
          <template #actions>
            <a class="hint" :href="upstreamUrl" target="_blank" rel="noopener"
              >Upstream on GitHub ↗</a
            >
            <span class="spacer"></span>
            <button class="btn" type="button" @click="previewOpen = false">
              Close
            </button>
          </template>
        </BaseDialog>

        <div class="section-label mt">SUMO mappings</div>
        <table class="wn-maps">
          <tbody>
            <tr v-for="(a, i) in anchors" :key="a.term + i">
              <td>
                <a class="sym" @click="navigate('browse', { sym: a.term })">{{
                  a.term
                }}</a>
                <span
                  v-if="known[a.term] === false"
                  class="wn-warn"
                  title="No such term in the loaded knowledge base"
                  >not in loaded KB</span
                >
              </td>
              <td>
                <select
                  :value="a.suffix"
                  :disabled="busy"
                  :aria-label="`Mapping kind for ${a.term}`"
                  @change="setKind(i, $event)"
                >
                  <option
                    v-for="k in kindOptions(a.suffix)"
                    :key="k.suffix"
                    :value="k.suffix"
                  >
                    {{ k.suffix }} {{ k.label }}
                  </option>
                </select>
              </td>
              <td class="wn-act">
                <button
                  type="button"
                  class="btn ghost"
                  :disabled="busy"
                  :aria-label="`Remove mapping to ${a.term}`"
                  @click="removeAt(i)"
                >
                  Remove
                </button>
              </td>
            </tr>
            <tr v-if="!anchors.length">
              <td colspan="3" class="hint">No SUMO mapping.</td>
            </tr>
          </tbody>
        </table>

        <form class="wn-add" @submit.prevent="addMapping">
          <input
            v-model="draftTerm"
            type="text"
            list="wn-term-options"
            spellcheck="false"
            placeholder="Add a SUMO term"
            aria-label="SUMO term to map"
          />
          <datalist id="wn-term-options">
            <option v-for="t in suggestions" :key="t" :value="t" />
          </datalist>
          <select v-model="draftKind" aria-label="Mapping kind">
            <option
              v-for="k in MAPPING_KINDS"
              :key="k.suffix"
              :value="k.suffix"
            >
              {{ k.suffix }} {{ k.label }}
            </option>
          </select>
          <button
            class="btn"
            type="submit"
            :disabled="busy || !draftTerm.trim()"
          >
            Add
          </button>
        </form>
        <div class="inline between mt">
          <StatusLine :text="status.text" :error="status.error" />
          <button
            v-if="edited"
            type="button"
            class="btn ghost"
            :disabled="busy"
            title="Put back the mappings this sense had when it was looked up"
            @click="restore"
          >
            Undo changes to this sense
          </button>
        </div>
      </Card>
    </div>
    <StatusLine
      v-else-if="status.text"
      :text="status.text"
      :error="status.error"
    />

    <Card
      v-if="pendingCount"
      title="Local mapping changes"
      :description="`${pendingCount === 1 ? '1 sense differs' : `${pendingCount} senses differ`} from upstream${wnActionable ? '' : ' (already proposed)'}.`"
    >
      <template v-for="f in pending" :key="f.path">
        <div class="section-label">{{ f.file }}</div>
        <ul class="results">
          <li v-for="c in f.list" :key="c.key" class="wn-change">
            <a class="mono" @click="openSynset(c.key)">{{ c.word }}</a>
            <span class="hint mono">{{ c.key }}</span>
            <span class="mono wn-diff">
              <del>{{ c.before || "unmapped" }}</del> →
              <ins>{{ c.after || "unmapped" }}</ins>
            </span>
            <button type="button" class="btn ghost" @click="revert(f.file, c)">
              Revert
            </button>
          </li>
        </ul>
      </template>
    </Card>
  </div>
</template>

<style scoped>
.wordnet-tab {
  display: flex;
  flex-direction: column;
  gap: 14px;
}
.wn-search,
.wn-add {
  display: flex;
  gap: 8px;
  align-items: center;
}
.wn-search {
  margin-top: 12px;
}
.wn-filters {
  display: flex;
  flex-wrap: wrap;
  gap: 6px 14px;
  align-items: center;
  margin-top: 10px;
}
.wn-pos-filter {
  margin-left: auto;
}
.wn-pager {
  display: inline-flex;
  gap: 6px;
}
.wn-preview {
  max-height: 60vh;
  overflow: auto;
  font-size: 12px;
  border: 1px solid var(--line);
  border-radius: 6px;
  background: var(--card);
}
.wn-preview-line {
  display: flex;
  white-space: pre;
  width: max-content;
  min-width: 100%;
}
.wn-preview-line.hit {
  background: color-mix(in srgb, var(--accent) 18%, transparent);
}
.wn-preview-n {
  flex: 0 0 auto;
  min-width: 6ch;
  padding: 0 8px;
  text-align: right;
  color: var(--muted);
  user-select: none;
  position: sticky;
  left: 0;
  background: var(--card);
}
.wn-search input,
.wn-add input {
  flex: 1;
}
.wn-body {
  display: grid;
  grid-template-columns: minmax(0, 2fr) minmax(0, 3fr);
  gap: 14px;
  align-items: start;
}
@media (max-width: 1000px) {
  .wn-body {
    grid-template-columns: 1fr;
  }
}
.wn-results ul {
  max-height: 65vh;
  overflow-y: auto;
}
.wn-results-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 8px;
  margin-bottom: 6px;
}
.wn-results li {
  padding: 0;
}
.wn-results li.active {
  background: var(--bg);
}
.wn-hit {
  display: flex;
  flex-direction: column;
  gap: 2px;
  width: 100%;
  padding: 8px 6px;
  background: none;
  border: none;
  color: inherit;
  font: inherit;
  text-align: left;
  cursor: pointer;
}
.wn-hit-head {
  display: flex;
  gap: 8px;
  align-items: baseline;
}
.wn-map {
  margin-left: auto;
  font-size: 12px;
  color: var(--accent);
}
.wn-gloss {
  font-size: 12px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.wn-pos {
  font-family: var(--mono);
  font-size: 11px;
  color: var(--muted);
}
.wn-detail-head h2 {
  font-size: 17px;
  margin: 0;
}
.wn-def {
  margin: 10px 0 4px;
}
.wn-src {
  font-size: 12px;
  color: var(--accent);
  cursor: pointer;
}
.wn-src:hover {
  text-decoration: underline;
}
.wn-maps {
  width: 100%;
  border-collapse: collapse;
  margin: 6px 0 10px;
}
.wn-maps td {
  padding: 4px 6px 4px 0;
  border-bottom: 1px solid var(--line);
}
.wn-maps .wn-act {
  text-align: right;
}
.wn-maps a.sym {
  cursor: pointer;
}
.wn-warn {
  margin-left: 8px;
  font-size: 11px;
  color: var(--warn);
}
.wn-change {
  display: flex;
  gap: 10px;
  align-items: center;
}
.wn-change a {
  cursor: pointer;
  color: var(--accent);
}
.wn-diff {
  flex: 1;
  font-size: 12px;
}
.wn-diff del {
  color: var(--bad);
}
.wn-diff ins {
  color: var(--ok);
  text-decoration: none;
}
.gh-btn {
  display: inline-flex;
  gap: 6px;
  align-items: center;
}
</style>
