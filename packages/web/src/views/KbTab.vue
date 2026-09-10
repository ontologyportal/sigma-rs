<script setup>
import { ref, reactive } from 'vue';
import { SUMO_FILE_SETTING, WORDNET_ENABLED_KEY } from '../../../../constants.ts';
import { state } from '../../../../state.ts';
import { call } from '../../../../rpc.ts';
import { fetchText, fetchAllTexts } from '../../../../sources.ts';
import { ingestConstituent, removeConstituent, reprocess } from '../../../../kb.ts';
import { reinstallWordNetIfEnabled } from '../../../../boot.ts';
import { lspReset } from '../../../../editor/lsp-client.ts';
import { addTest, isTestFile } from '../../../../tabs/tests.ts';
import { navigate } from '../../../../router.ts';
import {
  kbTotalsHtml, loadedListItems, renderConstituents,
  wordnetEnabledRef, wordnetPanelStatus, wordnetFileRows, wordnetTotalLabel, renderWordNetPanel,
  pickerStatus, sumoPickerOptions, fileFilter, onFileFilterInput,
} from '../../../../tabs/kb-tab.ts';

// Nothing outside this component calls these -- `renderConstituents`/
// `renderWordNetPanel`/`loadSumoCatalog`/`renderPicker` (tabs/kb-tab.ts) are
// the only entry points `boot.ts`/`kb.ts`/`router.ts` need.

function removeLoadedConstituent(name, origin) {
  kbLog.text = '';
  kbLog.isError = false;
  removeConstituent(name, origin);
}

function openConstituentInEditor(name) {
  navigate('edit', { file: name });
}

async function toggleWordnet(enabled) {
  state.wordnetEnabled = enabled;
  wordnetEnabledRef.value = enabled;
  try {
    localStorage.setItem(WORDNET_ENABLED_KEY, String(enabled));
  } catch {
    /* private mode */
  }
  try {
    if (enabled) {
      renderWordNetPanel(); // shows "Loading…" immediately
      await reinstallWordNetIfEnabled();
    } else {
      await call('clearWordNet');
    }
  } finally {
    renderWordNetPanel();
  }
}

async function onWordnetChange(e) {
  e.target.disabled = true;
  try {
    await toggleWordnet(e.target.checked);
  } finally {
    e.target.disabled = false;
  }
}

function onLoadedListClick(e) {
  const rm = e.target.closest('.rm');
  if (rm) { removeLoadedConstituent(rm.dataset.name, rm.dataset.source); return; }
  const open = e.target.closest('.file-open');
  if (open) openConstituentInEditor(open.dataset.name);
}

// -- Standard constituent sets ------------------------------------------------
//
// File lists mirror the Sigma XML configuration. Order is preserved from it:
// ingest is order-independent here (everything is promoted together at the
// end), but keeping it makes the two lists diffable against the source.

const PRESETS = {
  minimal: {
    label: 'Minimal SUMO',
    files: [
      'Merge.kif',
      'Mid-level-ontology.kif',
      'english_format.kif',
      'domainEnglishFormat.kif',
    ],
  },
  full: {
    label: 'Full SUMO',
    files: [
      'english_format.kif',
      'domainEnglishFormat.kif',
      'ArabicCulture.kif',
      'Anatomy.kif',
      'arteries.kif',
      'Biography.kif',
      'Cars.kif',
      'Catalog.kif',
      'Communications.kif',
      'ComputerInput.kif',
      'ComputingBrands.kif',
      'CountriesAndRegions.kif',
      'Dining.kif',
      'Economy.kif',
      'emotion.kif',
      'engineering.kif',
      'Facebook.kif',
      'FinancialOntology.kif',
      'Food.kif',
      'Geography.kif',
      'Government.kif',
      'Hotel.kif',
      'Justice.kif',
      'Languages.kif',
      'Law.kif',
      'Media.kif',
      'Medicine.kif',
      'Merge.kif',
      'Mid-level-ontology.kif',
      'MilitaryDevices.kif',
      'Military.kif',
      'MilitaryPersons.kif',
      'MilitaryProcesses.kif',
      'Music.kif',
      'naics.kif',
      'People.kif',
      'pictureList.kif',
      'pictureList-ImageNet.kif',
      'QoSontology.kif',
      'Sports.kif',
      'TransnationalIssues.kif',
      'Transportation.kif',
      'TransportDetail.kif',
      'UXExperimentalTerms.kif',
      'VirusProteinAndCellPart.kif',
      'Weather.kif',
      'WMD.kif',
      'capabilities.kif',
    ],
  },
};

const presetsBusy = ref(false);
const presetNote = reactive({ text: '', isError: false });

async function loadPreset(key) {
  const preset = PRESETS[key];
  presetsBusy.value = true;
  try {
    // A preset describes a whole KB, so it replaces rather than merges.
    state.constituents = [];
    state.savedConstituents = [];
    localStorage.setItem(
      SUMO_FILE_SETTING,
      JSON.stringify(state.savedConstituents),
    );
    await call('newSession');
    lspReset(); // the worker dropped its WasmLsp with the session
    await reinstallWordNetIfEnabled(); // newSession() also dropped WordNet
    renderConstituents();

    const total = preset.files.length;
    presetNote.isError = false;
    presetNote.text = `Fetching ${preset.label} — 0/${total}…`;
    const texts = await fetchAllTexts(preset.files, 6, (n) => {
      presetNote.text = `Fetching ${preset.label} — ${n}/${total}…`;
    });

    const failed = [];
    for (let i = 0; i < preset.files.length; i++) {
      const name = preset.files[i],
        text = texts[i];
      if (text instanceof Error) {
        failed.push(`${name}: ${text.message}`);
        continue;
      }
      presetNote.text = `Reading ${name} (${i + 1}/${total})…`;
      try {
        await ingestConstituent(name, text, 'sumo');
      } catch (e) {
        failed.push(`${name}: ${e.message || e}`);
      }
    }
    renderConstituents();
    presetNote.text = `Axiomatizing ${state.constituents.length} constituent(s)…`;
    await reprocess();

    presetNote.isError = failed.length > 0;
    presetNote.text = failed.length
      ? `${preset.label}: loaded ${state.constituents.length}/${total}, ${failed.length} failed — ${failed[0]}`
      : `${preset.label} loaded — ${state.constituents.length} constituents.`;
  } catch (e) {
    presetNote.isError = true;
    presetNote.text = String((e && e.message) || e);
  } finally {
    presetsBusy.value = false;
  }
}

// -- Import channels ----------------------------------------------------------

const kbLog = reactive({ text: '', isError: false });
const addSumoBusy = ref(false);
const addUrlBusy = ref(false);
const kbUrl = ref('');

async function addSumoSelected(paths) {
  if (!paths.length) {
    kbLog.text = 'Select one or more files first.';
    kbLog.isError = false;
    return;
  }
  addSumoBusy.value = true;
  try {
    // Ingest (fetch + parse) under the busy button — no toast yet. Fetches run
    // batched, like the presets: a multi-select of a dozen files is otherwise a
    // dozen serial round-trips.
    let added = 0,
      notices = 0;
    const failed = [];
    kbLog.isError = false;
    const texts = await fetchAllTexts(paths, 6, (n) => {
      kbLog.text = `Fetching — ${n}/${paths.length}…`;
    });
    for (let i = 0; i < paths.length; i++) {
      const path = paths[i],
        text = texts[i];
      if (text instanceof Error) {
        failed.push(`${path}: ${text.message}`);
        continue;
      }
      try {
        const r = isTestFile(path)
          ? await addTest(path, text, 'sumo')
          : await ingestConstituent(path, text);
        if (r.added) added += 1;
        notices += r.notices.length;
      } catch (err) {
        failed.push(`${path}: ${err.message || err}`);
      }
    }
    renderConstituents();
    kbLog.text = `Ingested ${added}/${paths.length} constituent(s); axiomatizing…`;
    await reprocess(); // toast → promote → validate → untoast
    if (failed.length) {
      kbLog.isError = true;
      kbLog.text = `Added ${added}/${paths.length}; ${failed.length} failed — ${failed[0]}`;
    } else {
      kbLog.isError = false;
      kbLog.text =
        `Added ${added}/${paths.length} constituent(s)` +
        (notices ? ` (${notices} load notice(s))` : '') +
        '.';
    }
  } finally {
    addSumoBusy.value = false;
  }
}

async function addUrl() {
  const url = kbUrl.value.trim();
  if (!url) {
    kbLog.text = 'Enter a URL first.';
    kbLog.isError = false;
    return;
  }
  addUrlBusy.value = true;
  try {
    const text = await fetchText(url);
    kbLog.isError = false;
    if (isTestFile(url)) {
      const r = await addTest(url, text, 'url');
      kbLog.text = r.added ? `Imported test ${url}.` : r.notices.join(' | ');
      return;
    }
    const r = await ingestConstituent(url, text, 'url');
    renderConstituents();
    kbLog.text = r.added ? `Ingested ${url}; axiomatizing…` : r.notices.join(' | ');
    if (r.added) await reprocess();
  } finally {
    addUrlBusy.value = false;
  }
}

async function uploadKbFile(file) {
  addUrlBusy.value = true; // shares the "Add URL" busy indicator, as before
  try {
    const text = await file.text();
    if (state.opfsRoot === null) throw new Error('File system not yet initialized');
    const handle = await state.opfsRoot.getFileHandle(file.name, { create: true });
    const stream = await handle.createWritable();
    await stream.write(text);
    await stream.close();
    kbLog.isError = false;
    // Test files (.kif.tq / .p / .tptp) import through the Ask/Tell tab's
    // "Load test" instead -- this uploader is KIF-constituent-only.
    const r = await ingestConstituent(file.name, text, 'file');
    renderConstituents();
    kbLog.text = r.added ? `Ingested ${file.name}; axiomatizing…` : r.notices.join(' | ');
    if (r.added) await reprocess();
  } finally {
    addUrlBusy.value = false;
  }
}

const sumoPickerEl = ref(null);
function submitAddSumo() {
  const paths = [...sumoPickerEl.value.selectedOptions].map((o) => o.value);
  addSumoSelected(paths);
}

function onKbFileChange(e) {
  const file = e.target.files[0];
  e.target.value = '';
  if (file) uploadKbFile(file);
}
</script>

<template>
<div class="card">
  <div
    class="inline"
    style="justify-content: space-between; gap: 10px; flex-wrap: wrap"
  >
    <div>
      <div style="font-weight: 600">Load a standard set</div>
      <div class="hint">Replaces everything currently loaded.</div>
    </div>
    <div class="inline" style="gap: 8px">
      <button class="btn" id="loadMinimal" type="button" :disabled="presetsBusy" @click="loadPreset('minimal')">
        Minimal SUMO
      </button>
      <button class="btn" id="loadFull" type="button" :disabled="presetsBusy" @click="loadPreset('full')">Full SUMO</button>
    </div>
  </div>
  <div id="presetNote" class="hint" :style="{ marginTop: '8px', color: presetNote.isError ? 'var(--bad)' : '' }">{{ presetNote.text }}</div>
</div>

<div class="card">
  <div class="inline" style="justify-content: space-between">
    <div id="kbTotals" class="hint" v-html="kbTotalsHtml"></div>
  </div>
  <ul id="loadedList" class="results" style="margin-top: 8px" @click="onLoadedListClick">
    <li v-for="c in loadedListItems" :key="c.origin + ':' + c.name" class="loaded-row">
      <span><a class="file-open" :data-name="c.name" title="Open in the editor">{{ c.name }}</a>
        <span class="hint">{{ c.sizeLabel }} · {{ c.originLabel }}</span></span>
      <span v-if="c.isCore" class="hint">core</span>
      <a v-else class="rm" :data-name="c.name" :data-source="c.origin">remove</a>
    </li>
  </ul>
</div>

<div class="card">
  <div style="font-weight: 600">WordNet lexicon</div>
  <div class="hint">
    Powers synonym-aware search (results tagged "wn") — fetched from the
    same <code>ontologyportal/sumo</code> repo as the KIF constituents
    above.
  </div>
  <label class="check" style="margin-top: 8px"
    ><input type="checkbox" id="wordnetEnabled" :checked="wordnetEnabledRef" @change="onWordnetChange" /> Enable
    WordNet search</label
  >
  <ul id="wordnetFileList" class="results" style="margin-top: 8px">
    <li v-if="wordnetPanelStatus === 'disabled'" class="hint">Disabled — search runs without WordNet synonym expansion.</li>
    <li v-else-if="wordnetPanelStatus === 'loading'" class="hint">Loading…</li>
    <template v-else>
      <li v-for="f in wordnetFileRows" :key="f.name" class="loaded-row">
        <span class="mono">{{ f.name }}</span>
        <span class="hint">{{ f.sizeLabel }}</span>
      </li>
      <li class="loaded-row"><span><b>Total</b></span><span class="hint">{{ wordnetTotalLabel }}</span></li>
    </template>
  </ul>
</div>

<div class="card">
  <label for="fileFilter"
    >Add SUMO constituents from <code>ontologyportal/sumo</code></label
  >
  <input
    type="search"
    id="fileFilter"
    placeholder="filter file list…"
    autocomplete="off"
    :value="fileFilter"
    @input="onFileFilterInput($event.target.value)"
  />
  <select
    id="sumoPicker"
    ref="sumoPickerEl"
    multiple
    size="10"
    style="width: 100%; margin-top: 8px"
  >
    <option v-for="p in sumoPickerOptions" :key="p" :value="p">{{ p }}</option>
  </select>
  <div
    class="inline"
    style="
      margin-top: 8px;
      justify-content: space-between;
      align-items: center;
    "
  >
    <span id="pickerStatus" class="hint">{{ pickerStatus }}</span>
    <button class="btn" id="addSumo" type="button" :disabled="addSumoBusy" @click="submitAddSumo">
      {{ addSumoBusy ? 'Working…' : 'Add selected' }}
    </button>
  </div>
</div>

<div class="card">
  <label>Add a constituent from elsewhere</label>
  <div class="inline" style="flex-wrap: nowrap">
    <input
      type="text"
      id="kbUrl"
      placeholder="https://example.org/ontology.kif"
      style="flex: 1 1 auto; width: auto; min-width: 0"
      v-model="kbUrl"
    />
    <button class="btn" id="addUrl" type="button" :disabled="addUrlBusy" @click="addUrl">
      {{ addUrlBusy ? 'Working…' : 'Add URL' }}
    </button>
  </div>
  <div style="margin-top: 10px">
    <label for="kbFile">Upload a .kif file</label
    ><input type="file" id="kbFile" accept=".kif,.txt,text/plain" @change="onKbFileChange" />
  </div>
</div>

<div id="kbLog" class="hint" :style="{ color: kbLog.isError ? 'var(--bad)' : '' }">{{ kbLog.text }}</div>
</template>
