<script setup lang="ts">
/** Knowledge base tab: the unified constituent table (with the standard-set
 *  presets and the Import dialog in its header) and the WordNet panel. */
import { computed, onActivated, onMounted, ref } from "vue";
import { GitOrigin } from "../models/Origin";
import { fetchAllTexts } from "../services/sources";
import { useKBStore } from "../stores/kb";
import { useLibraryStore } from "../stores/library";
import { useStatus } from "../composables/useStatus";
import { formatSize } from "../utils/format";
import Card from "../components/Card.vue";
import DropMenu from "../components/DropMenu.vue";
import StatusLine from "../components/StatusLine.vue";
import ConstituentTable from "../components/kb/ConstituentTable.vue";
import ImportDialog from "../components/kb/ImportDialog.vue";
import WordNetPanel from "../components/kb/WordNetPanel.vue";

const kb = useKBStore();
const library = useLibraryStore();

onMounted(() => library.loadCatalogs());
onActivated(() => library.loadCatalogs());

const tableLog = useStatus();

const summary = computed(() => {
  const loaded = kb.constituents.length;
  const bytes = kb.constituents.reduce((sum, c) => sum + c.text.length, 0);
  return `${loaded} loaded · ${library.size} in library · ${formatSize(bytes)} loaded`;
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
const importOpen = ref(false);

async function loadPreset(key: string) {
  presetMenuOpen.value = false;
  const preset = PRESETS[key];
  presetsBusy.value = true;
  try {
    // A preset describes a whole KB, so it replaces rather than merges.
    await kb.replaceAll();

    const total = preset.files.length;
    const origin = GitOrigin.default();
    tableLog.set(`Fetching ${preset.label} — 0/${total}…`);
    const texts = await fetchAllTexts(
      preset.files.map((name) => ({ name, origin })),
      6,
      (n) => {
        tableLog.set(`Fetching ${preset.label} — ${n}/${total}…`);
      },
    );

    const failed: string[] = [];
    const add: { name: string; text: string; origin: GitOrigin }[] = [];
    preset.files.forEach((name, i) => {
      const text = texts[i];
      if (text instanceof Error) failed.push(`${name}: ${text.message}`);
      else add.push({ name, text, origin });
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
</script>

<template>
  <Card title="Constituents" :description="summary">
    <template #header>
      <span class="inline tight">
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
        <button class="btn ghost" type="button" @click="importOpen = true">
          Import…
        </button>
      </span>
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

  <ImportDialog v-model="importOpen" @imported="(msg) => tableLog.set(msg)" />

  <Card title="WordNet lexicon">
    <template #description>
      Powers synonym-aware search (results tagged "wn") — fetched from the same
      <code>ontologyportal/sumo</code> repo as the KIF constituents above.
    </template>
    <WordNetPanel />
  </Card>
</template>
