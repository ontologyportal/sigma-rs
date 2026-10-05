<script setup lang="ts">
import {
  computed,
  onActivated,
  onBeforeUnmount,
  reactive,
  ref,
  shallowRef,
  watch,
} from "vue";
import { AskResult, formatTest, type ParsedTest } from "sigmakee/sdk";
import { useTabQuery } from "../composables/useTabQuery";
import { useStatus } from "../composables/useStatus";
import { navigate } from "../router";
import { call } from "../services/sigma";
import { downloadText, errMsg } from "../utils/format";
import { useProverStore } from "../stores/prover";
import {
  gradeTest,
  useTestsStore,
  type TestEntry,
  type TestOutcome,
} from "../stores/tests";
import type * as Monaco from "monaco-editor/esm/vs/editor/editor.api.js";
import type { MonacoNs } from "../services/monaco";
import BusyButton from "../components/BusyButton.vue";
import { useElapsed } from "../composables/useElapsed";
import Card from "../components/Card.vue";
import Disclosure from "../components/Disclosure.vue";
import DropMenu from "../components/DropMenu.vue";
import MonacoEditor from "../components/MonacoEditor.vue";
import ProofView from "../components/ProofView.vue";
import ProverChips from "../components/ProverChips.vue";
import ProverOptions from "../components/ProverOptions.vue";
import Segmented from "../components/Segmented.vue";
import StatusLine from "../components/StatusLine.vue";

const prover = useProverStore();
const tests = useTestsStore();

const assertions = ref("(instance Rex Dog)\n(subclass Dog Mammal)");
const query = ref("(instance Rex Animal)");
const assertionsEd = ref<InstanceType<typeof MonacoEditor> | null>(null);
const queryEd = ref<InstanceType<typeof MonacoEditor> | null>(null);

/** TPTP mode reads the single assertions pane as one whole problem (axioms +
 *  an embedded conjecture) -- a TPTP problem is naturally one role-tagged
 *  document, not two independently composed pieces -- so the query pane is
 *  hidden and the "Use SUMO" toggle shown only there. */
const tptpMode = computed(() => prover.proofLang === "tptp");

const proving = ref(false);
const runLimitSecs = ref(0);
const {
  label: elapsedLabel,
  fraction: elapsedFraction,
  lastLabel: tookLabel,
} = useElapsed(proving);
const savingTest = ref(false);
/** The save-test result line under the button row. */
const testLog = useStatus();
/** Transient note beside the actions ("Enter a query first."). */
const cfgNote = ref("");
const optionsOpen = ref(false);
const moreOpen = ref(false);
const moreBtn = ref<HTMLElement | null>(null);

const langOptions = [
  {
    value: "kif" as const,
    label: "SUO-KIF",
    title: "Assertions + query in SUO-KIF",
  },
  {
    value: "tptp" as const,
    label: "TPTP",
    title: "One TPTP problem with an embedded conjecture",
  },
];
const proveKey = /Mac|iPhone|iPad/.test(navigator.platform)
  ? "⌘↵"
  : "Ctrl+Enter";

// The exact TPTP problem text handed to Vampire for the most recent Ask/Tell
// run -- a Vampire result carries it as `input_tptp` (the worker's Config
// asks the engine to keep it); the download button just hands back what's
// already in memory, no extra worker round-trip.
let lastVampireTptp: string | null = null;
const showDownloadTptp = ref(false);

const result = shallowRef<AskResult | null>(null);
/** The last result graded against the test's expected answer, if any. */
const outcome = ref<TestOutcome | null>(null);

// -- test details (the `.kif.tq` harness directives) ---------------------------

type ExpectedAnswer = "yes" | "no" | "bindings" | "none";
/** Edited beside the panes, filled from an opened test, and written back by
 *  Save test. Lists are comma-separated in the form. */
const meta = reactive({
  note: "",
  categories: "",
  timeout: 0,
  answer: "yes" as ExpectedAnswer,
  bindings: "",
  files: "",
});
const metaOpen = ref(false);
const splitList = (s: string) =>
  s
    .split(/[,\n]/)
    .map((x) => x.trim())
    .filter(Boolean);

function resetMeta() {
  Object.assign(meta, {
    note: "",
    categories: "",
    timeout: 0,
    answer: "yes",
    bindings: "",
    files: "",
  });
}

function metaFromTest(p: ParsedTest) {
  meta.note = p.noteGiven ? p.note : "";
  meta.categories = (p.categories ?? []).join(", ");
  meta.timeout = p.timeGiven ? p.timeout : 0;
  meta.answer = p.expectedAnswer?.length
    ? "bindings"
    : p.expectedProof === true
      ? "yes"
      : p.expectedProof === false
        ? "no"
        : "none";
  meta.bindings = (p.expectedAnswer ?? []).join(" ");
  meta.files = (p.extraFiles ?? []).join(", ");
}

/** The details apply while a test is being worked on -- one is open, or the
 *  user expanded the section to write one -- never to ad-hoc queries. */
const testActive = computed(
  () => !tptpMode.value && (!!tests.openTest || metaOpen.value),
);
/** The expected proof outcome `meta` describes (bindings imply a proof). */
const expectedProof = computed<boolean | null>(() =>
  meta.answer === "none" ? null : meta.answer !== "no",
);
/** A test's own `(time N)` overrides the prover time limit, as when the
 *  Inference Tests tab runs it. */
const testTimeLimit = computed(() =>
  testActive.value && meta.timeout > 0 ? Math.round(meta.timeout) : 0,
);
const metaSummary = computed(() => {
  const parts: string[] = [];
  if (meta.note) parts.push(meta.note);
  const cats = splitList(meta.categories);
  if (cats.length) parts.push(cats.join(", "));
  if (meta.answer !== "none")
    parts.push(
      `expects ${meta.answer === "bindings" ? meta.bindings || "bindings" : meta.answer}`,
    );
  if (testTimeLimit.value) parts.push(`${testTimeLimit.value}s`);
  return parts.length ? `Test details — ${parts.join(" · ")}` : "Test details";
});
const resultBackend = ref("");
const error = ref("");

const backendBadge = computed(() =>
  resultBackend.value && !error.value ? `via ${resultBackend.value}` : "",
);
const stepsText = computed(() => {
  if (error.value) return error.value;
  const r = result.value;
  return r && r.given_steps != null
    ? `${r.given_steps} given-clause steps`
    : "";
});

// -- scratch validation -------------------------------------------------------

let scratchTimer = 0;
let scratchBusy = false;
let scratchQueued = false;
let editorsReady = 0;

function scheduleScratchValidate() {
  clearTimeout(scratchTimer);
  scratchTimer = window.setTimeout(runScratchValidate, 400);
}

async function runScratchValidate() {
  const a = assertionsEd.value;
  const q = queryEd.value;
  if (!a || !q) return;
  // `validateScratch` only understands SUO-KIF -- in TPTP mode it would
  // paint the editors with spurious "parse error" squiggles under
  // perfectly valid TPTP text, so just clear any stale markers instead.
  if (tptpMode.value) {
    a.setMarkers([]);
    q.setMarkers([]);
    return;
  }
  if (scratchBusy) {
    scratchQueued = true;
    return;
  }
  scratchBusy = true;
  try {
    const r = await call("validateScratch", {
      assertions: assertions.value,
      query: query.value,
    });
    assertionsEd.value?.setMarkers(r.assertions);
    queryEd.value?.setMarkers(r.query);
  } catch (e) {
    console.warn("validateScratch:", errMsg(e));
  } finally {
    scratchBusy = false;
    if (scratchQueued) {
      scratchQueued = false;
      runScratchValidate();
    }
  }
}

function onEditorReady(
  editor: Monaco.editor.IStandaloneCodeEditor,
  m: MonacoNs,
) {
  // An action, not `addCommand`: Monaco registers commands for every editor
  // on the page, so the shortcut would also prove from the Edit tab.
  editor.addAction({
    id: "sumo.prove",
    label: "Prove",
    keybindings: [m.KeyMod.CtrlCmd | m.KeyCode.Enter],
    run: () => {
      if (!proving.value) prove();
    },
  });
  editorsReady += 1;
  if (editorsReady === 2) runScratchValidate();
}

watch([assertions, query], scheduleScratchValidate);
watch(
  () => prover.proofLang,
  () => runScratchValidate(),
);
watch(
  () => prover.backend,
  () => {
    showDownloadTptp.value = false;
  },
);
onBeforeUnmount(() => clearTimeout(scratchTimer));

// -- prove --------------------------------------------------------------------

/** Validate both panes now (not after the edit debounce) and count their
 *  error-level diagnostics; the markers are refreshed as a side effect. */
async function paneErrors(): Promise<{ assertions: number; query: number }> {
  const r = await call("validateScratch", {
    assertions: assertions.value,
    query: query.value,
  });
  assertionsEd.value?.setMarkers(r.assertions);
  queryEd.value?.setMarkers(r.query);
  const errors = (ds: { severity: string }[]) =>
    ds.filter((d) => d.severity === "error").length;
  return { assertions: errors(r.assertions), query: errors(r.query) };
}

async function prove() {
  const backend = prover.backendLabel;
  cfgNote.value = "";
  proving.value = true;
  lastVampireTptp = null;
  showDownloadTptp.value = false;
  outcome.value = null;
  try {
    if (!tptpMode.value) {
      const n = await paneErrors();
      if (n.assertions || n.query) {
        const where = [
          n.assertions && `${n.assertions} in the assertions`,
          n.query && `${n.query} in the query`,
        ]
          .filter(Boolean)
          .join(" and ");
        result.value = null;
        resultBackend.value = "";
        error.value = `Not proved: fix the errors first (${where}; they're underlined).`;
        return;
      }
    }
    await prover.loadDefaults();
    const config = prover.config(
      "ask",
      testTimeLimit.value ? { timeLimitSecs: testTimeLimit.value } : {},
    );
    runLimitSecs.value = config.timeLimitSecs ?? 0;
    let r: AskResult;
    let asserted = assertions.value.trim();
    let asked = tptpMode.value ? "" : query.value;
    if (tptpMode.value) {
      // Reuse the `parseTptpTest` RPC (built for the test-import workflow)
      // to split the single-pane problem into KIF text, then run the
      // ordinary KIF prove RPC against it.
      const { test } = await call("parseTptpTest", {
        name: "problem",
        text: asserted,
        remap: prover.useSumo,
      });
      asserted = test.axiomKif;
      asked = test.queryKif ?? "";
    }
    ({ result: r } = await call("prove", {
      assertions: asserted,
      query: asked,
      config,
      session: "user-assertions",
    }));
    lastVampireTptp = r?.input_tptp ?? null;
    showDownloadTptp.value = !!lastVampireTptp;
    error.value = "";
    result.value = r;
    resultBackend.value = backend;
    if (testActive.value && expectedProof.value !== null)
      outcome.value = gradeTest({ expectedProof: expectedProof.value }, r);
  } catch (e) {
    result.value = null;
    resultBackend.value = "";
    error.value = errMsg(e);
  } finally {
    proving.value = false;
  }
}

function downloadVampireTptp() {
  if (!lastVampireTptp) return;
  downloadText("prover-input.tptp", lastVampireTptp);
}

// -- tests: open / save -------------------------------------------------------

/** Load an imported test into the panes in its own dialect: a `.kif.tq`
 *  fills assertions + query with the parsed KIF, a `.p`/`.tptp` puts the
 *  whole problem text into TPTP mode's single pane. */
let loadedTestText: string | null = null;
function openTest(t: TestEntry) {
  loadedTestText = t.text;
  if (tests.dialect(t.name) === "kif") {
    prover.proofLang = "kif";
    assertions.value = t.parsed.axiomKif || "";
    query.value = t.parsed.queryKif || "";
    metaFromTest(t.parsed);
    metaOpen.value = true;
  } else {
    prover.proofLang = "tptp";
    assertions.value = t.text;
    query.value = "";
    resetMeta();
    metaOpen.value = false;
  }
  tests.setOpen({ name: t.name, origin: t.origin });
}

// The open test went away (removed on the Inference Tests tab): its details
// must not linger over the next ad-hoc query.
watch(
  () => tests.openTest,
  (open) => {
    if (open) return;
    resetMeta();
    metaOpen.value = false;
    outcome.value = null;
  },
);

onActivated(() => {
  const t = tests.openTest && tests.find(tests.openTest.name);
  if (t && t.text !== loadedTestText) openTest(t);
});

const { onQuery, str } = useTabQuery(["prover"]);

// `?test=<name>` opens an imported test (the Inference Tests tab's name link).
onQuery((q) => {
  const name = str(q.test);
  if (!name) return;
  const t = tests.find(name);
  if (t && (name !== tests.openTest?.name || t.text !== loadedTestText))
    openTest(t);
  else if (t) return;
  else cfgNote.value = `${name} is not imported (see the Inference Tests tab)`;
});

async function saveTest() {
  let text: string;
  if (tptpMode.value) {
    text = assertions.value;
    if (!text.trim()) {
      cfgNote.value = "Enter a problem first.";
      return;
    }
  } else {
    const q = query.value.trim();
    if (!q) {
      cfgNote.value = "Enter a query first.";
      return;
    }
    text = formatTest({
      note: meta.note.trim(),
      categories: splitList(meta.categories),
      timeout: meta.timeout > 0 ? Math.round(meta.timeout) : 0,
      extraFiles: splitList(meta.files),
      assertions: assertions.value,
      query: q,
      expectedProof: expectedProof.value,
      expectedAnswer:
        meta.answer === "bindings" ? meta.bindings.split(/\s+/) : null,
    });
  }
  savingTest.value = true;
  try {
    const r = await tests.saveCurrent(text, tptpMode.value ? "tptp" : "kif");
    if (!r.saved) return; // cancelled the save-as prompt
    loadedTestText = text;
    testLog.set(
      r.overwritten
        ? `Saved changes to ${r.name}.`
        : r.notices && r.notices.length
          ? r.notices.join(" | ")
          : `Saved as ${r.name}.`,
    );
  } catch (err) {
    testLog.fail(err);
  } finally {
    savingTest.value = false;
  }
}
</script>

<template>
  <Card>
    <div class="top">
      <Segmented
        v-model="prover.proofLang"
        :options="langOptions"
        label="Input language"
      />
      <span v-if="tests.openTest" class="hint editing">
        Editing test: {{ tests.openTest.name }}
        <button
          class="btn ghost small"
          type="button"
          @click="navigate('edit', { file: tests.openTest.name })"
        >
          Edit raw test
        </button>
      </span>
    </div>
    <label v-if="tptpMode" class="mt"
      >TPTP problem — axioms + an embedded <code>conjecture</code></label
    >
    <label v-else class="mt"
      >Assertions — <code>tell</code> (added to the KB for this query)</label
    >
    <div class="pane-editor" :class="{ 'pane-editor-tall': tptpMode }">
      <MonacoEditor
        ref="assertionsEd"
        v-model="assertions"
        compact
        :language="tptpMode ? 'tptp' : 'kif'"
        @ready="onEditorReady"
      />
    </div>
    <div v-show="!tptpMode">
      <label class="mt">Query — <code>ask</code></label>
      <div class="pane-editor pane-editor-sm">
        <MonacoEditor
          ref="queryEd"
          v-model="query"
          compact
          @ready="onEditorReady"
        />
      </div>
      <Disclosure
        :summary="metaSummary"
        :open="metaOpen"
        @toggle="metaOpen = $event"
      >
        <div class="meta-grid">
          <div>
            <label for="tqNote">note</label>
            <input
              id="tqNote"
              v-model="meta.note"
              type="text"
              placeholder="e.g. Astronomy_4"
            />
          </div>
          <div>
            <label for="tqCategories">categories</label>
            <input
              id="tqCategories"
              v-model="meta.categories"
              type="text"
              placeholder="comma-separated"
            />
          </div>
          <div>
            <label for="tqTime">time limit (s)</label>
            <input
              id="tqTime"
              v-model.number="meta.timeout"
              type="number"
              min="0"
              placeholder="prover setting"
            />
          </div>
          <div>
            <label for="tqAnswer">expected answer</label>
            <div class="answer">
              <select id="tqAnswer" v-model="meta.answer">
                <option value="yes">yes (provable)</option>
                <option value="no">no (not provable)</option>
                <option value="bindings">bindings</option>
                <option value="none">unspecified</option>
              </select>
              <input
                v-if="meta.answer === 'bindings'"
                v-model="meta.bindings"
                type="text"
                placeholder="e.g. Rex Fido"
                aria-label="Expected bindings"
              />
            </div>
          </div>
          <div>
            <label for="tqFiles">required files</label>
            <input
              id="tqFiles"
              v-model="meta.files"
              type="text"
              placeholder="e.g. Astronomy.kif"
            />
          </div>
        </div>
        <div class="hint sub">
          The <code>.kif.tq</code> directives Save test writes. A time limit
          here overrides the prover setting when proving this test.
        </div>
      </Disclosure>
    </div>
    <div v-show="tptpMode" class="mt-sm">
      <label class="check"
        ><input type="checkbox" v-model="prover.useSumo" /> Use SUMO
        <span class="hint"
          >— prove against all of SUMO as background axioms and decode
          SUMO-mangled symbol names; off proves the file standalone,
          unmangled</span
        ></label
      >
    </div>
    <div class="actions mt">
      <BusyButton
        :busy="proving"
        label="Prove"
        :title="`Prove (${proveKey})`"
        :busy-label="`Proving… ${elapsedLabel(runLimitSecs)}`"
        :progress="elapsedFraction(runLimitSecs)"
        @click="prove"
      />
      <button
        ref="moreBtn"
        class="btn ghost"
        type="button"
        aria-haspopup="menu"
        :aria-expanded="moreOpen"
        @click="moreOpen = !moreOpen"
      >
        More ▾
      </button>
      <ProverChips
        v-model:open="optionsOpen"
        profile="ask"
        :fixed="testTimeLimit ? [`${testTimeLimit}s from the test`] : []"
      />
      <span v-if="cfgNote" class="hint">{{ cfgNote }}</span>
    </div>
    <DropMenu v-model="moreOpen" :anchor="moreBtn">
      <template #default="{ close }">
        <button
          type="button"
          role="menuitem"
          :disabled="savingTest"
          @click="
            close();
            saveTest();
          "
        >
          Save as test
        </button>
        <button
          type="button"
          role="menuitem"
          @click="
            close();
            navigate('problems');
          "
        >
          Inference tests…
        </button>
        <button
          v-if="showDownloadTptp"
          type="button"
          role="menuitem"
          title="The exact TPTP input for the last external prover run"
          @click="
            close();
            downloadVampireTptp();
          "
        >
          Download the prover's TPTP input
        </button>
      </template>
    </DropMenu>
    <ProverOptions v-if="optionsOpen" profile="ask" :disabled="proving" />
    <StatusLine :text="testLog.text" :error="testLog.error" />
  </Card>

  <Card v-if="error || result">
    <div class="inline between">
      <div class="inline tight center">
        <span class="status" :class="error ? 'InputError' : result?.status">{{
          error ? "Error" : result?.status
        }}</span>
        <span class="hint">{{ backendBadge }}</span>
        <span class="hint">{{ stepsText }}</span>
        <span v-if="tookLabel" class="hint" title="Wall-clock time">{{
          tookLabel
        }}</span>
        <span
          v-if="outcome"
          class="test-outcome"
          :class="outcome.cls"
          :title="`Graded against the expected answer (${meta.answer})`"
          >expected {{ meta.answer === "no" ? "no" : "yes" }} ·
          {{ outcome.label }}</span
        >
      </div>
      <label class="check"
        ><input type="checkbox" v-model="prover.plainProof" /> Plain
        Proof</label
      >
    </div>
    <ProofView
      v-if="result"
      :steps="result.proof || []"
      :prologue="result.proof_tptp_prologue"
      :prose="result.prose"
      :prose-missing="result.prose_missing"
      :graphviz="result.graphviz"
      :raw-output="result.raw_output"
    />
  </Card>
</template>

<style scoped>
.pane-editor {
  height: 96px;
  border: 1px solid var(--line);
  border-radius: 7px;
  overflow: hidden;
}
.meta-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(180px, 1fr));
  gap: 10px 14px;
  margin-top: 8px;
}
.meta-grid .answer {
  display: flex;
  gap: 6px;
}
.meta-grid select {
  min-width: 0;
}
.sub {
  font-size: 12px;
  margin-top: 6px;
}
.test-outcome {
  font-size: 12px;
  font-weight: 600;
  padding: 2px 9px;
  border-radius: 20px;
  background: color-mix(in srgb, var(--muted) 16%, transparent);
  color: var(--muted);
}
.test-outcome.ok {
  background: color-mix(in srgb, var(--ok) 18%, transparent);
  color: var(--ok);
}
.test-outcome.bad {
  background: color-mix(in srgb, var(--bad) 18%, transparent);
  color: var(--bad);
}
.pane-editor-sm {
  height: 56px;
}
.pane-editor-tall {
  height: 320px;
}
.top {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
}
.editing {
  display: inline-flex;
  align-items: center;
  gap: 8px;
}
button.small {
  height: auto;
  padding: 4px 10px;
  font-size: 13px;
}
.actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 10px;
}
</style>
