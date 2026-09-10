/** Knowledge base tab state: the loaded-constituent list, the WordNet panel,
 *  and the upstream file catalog.
 *
 * Reactive state (not DOM ids) so `KbTab.vue` can bind to it directly;
 * `renderConstituents`/`renderWordNetPanel`/`loadSumoCatalog`/`renderPicker`
 * stay the entry points `boot.ts`, `kb.ts`, and `router.ts` call into --
 * everything else here is state those functions themselves populate/read, so
 * it stays alongside them. The three import channels (presets, GitHub
 * picker submission, URL/upload) and the WordNet on/off toggle are pure UI
 * triggers nothing outside `KbTab.vue` calls, so they live in the component
 * directly instead. */

import { ref } from "vue";
import { MERGE } from "../constants.ts";
import { state } from "../state.ts";
import { fetchSumoTree } from "../github-api.ts";
import { loadedSumoTestNames } from "./tests.ts";

// -- Constituent management ---------------------------------------------------

const ORIGIN_LABELS = { sumo: "GitHub", file: "Local File", url: "Remote URL" };

export const kbTotalsHtml = ref("");
export const loadedListItems = ref<{ name: string; sizeLabel: string; originLabel: string; isCore: boolean; origin: string }[]>([]);

export function renderConstituents() {
  kbTotalsHtml.value = `<b>${state.constituents.length}</b> constituent(s) loaded · ${state.diagnostics.length} diagnostic(s)`;
  loadedListItems.value = state.constituents.map((c) => ({
    name: c.name,
    sizeLabel: `${(c.text.length / 1000).toFixed(0)} KB`,
    originLabel: ORIGIN_LABELS[c.origin] || c.origin,
    isCore: c.name === MERGE,
    origin: c.origin,
  }));
}

// -- WordNet lexicon -----------------------------------------------------------
//
// Hardcoded source (the same SUMO repo/ref as the KIF constituents — see
// wordnet.ts); the only user-facing control is on/off (KbTab.vue's
// toggleWordnet). Enable/disable acts on the LIVE session immediately
// (clearWordNet / reinstallWordNetIfEnabled), separate from the persisted
// setting that decides what the NEXT boot does.

/** `bytes` as a human-readable size — KB for anything under 1 MB (matching
 *  the loaded-constituent list's units), MB above that: the mapping files
 *  run into the tens of megabytes, where an all-KB number is unreadable. */
function formatSize(bytes) {
  return bytes >= 1e6
    ? `${(bytes / 1e6).toFixed(1)} MB`
    : `${Math.round(bytes / 1000)} KB`;
}

export const wordnetEnabledRef = ref(state.wordnetEnabled);
export const wordnetPanelStatus = ref(""); // 'disabled' | 'loading' | ''
export const wordnetFileRows = ref<{ name: string; sizeLabel: string }[]>([]);
export const wordnetTotalLabel = ref("");

export function renderWordNetPanel() {
  wordnetEnabledRef.value = state.wordnetEnabled;
  if (!state.wordnetEnabled) {
    wordnetPanelStatus.value = "disabled";
    wordnetFileRows.value = [];
    return;
  }
  if (!state.wordnetFiles.length) {
    wordnetPanelStatus.value = "loading";
    wordnetFileRows.value = [];
    return;
  }
  wordnetPanelStatus.value = "";
  const total = state.wordnetFiles.reduce((sum, f) => sum + f.size, 0);
  wordnetFileRows.value = state.wordnetFiles.map((f) => ({ name: f.name, sizeLabel: formatSize(f.size) }));
  wordnetTotalLabel.value = formatSize(total);
}

// -- The upstream file catalog ------------------------------------------------

export const pickerStatus = ref("");
export const sumoPickerOptions = ref<string[]>([]);
export const fileFilter = ref("");

export async function loadSumoCatalog() {
  if (state.sumoCatalog) return;
  pickerStatus.value = "loading file list…";
  try {
    // Via the shared client so a rate-limited response raises rather than
    // silently yielding `undefined.tree`, and via the shared tree read so this
    // and the change tracker's staleness check cost one request between them.
    const tree = await fetchSumoTree();
    state.sumoCatalog = tree
      .filter((e) => e.type === "blob" && /\.kif(\.tq)?$/i.test(e.path))
      .map((e) => e.path)
      .sort();
    renderPicker();
  } catch (e) {
    pickerStatus.value = "could not load file list: " + (e.message || e);
  }
}

export function renderPicker() {
  const filter = fileFilter.value.toLowerCase();
  const loaded = new Set(
    state.constituents.filter((c) => c.origin === "sumo").map((c) => c.name),
  );
  for (const name of loadedSumoTestNames()) loaded.add(name);
  const avail = state.sumoCatalog.filter(
    (p) => !loaded.has(p) && p.toLowerCase().includes(filter),
  );
  sumoPickerOptions.value = avail;
  pickerStatus.value = `${avail.length} file(s) available`;
}

export function onFileFilterInput(value: string) {
  fileFilter.value = value;
  if (state.sumoCatalog) renderPicker();
}
