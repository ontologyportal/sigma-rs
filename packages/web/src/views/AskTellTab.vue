<script setup lang="ts">
import { computed, onBeforeUnmount, ref, shallowRef, watch } from "vue";
import { formatTest } from "sigmakee/sdk";
import { call } from "../services/sigma";
import { downloadText, errMsg } from "../utils/format";
import { useProverStore } from "../stores/prover";
import { useTestsStore, type TestEntry } from "../stores/tests";
import BusyButton from "../components/BusyButton.vue";
import Card from "../components/Card.vue";
import MonacoEditor from "../components/MonacoEditor.vue";
import ProofView from "../components/ProofView.vue";
import ProverSettings from "../components/ProverSettings.vue";
import TestsPanel from "../components/prover/TestsPanel.vue";

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
const testsOpen = ref(false);
const loadingTest = ref(false);
const savingTest = ref(false);
const testsLog = ref("");
/** Transient note shown in place of the settings summary ("Enter a query first."). */
const cfgNote = ref("");
const loadTestFile = ref<HTMLInputElement | null>(null);

// The exact TPTP problem text handed to Vampire for the most recent Ask/Tell
// run -- `proveVampire` returns it alongside the result (computed anyway to
// run the query, previously discarded); the download button just hands back
// what's already in memory, no extra worker round-trip.
let lastVampireTptp: string | null = null;
const showDownloadTptp = ref(false);

const result = shallowRef<any | null>(null);
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

function onEditorReady() {
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
watch(
  () => prover.cfgSummary,
  () => {
    cfgNote.value = "";
  },
);
onBeforeUnmount(() => clearTimeout(scratchTimer));

// -- prove --------------------------------------------------------------------

async function prove() {
  const vampire = prover.vampireSelected;
  proving.value = true;
  lastVampireTptp = null;
  showDownloadTptp.value = false;
  try {
    let r: any;
    let asserted = assertions.value.trim();
    let asked = tptpMode.value ? "" : query.value;
    if (tptpMode.value) {
      // Reuse the `parseTptpTest` RPC (built for the test-import workflow)
      // to split the single-pane problem into KIF text, then run the
      // ordinary KIF prove/proveVampire RPCs against it.
      const { test } = await call("parseTptpTest", {
        name: "problem",
        text: asserted,
        remap: prover.useSumo,
      });
      asserted = test.axiomKif;
      asked = test.queryKif;
    }
    if (vampire) {
      const res = await call("proveVampire", {
        assertions: asserted,
        query: asked,
        timeLimitSecs: prover.config().timeLimitSecs,
        selectionTolerancePct: prover.config().selectionTolerancePct,
        extraArgs: prover.vampireArgs.trim(),
      });
      r = res.result;
      lastVampireTptp = res.tptp;
      showDownloadTptp.value = true;
    } else {
      ({ result: r } = await call("prove", {
        assertions: asserted,
        query: asked,
        config: prover.config(),
        session: "user-assertions",
      }));
    }
    error.value = "";
    result.value = r;
    resultBackend.value = vampire ? "Vampire" : "SUPr";
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
  downloadText("vampire-input.tptp", lastVampireTptp);
}

// -- tests: open / load / save ------------------------------------------------

/** Load a parsed test's fields into the panes. Every test's panes carry KIF
 *  text regardless of source dialect (`.p`/`.tptp` imports come back
 *  translated); if the proof-language toggle is on TPTP's single-pane mode,
 *  switch it back to KIF, or that text would be misread as one whole TPTP
 *  problem with its query pane hidden. */
function openParsed(parsed: any) {
  assertions.value = parsed.axiomKif || "";
  query.value = parsed.queryKif || "";
  if (prover.proofLang === "tptp") prover.proofLang = "kif";
}

function onOpenTest(t: TestEntry) {
  openParsed(t.parsed);
  tests.setOpen({ name: t.name, origin: t.origin });
  testsOpen.value = false;
}

async function onLoadTestFile(e: Event) {
  const input = e.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = ""; // allow re-picking the same file name later
  if (!file) return;
  loadingTest.value = true;
  try {
    const r = await tests.loadFile(file.name, await file.text());
    if (r.parsed) openParsed(r.parsed);
    testsLog.value = r.added
      ? `Imported and opened ${file.name}.`
      : r.notices.join(" | ");
  } catch (err) {
    testsLog.value = errMsg(err);
  } finally {
    loadingTest.value = false;
  }
}

async function saveTest() {
  const q = query.value.trim();
  if (!q) {
    cfgNote.value = "Enter a query first.";
    return;
  }
  const formatted = formatTest({
    timeout: prover.config().timeLimitSecs,
    assertions: assertions.value,
    query: q,
    expectedProof: true,
  });
  savingTest.value = true;
  try {
    const r = await tests.saveCurrent(formatted);
    if (!r.saved) return; // cancelled the save-as prompt
    testsLog.value = r.overwritten
      ? `Saved changes to ${r.name}.`
      : r.notices && r.notices.length
        ? r.notices.join(" | ")
        : `Saved as ${r.name}.`;
  } catch (err) {
    testsLog.value = errMsg(err);
  } finally {
    savingTest.value = false;
  }
}
</script>

<template>
  <Card>
    <label v-if="tptpMode"
      >TPTP problem — axioms + an embedded <code>conjecture</code>, read as one
      document</label
    >
    <label v-else
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
    <div v-show="tptpMode">
      <label class="check"
        ><input type="checkbox" v-model="prover.useSumo" /> Use SUMO
        <span class="hint"
          >— prove against all of SUMO as background axioms and decode
          SUMO-mangled symbol names; off proves the file standalone,
          unmangled</span
        ></label
      >
    </div>
    <div class="inline tight center mt">
      <BusyButton
        :busy="proving"
        label="Prove"
        busy-label="Proving…"
        @click="prove"
      />
      <button
        class="btn ghost"
        type="button"
        :aria-expanded="testsOpen"
        title="Open a previously imported test"
        @click="testsOpen = !testsOpen"
      >
        Open test
      </button>
      <BusyButton
        ghost
        :busy="loadingTest"
        label="Load test"
        title="Import a new .kif.tq or .p/.tptp test file"
        @click="loadTestFile?.click()"
      />
      <input
        ref="loadTestFile"
        type="file"
        accept=".tq,.p,.tptp,text/plain"
        hidden
        @change="onLoadTestFile"
      />
      <BusyButton
        ghost
        :busy="savingTest"
        label="Save test"
        title="Save the current assertions/query as a test"
        @click="saveTest"
      />
      <button
        class="btn ghost"
        type="button"
        v-show="showDownloadTptp"
        title="Download the exact TPTP problem text handed to Vampire for the last run"
        @click="downloadVampireTptp"
      >
        Download TPTP input
      </button>
      <button
        class="cog"
        type="button"
        title="Prover settings"
        aria-label="Prover settings"
        :aria-expanded="prover.settingsOpen"
        @click="prover.toggleSettings()"
      >
        ⚙
      </button>
      <span class="hint">{{ cfgNote || prover.cfgSummary }}</span>
    </div>
    <div class="hint mt-sm">
      {{ tests.openTest ? "Editing test: " + tests.openTest.name : "" }}
    </div>
  </Card>

  <ProverSettings />

  <TestsPanel v-show="testsOpen" v-model:log="testsLog" @open="onOpenTest" />

  <Card v-if="error || result">
    <div class="inline between">
      <div class="inline tight center">
        <span class="status" :class="error ? 'InputError' : result.status">{{
          error ? "Error" : result.status
        }}</span>
        <span class="hint">{{ backendBadge }}</span>
        <span class="hint">{{ stepsText }}</span>
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
</style>
