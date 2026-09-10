<script setup lang="ts">
/** Knowledge base tab: the unified constituent table (with the standard-set
 *  presets in its header), the URL / local-upload channels, and the WordNet
 *  panel. */
import { computed, onActivated, onMounted, ref } from "vue";
import { GitOrigin, LocalOrigin, RemoteOrigin } from "../models/Origin";
import { fetchText, fetchAllTexts } from "../services/sources";
import { useKBStore } from "../stores/kb";
import { useBootStore } from "../stores/boot";
import { useTestsStore, isTestFile } from "../stores/tests";
import { useStatus } from "../composables/useStatus";
import { formatSize } from "../utils/format";
import BusyButton from "../components/BusyButton.vue";
import Card from "../components/Card.vue";
import DropMenu from "../components/DropMenu.vue";
import StatusLine from "../components/StatusLine.vue";
import ConstituentTable from "../components/kb/ConstituentTable.vue";
import WordNetPanel from "../components/kb/WordNetPanel.vue";

const kb = useKBStore();
const boot = useBootStore();
const tests = useTestsStore();

onMounted(() => kb.loadSumoCatalog());
onActivated(() => kb.loadSumoCatalog());

const kbLog = useStatus();
const tableLog = useStatus();

const summary = computed(() => {
  const loaded = kb.constituents.length;
  const loadedNames = new Set(kb.sumoNames);
  const available = (kb.sumoCatalog ?? []).filter(
    (e) => !loadedNames.has(e.path) && !/\.tq$/i.test(e.path),
  ).length;
  const bytes = kb.constituents.reduce((sum, c) => sum + c.text.length, 0);
  return `${loaded} loaded · ${available} available upstream · ${formatSize(bytes)} loaded`;
});

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
const presetBtn = ref<HTMLElement | null>(null);
const presetMenuOpen = ref(false);

async function loadPreset(key: string) {
  presetMenuOpen.value = false;
  const preset = PRESETS[key];
  presetsBusy.value = true;
  try {
    // A preset describes a whole KB, so it replaces rather than merges.
    await kb.replaceAll();

    const total = preset.files.length;
    tableLog.set(`Fetching ${preset.label} — 0/${total}…`);
    const texts = await fetchAllTexts(preset.files, 6, (n) => {
      tableLog.set(`Fetching ${preset.label} — ${n}/${total}…`);
    });

    const failed: string[] = [];
    const add: { name: string; text: string; origin: GitOrigin }[] = [];
    preset.files.forEach((name, i) => {
      const text = texts[i];
      if (text instanceof Error) failed.push(`${name}: ${text.message}`);
      else add.push({ name, text, origin: GitOrigin.default() });
    });
    tableLog.set(`Axiomatizing ${add.length} constituent(s)…`);
    const r = await kb.applyChanges({ add });
    failed.push(...r.failed);

    tableLog.set(
      failed.length
        ? `${preset.label}: loaded ${kb.constituents.length}/${total}, ${failed.length} failed — ${failed[0]}`
        : `${preset.label} loaded — ${kb.constituents.length} constituents.`,
      failed.length > 0,
    );
  } catch (e) {
    tableLog.fail(e);
  } finally {
    presetsBusy.value = false;
  }
}

// -- Import channels ----------------------------------------------------------

const addUrlBusy = ref(false);
const kbUrl = ref("");

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
  } catch (e) {
    kbLog.fail(e);
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
  } catch (e) {
    kbLog.fail(e);
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
  <Card title="Constituents" :description="summary">
    <template #header>
      <button
        ref="presetBtn"
        class="btn ghost"
        type="button"
        :disabled="presetsBusy"
        :aria-expanded="presetMenuOpen"
        aria-haspopup="menu"
        @click="presetMenuOpen = !presetMenuOpen"
      >
        {{ presetsBusy ? "Loading…" : "Load a standard set ▾" }}
      </button>
    </template>
    <DropMenu v-model="presetMenuOpen" :anchor="presetBtn">
      <button
        v-for="(preset, key) in PRESETS"
        :key="key"
        type="button"
        role="menuitem"
        @click="loadPreset(key)"
      >
        {{ preset.label }}
      </button>
    </DropMenu>
    <ConstituentTable @log="(text, error) => tableLog.set(text, error)" />
    <StatusLine class="mt-sm" :text="tableLog.text" :error="tableLog.error" />
  </Card>

  <Card title="Add from elsewhere">
    <div class="inline url-row">
      <input
        v-model="kbUrl"
        type="text"
        class="url-input"
        placeholder="https://example.org/ontology.kif"
        aria-label="Constituent URL"
        @keydown.enter="addUrl"
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
    <StatusLine class="mt-sm" :text="kbLog.text" :error="kbLog.error" />
  </Card>

  <Card title="WordNet lexicon">
    <template #description>
      Powers synonym-aware search (results tagged "wn") — fetched from the same
      <code>ontologyportal/sumo</code> repo as the KIF constituents above.
    </template>
    <WordNetPanel />
  </Card>
</template>

<style scoped>
.url-row {
  flex-wrap: nowrap;
}
.url-input {
  flex: 1 1 auto;
  width: auto;
  min-width: 0;
}
</style>
