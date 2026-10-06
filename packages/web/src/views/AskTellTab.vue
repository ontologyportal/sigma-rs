<script setup lang="ts">
import { computed, onActivated, ref, shallowRef, watch } from "vue";
import { AskResult, formatTest } from "sigmakee/sdk";
import { useTabQuery } from "../composables/useTabQuery";
import { useStatus } from "../composables/useStatus";
import { navigate } from "../router";
import { call } from "../services/sigma";
import { downloadText, errMsg } from "../utils/format";
import { profileDefaults, useProverStore } from "../stores/prover";
import {
  gradeTest,
  useTestsStore,
  type TestEntry,
  type TestOutcome,
} from "../stores/tests";
import type * as Monaco from "monaco-editor/esm/vs/editor/editor.api.js";
import { useScratchValidation } from "../composables/useScratchValidation";
import {
  emptyTestMeta,
  expectedProof,
  metaFromTest,
  testDirectives,
} from "../utils/testMeta";
import TestDetails from "../components/asktell/TestDetails.vue";
import type { MonacoNs } from "../services/monaco";
import BusyButton from "../components/BusyButton.vue";
import { useElapsed } from "../composables/useElapsed";
import Card from "../components/Card.vue";
import MonacoEditor from "../components/MonacoEditor.vue";
import ProofView from "../components/ProofView.vue";
import ProverChips from "../components/ProverChips.vue";
import ProverOptions from "../components/ProverOptions.vue";
import ProverSideStack from "../components/ProverSideStack.vue";
import ProofHistory from "../components/ProofHistory.vue";
import HistoryToggle from "../components/HistoryToggle.vue";
import {
  countForms,
  useAskHistoryStore,
  type ProofRun,
} from "../stores/runHistory";
import Row from "../components/Row.vue";
import Col from "../components/Col.vue";
import { useShellStore } from "../stores/shell";
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
const history = useAskHistoryStore();
/** Comfortable layout: the run history toggles open under the form. */
const historyOpen = ref(false);
const shell = useShellStore();
/** Comfortable layout: options toggle open under the form; classic: they sit
 *  in a column on the right. */
const isCompact = computed(() => shell.isCompact);

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

/** Edited beside the panes, filled from an opened test, and written back by
 *  Save test (see utils/testMeta). */
const meta = ref(emptyTestMeta());
const metaOpen = ref(false);

/** The details apply while a test is being worked on -- one is open, or the
 *  user expanded the section to write one -- never to ad-hoc queries. */
const testActive = computed(
  () => !tptpMode.value && (!!tests.openTest || metaOpen.value),
);
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

const { editorReady, paneErrors } = useScratchValidation({
  assertions,
  query,
  assertionsEd,
  queryEd,
  enabled: computed(() => !tptpMode.value),
});

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
  editorReady();
}

watch(
  () => prover.backend,
  () => {
    showDownloadTptp.value = false;
  },
);

// -- prove --------------------------------------------------------------------

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
    const config = prover.config("ask");
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
    recordRun(asked, asserted, r.status, backend);
    const expected = expectedProof(meta.value);
    if (testActive.value && expected !== null)
      outcome.value = gradeTest({ expectedProof: expected }, r);
  } catch (e) {
    result.value = null;
    resultBackend.value = "";
    error.value = errMsg(e);
    recordRun("", "", "Error", backend);
  } finally {
    proving.value = false;
  }
}

/** Add this run to the history: the panes as they are, plus the goal and
 *  the told formulas in KIF (`goal`/`told` empty when the run failed before
 *  a TPTP problem was translated). */
function recordRun(
  goal: string,
  told: string,
  status: string,
  backend: string,
) {
  history.record({
    lang: tptpMode.value ? "tptp" : "kif",
    assertions: assertions.value,
    query: tptpMode.value ? "" : query.value,
    goal: goal.trim() || (tptpMode.value ? "" : query.value.trim()),
    told: countForms(told || (tptpMode.value ? "" : assertions.value)),
    status,
    backend,
  });
}

/** Put an earlier run back in the panes (no longer editing a test). */
function restoreRun(run: ProofRun) {
  tests.setOpen(null);
  prover.proofLang = run.lang;
  assertions.value = run.assertions;
  query.value = run.query;
  historyOpen.value = false;
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
    meta.value = metaFromTest(t.parsed);
    if (t.parsed.timeGiven)
      prover.profiles.ask.timeLimitSecs = t.parsed.timeout;
    metaOpen.value = true;
  } else {
    prover.proofLang = "tptp";
    assertions.value = t.text;
    query.value = "";
    meta.value = emptyTestMeta();
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
    meta.value = emptyTestMeta();
    metaOpen.value = false;
    outcome.value = null;
  },
);

onActivated(() => {
  const t = tests.openTest && tests.find(tests.openTest.name);
  if (t && t.text !== loadedTestText) openTest(t);
});

const { onQuery, query: urlQuery, str } = useTabQuery(["prover"]);

// `?test=<name>` opens a test (the Inference Tests tab's name link), fetching
// it from the library if it hasn't been loaded. On a fresh page load the
// library is still being listed: wait for that before deciding the test
// doesn't exist.
onQuery(async (q) => {
  const name = str(q.test);
  if (!name) return;
  let t = tests.find(name);
  if (!t) {
    testLog.set(`Loading test ${name}…`);
    try {
      await tests.whenLoaded();
      t = await tests.ensure(name);
    } catch (e) {
      if (str(urlQuery.value.test) === name)
        testLog.set(`Could not open ${name}: ${errMsg(e)}`, true);
      return;
    }
    if (str(urlQuery.value.test) !== name) return; // the URL moved on meanwhile
    testLog.clear();
  }
  if (name !== tests.openTest?.name || t.text !== loadedTestText) openTest(t);
});

/** The `(time N)` a saved test carries: the time limit, when it isn't the
 *  default (a test without one runs at the Inference Tests default). */
function testTime(): number {
  const secs = Math.round(prover.profiles.ask.timeLimitSecs);
  return secs > 0 && secs !== profileDefaults("ask").timeLimitSecs ? secs : 0;
}

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
      ...testDirectives(meta.value),
      timeout: testTime(),
      assertions: assertions.value,
      query: q,
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
  <Row>
    <Col :span="isCompact ? 12 : 8">
      <Card>
        <div class="top">
          <Segmented
            v-model="prover.proofLang"
            :options="langOptions"
            label="Input language"
          />
          <div class="test-actions">
            <button
              class="btn ghost small"
              type="button"
              title="Pick a test on the Inference Tests tab"
              @click="navigate('problems')"
            >
              Browse tests
            </button>
            <button
              class="btn ghost small"
              type="button"
              :disabled="savingTest"
              :title="
                tests.openTest
                  ? `Save changes to ${tests.openTest.name}`
                  : 'Save the panes as a new inference test'
              "
              @click="saveTest"
            >
              {{ tests.openTest ? "Save test" : "Save as test" }}
            </button>
          </div>
        </div>
        <div v-if="tests.openTest" class="open-test">
          <span class="hint">Editing</span>
          <code>{{ tests.openTest.name }}</code>
          <a
            href="#"
            @click.prevent="navigate('edit', { file: tests.openTest.name })"
            >Edit raw</a
          >
        </div>

        <div class="pane-label">
          <template v-if="tptpMode">
            <span>TPTP problem</span>
            <span class="hint"
              >axioms + an embedded <code>conjecture</code></span
            >
          </template>
          <template v-else>
            <span>Assertions</span>
            <span class="hint"
              ><code>tell</code> — added to the KB for this query</span
            >
          </template>
        </div>
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
          <div class="pane-label">
            <span>Query</span>
            <span class="hint"><code>ask</code></span>
          </div>
          <div class="pane-editor pane-editor-sm">
            <MonacoEditor
              ref="queryEd"
              v-model="query"
              compact
              @ready="onEditorReady"
            />
          </div>
          <TestDetails v-model="meta" v-model:open="metaOpen" class="details" />
        </div>
        <label v-show="tptpMode" class="check mt-sm"
          ><input type="checkbox" v-model="prover.useSumo" /> Use SUMO
          <span class="hint"
            >— prove against all of SUMO as background axioms and decode
            SUMO-mangled symbol names; off proves the file standalone,
            unmangled</span
          ></label
        >

        <div class="actions">
          <BusyButton
            :busy="proving"
            label="Prove"
            :title="`Prove (${proveKey})`"
            :busy-label="`Proving… ${elapsedLabel(runLimitSecs)}`"
            :progress="elapsedFraction(runLimitSecs)"
            @click="prove"
          />
          <ProverChips
            v-model:open="optionsOpen"
            profile="ask"
            :toggle="isCompact"
          />
          <HistoryToggle v-if="isCompact" v-model:open="historyOpen" />
          <span v-if="cfgNote" class="hint">{{ cfgNote }}</span>
          <a
            v-if="showDownloadTptp"
            class="download"
            href="#"
            title="The exact TPTP input for the last external prover run"
            @click.prevent="downloadVampireTptp"
            >Download TPTP input</a
          >
        </div>
        <ProverOptions
          v-if="isCompact && optionsOpen"
          profile="ask"
          :disabled="proving"
        />
        <ProofHistory
          v-if="isCompact && historyOpen"
          class="history-panel"
          feed="ask"
          @pick="restoreRun"
        />
        <StatusLine :text="testLog.text" :error="testLog.error" />
      </Card>

      <Card v-if="error || result">
        <div class="inline between">
          <div class="inline tight center">
            <span
              class="status"
              :class="error ? 'InputError' : result?.status"
              >{{ error ? "Error" : result?.status }}</span
            >
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
    </Col>
    <Col v-if="!isCompact" :span="4" style="margin-left: 15px">
      <ProverSideStack
        profile="ask"
        feed="ask"
        :disabled="proving"
        @pick="restoreRun"
      />
    </Col>
  </Row>
</template>

<style scoped>
.pane-editor {
  height: 96px;
  border: 1px solid var(--line);
  border-radius: 7px;
  overflow: hidden;
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
.test-actions {
  display: flex;
  gap: 6px;
}
/* The open test's name, as a slim bar under the header. */
.open-test {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px;
  margin-top: 10px;
  padding: 6px 10px;
  border-radius: 7px;
  font-size: 13px;
  background: color-mix(in srgb, var(--accent) 8%, transparent);
}
.open-test code {
  word-break: break-all;
}
/* An editor's label: its name, then a muted gloss. */
.pane-label {
  display: flex;
  align-items: baseline;
  flex-wrap: wrap;
  gap: 8px;
  margin: 14px 0 6px;
  font-size: 13px;
  font-weight: 600;
}
.pane-label .hint {
  font-weight: normal;
}
.details {
  margin-top: 12px;
}
.download {
  margin-left: auto;
  font-size: 13px;
}
.history-panel {
  margin-top: 12px;
}
/* Prove and what it will run with, set off from the inputs above. */
.actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 10px;
  margin-top: 16px;
  padding-top: 14px;
  border-top: 1px solid var(--line);
}
</style>
