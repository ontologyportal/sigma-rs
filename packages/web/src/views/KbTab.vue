<script setup lang="ts">
/** Knowledge base tab: standard constituent sets, the loaded list, the
 *  WordNet panel, and the three import channels (upstream picker, URL,
 *  local upload). */
import { onActivated, onMounted, reactive, ref } from "vue";
import { GitOrigin, LocalOrigin, RemoteOrigin } from "../models/Origin";
import { fetchText, fetchAllTexts } from "../services/sources";
import { useKBStore } from "../stores/kb";
import { useBootStore } from "../stores/boot";
import { useTestsStore, isTestFile } from "../stores/tests";
import { errMsg } from "../utils/format";
import Card from "../components/Card.vue";
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
const presetNote = reactive({ text: "", isError: false });

async function loadPreset(key: string) {
  const preset = PRESETS[key];
  presetsBusy.value = true;
  try {
    // A preset describes a whole KB, so it replaces rather than merges.
    await kb.replaceAll();

    const total = preset.files.length;
    presetNote.isError = false;
    presetNote.text = `Fetching ${preset.label} — 0/${total}…`;
    const texts = await fetchAllTexts(preset.files, 6, (n) => {
      presetNote.text = `Fetching ${preset.label} — ${n}/${total}…`;
    });

    const failed: string[] = [];
    for (let i = 0; i < preset.files.length; i++) {
      const name = preset.files[i];
      const text = texts[i];
      if (text instanceof Error) {
        failed.push(`${name}: ${text.message}`);
        continue;
      }
      presetNote.text = `Reading ${name} (${i + 1}/${total})…`;
      try {
        await kb.ingest(name, text, GitOrigin.default());
      } catch (e) {
        failed.push(`${name}: ${errMsg(e)}`);
      }
    }
    presetNote.text = `Axiomatizing ${kb.constituents.length} constituent(s)…`;
    await kb.reprocess();

    presetNote.isError = failed.length > 0;
    presetNote.text = failed.length
      ? `${preset.label}: loaded ${kb.constituents.length}/${total}, ${failed.length} failed — ${failed[0]}`
      : `${preset.label} loaded — ${kb.constituents.length} constituents.`;
  } catch (e) {
    presetNote.isError = true;
    presetNote.text = errMsg(e);
  } finally {
    presetsBusy.value = false;
  }
}

// -- Import channels ----------------------------------------------------------

const kbLog = reactive({ text: "", isError: false });
const addSumoBusy = ref(false);
const addUrlBusy = ref(false);
const kbUrl = ref("");

function clearLog() {
  kbLog.text = "";
  kbLog.isError = false;
}

async function addSumoSelected(paths: string[]) {
  if (!paths.length) {
    kbLog.text = "Select one or more files first.";
    kbLog.isError = false;
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
    kbLog.isError = false;
    const texts = await fetchAllTexts(paths, 6, (n) => {
      kbLog.text = `Fetching — ${n}/${paths.length}…`;
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
    kbLog.text = `Ingested ${added}/${paths.length} constituent(s); axiomatizing…`;
    await kb.reprocess();
    if (failed.length) {
      kbLog.isError = true;
      kbLog.text = `Added ${added}/${paths.length}; ${failed.length} failed — ${failed[0]}`;
    } else {
      kbLog.isError = false;
      kbLog.text =
        `Added ${added}/${paths.length} constituent(s)` +
        (notices ? ` (${notices} load notice(s))` : "") +
        ".";
    }
  } finally {
    addSumoBusy.value = false;
  }
}

async function addUrl() {
  const url = kbUrl.value.trim();
  if (!url) {
    kbLog.text = "Enter a URL first.";
    kbLog.isError = false;
    return;
  }
  addUrlBusy.value = true;
  try {
    const text = await fetchText(url);
    kbLog.isError = false;
    if (isTestFile(url)) {
      const r = await tests.add(url, text, "url");
      kbLog.text = r.added ? `Imported test ${url}.` : r.notices.join(" | ");
      return;
    }
    const r = await kb.ingest(url, text, new RemoteOrigin());
    kbLog.text = r.added
      ? `Ingested ${url}; axiomatizing…`
      : r.notices.join(" | ");
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
    kbLog.isError = false;
    // Test files (.kif.tq / .p / .tptp) import through the Ask/Tell tab's
    // "Load test" instead -- this uploader is KIF-constituent-only.
    const r = await kb.ingest(file.name, text, new LocalOrigin());
    kbLog.text = r.added
      ? `Ingested ${file.name}; axiomatizing…`
      : r.notices.join(" | ");
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
  <Card>
    <div class="inline presets">
      <div>
        <div class="title">Load a standard set</div>
        <div class="hint">Replaces everything currently loaded.</div>
      </div>
      <div class="inline preset-buttons">
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
    </div>
    <div class="hint note" :class="{ bad: presetNote.isError }">
      {{ presetNote.text }}
    </div>
  </Card>

  <Card><ConstituentList @removed="clearLog" /></Card>

  <Card><WordNetPanel /></Card>

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
      <button class="btn" type="button" :disabled="addUrlBusy" @click="addUrl">
        {{ addUrlBusy ? "Working…" : "Add URL" }}
      </button>
    </div>
    <div class="upload">
      <label for="kbFile">Upload a .kif file</label>
      <input
        id="kbFile"
        type="file"
        accept=".kif,.txt,text/plain"
        @change="onKbFileChange"
      />
    </div>
  </Card>

  <div class="hint" :class="{ bad: kbLog.isError }">{{ kbLog.text }}</div>
</template>

<style scoped>
.presets {
  justify-content: space-between;
  gap: 10px;
  flex-wrap: wrap;
}
.preset-buttons {
  gap: 8px;
}
.title {
  font-weight: 600;
}
.note {
  margin-top: 8px;
}
.bad {
  color: var(--bad);
}
.url-row {
  flex-wrap: nowrap;
}
.url-input {
  flex: 1 1 auto;
  width: auto;
  min-width: 0;
}
.upload {
  margin-top: 10px;
}
</style>
