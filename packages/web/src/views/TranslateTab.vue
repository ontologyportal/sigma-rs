<script setup lang="ts">
/** Controlled English -> SUO-KIF via CELT, against the loaded KB and WordNet
 *  lexicon. Each sentence is shown with its KIF and how it was derived (the
 *  rule, lexical choices and side assertions), or with the reason it was
 *  refused and the offending words marked. The input is mirrored to `?q=`
 *  so a translation can be shared. */
import { computed, reactive, ref, shallowRef, toRaw, watch } from "vue";
import type { CeltOptions, CeltSentence, CeltTranslation } from "sigmakee/sdk";
import { call } from "../services/sigma";
import { navigate, updateParams } from "../router";
import { CELT_OPTIONS_KEY } from "../constants";
import { useTabQuery } from "../composables/useTabQuery";
import { useWordNetStore } from "../stores/wordnet";
import BusyButton from "../components/BusyButton.vue";
import Card from "../components/Card.vue";
import CiteRow from "../components/CiteRow.vue";
import CopyButton from "../components/CopyButton.vue";
import Disclosure from "../components/Disclosure.vue";

const text = ref("");
const busy = ref(false);
const translation = shallowRef<CeltTranslation | null>(null);
const error = ref("");

const wordnet = useWordNetStore();

/** Every option set, so the form binds to plain strings and booleans;
 *  blank strings mean unset (the binding ignores them). */
const DEFAULT_OPTIONS: Required<CeltOptions> = {
  speaker: "",
  addressee: "",
  genericYou: "",
  now: "",
  assumeNames: false,
  generic: false,
  useNames: true,
  lines: false,
  parrot: false,
};
const options = reactive<Required<CeltOptions>>({ ...DEFAULT_OPTIONS });

try {
  const saved = JSON.parse(localStorage.getItem(CELT_OPTIONS_KEY) || "null");
  if (saved && typeof saved === "object")
    for (const k of Object.keys(DEFAULT_OPTIONS) as (keyof CeltOptions)[])
      if (typeof saved[k] === typeof DEFAULT_OPTIONS[k])
        (options as Record<string, unknown>)[k] = saved[k];
} catch {
  /* storage unavailable or corrupt: keep the defaults */
}

/** The options that differ from the defaults, for the summary line. */
const activeOptions = computed(() =>
  (Object.keys(DEFAULT_OPTIONS) as (keyof CeltOptions)[]).filter(
    (k) => options[k] !== DEFAULT_OPTIONS[k],
  ),
);

/** False once the lexicon is fetched without a `FirstNames.csv`. */
const namesAvailable = computed(
  () =>
    !wordnet.files.length ||
    wordnet.files.some((f) => f.name === "FirstNames.csv"),
);

const { query, onQuery, str } = useTabQuery(["translate"]);

watch(
  options,
  () => {
    try {
      localStorage.setItem(CELT_OPTIONS_KEY, JSON.stringify(options));
    } catch {
      /* storage unavailable */
    }
    const input = str(query.value.q);
    if (input.trim()) void run(input);
  },
  { deep: true },
);

function resetOptions() {
  Object.assign(options, DEFAULT_OPTIONS);
}

onQuery((q) => {
  text.value = str(q.q);
  if (text.value.trim()) void run(text.value);
  else clear();
});

function clear() {
  translation.value = null;
  error.value = "";
}

/** Bumped per run; only the latest run's outcome is shown. */
let latestRun = 0;

async function run(input: string) {
  const id = ++latestRun;
  busy.value = true;
  clear();
  try {
    const { translation: t } = await call("translateCelt", {
      text: input,
      // A reactive proxy cannot be structured-cloned across postMessage.
      options: { ...toRaw(options) },
    });
    if (id !== latestRun) return;
    if (!t.available)
      error.value = t.error || "The WordNet-SUMO lexicon is not loaded.";
    else translation.value = t;
  } catch (e) {
    if (id === latestRun)
      error.value = e instanceof Error ? e.message : String(e);
  } finally {
    if (id === latestRun) busy.value = false;
  }
}

async function translate() {
  const input = text.value.trim();
  if (!input || busy.value) return;
  // A changed query re-runs through `onQuery`; an unchanged one would not.
  if (input === str(query.value.q)) await run(input);
  else await updateParams({ q: input });
}

/** `text` split around the refusal span, for marking the offending words.
 *  The span counts code points, so slice by code point, not UTF-16 unit. */
function marked(s: CeltSentence): {
  before: string;
  mark: string;
  after: string;
} {
  const chars = Array.from(s.text);
  if (!s.span) return { before: s.text, mark: "", after: "" };
  const start = Math.min(Math.max(0, s.span.start), chars.length);
  const end = Math.min(Math.max(start, s.span.end), chars.length);
  return {
    before: chars.slice(0, start).join(""),
    mark: chars.slice(start, end).join(""),
    after: chars.slice(end).join(""),
  };
}

function openTerm(symbol: string) {
  navigate("browse", { sym: symbol });
}

/** Look up and select a synset on the WordNet tab by its id. */
function openSynset(id: string) {
  navigate("wordnet", { q: id, s: id });
}

function onKeydown(e: KeyboardEvent) {
  if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
    e.preventDefault();
    void translate();
  }
}
</script>

<template>
  <Card
    title="Translate"
    description="Convert controlled English sentences into SUO-KIF using the loaded knowledge base and WordNet lexicon."
  >
    <label class="input-label" for="translate-input">English</label>
    <textarea
      id="translate-input"
      v-model="text"
      rows="4"
      spellcheck="true"
      placeholder="Every dog is an animal. Is Rex a dog?"
      :disabled="busy"
      @keydown="onKeydown"
    />
    <div class="inline between actions">
      <span class="hint">Ctrl/Cmd+Enter to translate</span>
      <BusyButton
        :busy="busy"
        label="Translate"
        busy-label="Translating..."
        :disabled="!text.trim()"
        @click="translate"
      />
    </div>
    <Disclosure
      :summary="
        activeOptions.length
          ? `Options (${activeOptions.length} changed)`
          : 'Options'
      "
      :open="activeOptions.length > 0"
    >
      <div class="options">
        <div class="fields">
          <div>
            <label for="celt-speaker">Speaker ("I", "me", "my")</label>
            <input
              id="celt-speaker"
              v-model.lazy.trim="options.speaker"
              type="text"
              placeholder="KB instance, e.g. Rex"
            />
          </div>
          <div>
            <label for="celt-addressee">Addressee ("you", "your")</label>
            <input
              id="celt-addressee"
              v-model.lazy.trim="options.addressee"
              type="text"
              placeholder="KB instance, e.g. Mary"
            />
          </div>
          <div>
            <label for="celt-generic-you">Generic "you" class</label>
            <input
              id="celt-generic-you"
              v-model.lazy.trim="options.genericYou"
              type="text"
              placeholder="e.g. Human"
              :disabled="!!options.addressee"
              :title="
                options.addressee
                  ? 'Ignored while an addressee is set'
                  : 'Read &quot;you&quot; as any instance of this class'
              "
            />
          </div>
          <div>
            <label for="celt-now">Now (for the past tense)</label>
            <input
              id="celt-now"
              v-model.lazy.trim="options.now"
              type="text"
              placeholder="Time point; defaults to the KB's Now"
            />
          </div>
        </div>
        <div class="checks">
          <label class="check"
            ><input v-model="options.useNames" type="checkbox" /> Read first
            names from <code>FirstNames.csv</code> as people
            <span v-if="!namesAvailable" class="hint">(not loaded)</span></label
          >
          <label class="check"
            ><input v-model="options.assumeNames" type="checkbox" /> Treat
            unknown capitalized words as names of people</label
          >
          <label class="check"
            ><input v-model="options.generic" type="checkbox" /> Read "Dogs
            chase cats" as a rule about every dog</label
          >
          <label class="check"
            ><input v-model="options.lines" type="checkbox" /> Translate each
            line separately (no pronouns or "the" across lines)</label
          >
          <label class="check"
            ><input v-model="options.parrot" type="checkbox" /> Parrot each
            translation back as English</label
          >
        </div>
        <button
          v-if="activeOptions.length"
          class="btn ghost small"
          type="button"
          @click="resetOptions"
        >
          Reset options
        </button>
      </div>
    </Disclosure>
  </Card>

  <Card v-if="error">
    <p class="bad" role="alert">{{ error }}</p>
  </Card>

  <Card v-else-if="translation">
    <p v-if="!translation.sentences.length" class="hint">
      No sentence was recognized.
    </p>
    <ol class="sentences">
      <li v-for="(s, i) in translation.sentences" :key="`${s.offset}-${i}`">
        <div class="inline tight center">
          <span class="kind" :class="s.kif ? 'ok' : 'refused'">{{
            s.kif ? s.kind : "not translated"
          }}</span>
          <span v-if="s.kif" class="source">{{ s.text }}</span>
          <span v-else class="source"
            >{{ marked(s).before
            }}<mark v-if="marked(s).mark">{{ marked(s).mark }}</mark
            >{{ marked(s).after }}</span
          >
        </div>
        <template v-if="s.kif">
          <div class="kif">
            <ol class="kif-rows">
              <CiteRow :kif="s.kif" :paraphrase="false" />
            </ol>
            <CopyButton :text="s.kif" title="Copy KIF" />
          </div>
          <p v-if="s.parrot" class="parrot">~ {{ s.parrot }}</p>
          <Disclosure summary="How it was translated">
            <dl class="steps">
              <dt>Rule</dt>
              <dd>
                <code>{{ s.rule }}</code>
              </dd>
              <template v-if="s.choices.length">
                <dt>Words</dt>
                <dd>
                  <table class="choices">
                    <thead>
                      <tr>
                        <th>Word</th>
                        <th>Term</th>
                        <th>WordNet sense</th>
                        <th>Passed over</th>
                      </tr>
                    </thead>
                    <tbody>
                      <tr v-for="(c, j) in s.choices" :key="j">
                        <td>{{ c.word }}</td>
                        <td>
                          <a href="#" @click.prevent="openTerm(c.term)">{{
                            c.term
                          }}</a>
                        </td>
                        <td>
                          <a
                            v-if="c.synset"
                            href="#"
                            :title="`Open synset ${c.synset} on the WordNet tab`"
                            @click.prevent="openSynset(c.synset)"
                            ><code>{{ c.sense }}</code></a
                          >
                          <code v-else>{{ c.sense || "-" }}</code>
                        </td>
                        <td>
                          <template v-if="c.alternatives.length">
                            <template
                              v-for="(alt, k) in c.alternatives"
                              :key="alt"
                              ><template v-if="k">, </template
                              ><a href="#" @click.prevent="openTerm(alt)">{{
                                alt
                              }}</a></template
                            >
                          </template>
                          <span v-else class="hint">none</span>
                        </td>
                      </tr>
                    </tbody>
                  </table>
                </dd>
              </template>
              <template v-if="s.declared.length">
                <dt>Assumes</dt>
                <dd>
                  <ol class="kif-rows">
                    <CiteRow
                      v-for="d in s.declared"
                      :key="d"
                      :kif="d"
                      :paraphrase="false"
                    />
                  </ol>
                </dd>
              </template>
            </dl>
          </Disclosure>
        </template>
        <p v-else class="bad reason">
          <code class="code">{{ s.code }}</code> {{ s.reason }}
        </p>
      </li>
    </ol>
  </Card>
</template>

<style scoped>
.options {
  display: grid;
  gap: 12px;
  margin-top: 10px;
  justify-items: start;
}
.fields {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
  gap: 10px 14px;
  width: 100%;
}
.checks {
  display: grid;
  gap: 6px;
}
.checks .check {
  font-size: 13px;
  color: var(--fg);
}
.parrot {
  margin: 6px 0 0;
  font-size: 13px;
  color: var(--ok);
}
.input-label {
  margin-top: 12px;
}
.actions {
  margin-top: 8px;
}
.sentences {
  list-style: none;
  margin: 0;
  padding: 0;
  display: grid;
  gap: 14px;
}
.kind {
  padding: 2px 8px;
  border-radius: 20px;
  font-size: 11px;
  font-weight: 700;
  text-transform: capitalize;
  white-space: nowrap;
}
.kind.ok {
  background: color-mix(in srgb, var(--ok) 20%, transparent);
  color: var(--ok);
}
.kind.refused {
  background: color-mix(in srgb, var(--bad) 18%, transparent);
  color: var(--bad);
}
.kif {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 8px;
  margin-top: 6px;
  padding: 2px 6px;
  border: 1px solid var(--line);
  border-radius: 7px;
  background: var(--bg);
}
.kif-rows {
  list-style: none;
  margin: 0;
  padding: 0;
  min-width: 0;
  flex: 1;
  display: grid;
  gap: 4px;
}
.reason {
  margin: 6px 0 0;
  font-size: 13px;
}
.reason .code {
  font-size: 11px;
  opacity: 0.8;
}
mark {
  background: color-mix(in srgb, var(--bad) 22%, transparent);
  color: inherit;
  border-bottom: 2px solid var(--bad);
  border-radius: 2px;
}
.steps {
  display: grid;
  grid-template-columns: max-content 1fr;
  gap: 8px 14px;
  margin: 10px 0 0;
  font-size: 13px;
}
.steps dt {
  color: var(--muted);
}
.steps dd {
  margin: 0;
  min-width: 0;
}
.choices {
  border-collapse: collapse;
  width: 100%;
}
.choices th {
  text-align: left;
  font-weight: 600;
  color: var(--muted);
  font-size: 12px;
}
.choices th,
.choices td {
  padding: 3px 12px 3px 0;
  border-bottom: 1px solid var(--line);
  vertical-align: top;
}
</style>
