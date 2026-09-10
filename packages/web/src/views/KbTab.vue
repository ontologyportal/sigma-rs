<script setup lang="ts">
/** Knowledge base tab: standard constituent sets, the loaded list, the
 *  WordNet panel, and the three import channels (upstream picker, URL,
 *  local upload). */
import { onActivated, onMounted, ref } from "vue";
import { GitOrigin, LocalOrigin, RemoteOrigin } from "../models/Origin";
import { fetchText, fetchAllTexts } from "../services/sources";
import { useKBStore } from "../stores/kb";
import { useBootStore } from "../stores/boot";
import { useTestsStore, isTestFile } from "../stores/tests";
import { useStatus } from "../composables/useStatus";
import { errMsg } from "../utils/format";
import BusyButton from "../components/BusyButton.vue";
import Card from "../components/Card.vue";
import StatusLine from "../components/StatusLine.vue";
import ConstituentList from "../components/kb/ConstituentList.vue";
import WordNetPanel from "../components/kb/WordNetPanel.vue";
import SumoPicker from "../components/kb/SumoPicker.vue";

const kb = useKBStore();
const boot = useBootStore();
const tests = useTestsStore();

onMounted(() => kb.loadSumoCatalog());
onActivated(() => kb.loadSumoCatalog());

// -- Standard constituent sets ------------------------------------------------
//
// File lists mirror the Sigma XML configuration. Order is preserved from it:
// ingest is order-independent here (everything is promoted together at the
// end), but keeping it makes the two lists diffable against the source.

const PRESETS: Record<string, { label: string; files: string[] }> = {
  minimal: {
    label: "Minimal SUMO",
    files: [
      "Merge.kif",
      "Mid-level-ontology.kif",
      "english_format.kif",
      "domainEnglishFormat.kif",
    ],
  },
  full: {
    label: "Full SUMO",
    files: [
      "english_format.kif",
      "domainEnglishFormat.kif",
      "ArabicCulture.kif",
      "Anatomy.kif",
      "arteries.kif",
      "Biography.kif",
      "Cars.kif",
      "Catalog.kif",
      "Communications.kif",
      "ComputerInput.kif",
      "ComputingBrands.kif",
      "CountriesAndRegions.kif",
      "Dining.kif",
      "Economy.kif",
      "emotion.kif",
      "engineering.kif",
      "Facebook.kif",
      "FinancialOntology.kif",
      "Food.kif",
      "Geography.kif",
      "Government.kif",
      "Hotel.kif",
      "Justice.kif",
      "Languages.kif",
      "Law.kif",
      "Media.kif",
      "Medicine.kif",
      "Merge.kif",
      "Mid-level-ontology.kif",
      "MilitaryDevices.kif",
      "Military.kif",
      "MilitaryPersons.kif",
      "MilitaryProcesses.kif",
      "Music.kif",
      "naics.kif",
      "People.kif",
      "pictureList.kif",
      "pictureList-ImageNet.kif",
      "QoSontology.kif",
      "Sports.kif",
      "TransnationalIssues.kif",
      "Transportation.kif",
      "TransportDetail.kif",
      "UXExperimentalTerms.kif",
      "VirusProteinAndCellPart.kif",
      "Weather.kif",
      "WMD.kif",
      "capabilities.kif",
    ],
  },
};

const presetsBusy = ref(false);
const presetNote = useStatus();

async function loadPreset(key: string) {
  const preset = PRESETS[key];
  presetsBusy.value = true;
  try {
    // A preset describes a whole KB, so it replaces rather than merges.
    await kb.replaceAll();

    const total = preset.files.length;
    presetNote.set(`Fetching ${preset.label} — 0/${total}…`);
    const texts = await fetchAllTexts(preset.files, 6, (n) => {
      presetNote.set(`Fetching ${preset.label} — ${n}/${total}…`);
    });

    const failed: string[] = [];
    for (let i = 0; i < preset.files.length; i++) {
      const name = preset.files[i];
      const text = texts[i];
      if (text instanceof Error) {
        failed.push(`${name}: ${text.message}`);
        continue;
      }
      presetNote.set(`Reading ${name} (${i + 1}/${total})…`);
      try {
        await kb.ingest(name, text, GitOrigin.default());
      } catch (e) {
        failed.push(`${name}: ${errMsg(e)}`);
      }
    }
    presetNote.set(`Axiomatizing ${kb.constituents.length} constituent(s)…`);
    await kb.reprocess();

    presetNote.set(
      failed.length
        ? `${preset.label}: loaded ${kb.constituents.length}/${total}, ${failed.length} failed — ${failed[0]}`
        : `${preset.label} loaded — ${kb.constituents.length} constituents.`,
      failed.length > 0,
    );
  } catch (e) {
    presetNote.fail(e);
  } finally {
    presetsBusy.value = false;
  }
}

// -- Import channels ----------------------------------------------------------

const kbLog = useStatus();
const addSumoBusy = ref(false);
const addUrlBusy = ref(false);
const kbUrl = ref("");

function clearLog() {
  kbLog.clear();
}

async function addSumoSelected(paths: string[]) {
  if (!paths.length) {
    kbLog.set("Select one or more files first.");
    return;
  }
  addSumoBusy.value = true;
  try {
    // Ingest (fetch + parse) under the busy button -- no toast yet. Fetches
    // run batched, like the presets: a multi-select of a dozen files is
    // otherwise a dozen serial round-trips.
    let added = 0;
    let notices = 0;
    const failed: string[] = [];
    const texts = await fetchAllTexts(paths, 6, (n) => {
      kbLog.set(`Fetching — ${n}/${paths.length}…`);
    });
    for (let i = 0; i < paths.length; i++) {
      const path = paths[i];
      const text = texts[i];
      if (text instanceof Error) {
        failed.push(`${path}: ${text.message}`);
        continue;
      }
      try {
        const r = isTestFile(path)
          ? await tests.add(path, text, "sumo")
          : await kb.ingest(path, text, GitOrigin.default());
        if (r.added) added += 1;
        notices += r.notices.length;
      } catch (err) {
        failed.push(`${path}: ${errMsg(err)}`);
      }
    }
    kbLog.set(
      `Ingested ${added}/${paths.length} constituent(s); axiomatizing…`,
    );
    await kb.reprocess();
    if (failed.length) {
      kbLog.set(
        `Added ${added}/${paths.length}; ${failed.length} failed — ${failed[0]}`,
        true,
      );
    } else {
      kbLog.set(
        `Added ${added}/${paths.length} constituent(s)` +
          (notices ? ` (${notices} load notice(s))` : "") +
          ".",
      );
    }
  } finally {
    addSumoBusy.value = false;
  }
}

async function addUrl() {
  const url = kbUrl.value.trim();
  if (!url) {
    kbLog.set("Enter a URL first.");
    return;
  }
  addUrlBusy.value = true;
  try {
    const text = await fetchText(url);
    if (isTestFile(url)) {
      const r = await tests.add(url, text, "url");
      kbLog.set(r.added ? `Imported test ${url}.` : r.notices.join(" | "));
      return;
    }
    const r = await kb.ingest(url, text, new RemoteOrigin());
    kbLog.set(
      r.added ? `Ingested ${url}; axiomatizing…` : r.notices.join(" | "),
    );
    if (r.added) await kb.reprocess();
  } finally {
    addUrlBusy.value = false;
  }
}

async function uploadKbFile(file: File) {
  addUrlBusy.value = true; // shares the "Add URL" busy indicator
  try {
    const text = await file.text();
    if (!boot.opfsRoot) throw new Error("File system not yet initialized");
    const handle = await boot.opfsRoot.getFileHandle(file.name, {
      create: true,
    });
    const stream = await handle.createWritable();
    await stream.write(text);
    await stream.close();
    // Test files (.kif.tq / .p / .tptp) import through the Ask/Tell tab's
    // "Load test" instead -- this uploader is KIF-constituent-only.
    const r = await kb.ingest(file.name, text, new LocalOrigin());
    kbLog.set(
      r.added ? `Ingested ${file.name}; axiomatizing…` : r.notices.join(" | "),
    );
    if (r.added) await kb.reprocess();
  } finally {
    addUrlBusy.value = false;
  }
}

function onKbFileChange(e: Event) {
  const input = e.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = "";
  if (file) uploadKbFile(file);
}
</script>

<template>
  <Card
    title="Load a standard set"
    description="Replaces everything currently loaded."
  >
    <template #header>
      <div class="inline tight">
        <button
          class="btn"
          type="button"
          :disabled="presetsBusy"
          @click="loadPreset('minimal')"
        >
          Minimal SUMO
        </button>
        <button
          class="btn"
          type="button"
          :disabled="presetsBusy"
          @click="loadPreset('full')"
        >
          Full SUMO
        </button>
      </div>
    </template>
    <StatusLine
      class="note"
      :text="presetNote.text"
      :error="presetNote.error"
    />
  </Card>

  <Card><ConstituentList @removed="clearLog" /></Card>

  <Card title="WordNet lexicon">
    <template #description>
      Powers synonym-aware search (results tagged "wn") — fetched from the same
      <code>ontologyportal/sumo</code> repo as the KIF constituents above.
    </template>
    <WordNetPanel />
  </Card>

  <Card><SumoPicker :busy="addSumoBusy" @add="addSumoSelected" /></Card>

  <Card>
    <label>Add a constituent from elsewhere</label>
    <div class="inline url-row">
      <input
        v-model="kbUrl"
        type="text"
        class="url-input"
        placeholder="https://example.org/ontology.kif"
      />
      <BusyButton :busy="addUrlBusy" label="Add URL" @click="addUrl" />
    </div>
    <div class="mt">
      <label for="kbFile">Upload a .kif file</label>
      <input
        id="kbFile"
        type="file"
        accept=".kif,.txt,text/plain"
        @change="onKbFileChange"
      />
    </div>
  </Card>

  <StatusLine :text="kbLog.text" :error="kbLog.error" />
</template>

<style scoped>
.note {
  margin-top: 8px;
}
.url-row {
  flex-wrap: nowrap;
}
.url-input {
  flex: 1 1 auto;
  width: auto;
  min-width: 0;
}
</style>
