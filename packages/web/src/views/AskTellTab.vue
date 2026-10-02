<script setup lang="ts">
import {
  computed,
  onActivated,
  onBeforeUnmount,
  ref,
  shallowRef,
  watch,
} from "vue";
import { AskResult, formatTest } from "sigmakee/sdk";
import { useTabQuery } from "../composables/useTabQuery";
import { useStatus } from "../composables/useStatus";
import { navigate } from "../router";
import { call } from "../services/sigma";
import { downloadText, errMsg } from "../utils/format";
import { useProverStore } from "../stores/prover";
import { useTestsStore, type TestEntry } from "../stores/tests";
import type * as Monaco from "monaco-editor/esm/vs/editor/editor.api.js";
import type { MonacoNs } from "../services/monaco";
import BusyButton from "../components/BusyButton.vue";
import { useElapsed } from "../composables/useElapsed";
import Card from "../components/Card.vue";
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
  editor.addCommand(m.KeyMod.CtrlCmd | m.KeyCode.Enter, () => {
    if (!proving.value) prove();
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

async function prove() {
  const backend = prover.backendLabel;
  cfgNote.value = "";
  proving.value = true;
  lastVampireTptp = null;
  showDownloadTptp.value = false;
  try {
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
  } else {
    prover.proofLang = "tptp";
    assertions.value = t.text;
    query.value = "";
  }
  tests.setOpen({ name: t.name, origin: t.origin });
}

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
      timeout: prover.profiles.ask.timeLimitSecs,
      assertions: assertions.value,
      query: q,
      expectedProof: true,
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
      <ProverChips v-model:open="optionsOpen" profile="ask" />
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
