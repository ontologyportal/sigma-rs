/** Knowledge base tab: the loaded-constituent list, the standard presets, and
 *  the three import channels (GitHub picker / URL / upload). */

import { MERGE, SUMO, SUMO_FILE_SETTING } from '../constants.js';
import { state } from '../state.js';
import { call } from '../rpc.js';
import { $, esc, escAttr, withBusy } from '../dom.js';
import { fetchText, fetchAllTexts } from '../sources.js';
import { githubApi } from '../github-api.js';
import { ingestConstituent, removeConstituent, reprocess } from '../kb.js';
import { addTest, isTestFile, loadedSumoTestNames } from './tests.js';
import { navigate } from '../router.js';

// -- Constituent management ---------------------------------------------------

export function renderConstituents() {
  const kb = $('kbTotals');
  if (kb) kb.innerHTML = `<b>${state.constituents.length}</b> constituent(s) loaded · ${state.diagnostics.length} diagnostic(s)`;
  const list = $('loadedList');
  if (!list) return;
  const ORIGIN_LABELS = { sumo: 'GitHub', file: 'Local File', url: 'Remote URL' };
  list.innerHTML = state.constituents.map((c) => `
    <li class="loaded-row">
      <span><a class="file-open" data-name="${escAttr(c.name)}" title="Open in the editor">${esc(c.name)}</a>
        <span class="hint">${(c.text.length / 1000).toFixed(0)} KB · ${ORIGIN_LABELS[c.origin] || esc(c.origin)}</span></span>
      ${c.name === MERGE ? '<span class="hint">core</span>' : `<a class="rm" data-name="${esc(c.name)}" data-source="${c.origin}">remove</a>`}
    </li>`).join('');
}

$('loadedList').addEventListener('click', (e) => {
  const rm = e.target.closest('.rm');
  if (rm) { $('kbLog').textContent = ''; removeConstituent(rm.dataset.name, rm.dataset.source); return; }
  const open = e.target.closest('.file-open');
  if (open) navigate('edit', { file: open.dataset.name });
});

// -- Standard constituent sets ------------------------------------------------
//
// File lists mirror the Sigma XML configuration. Order is preserved from it:
// ingest is order-independent here (everything is promoted together at the
// end), but keeping it makes the two lists diffable against the source.

const PRESETS = {
  minimal: {
    label: 'Minimal SUMO',
    files: ['Merge.kif', 'Mid-level-ontology.kif', 'english_format.kif', 'domainEnglishFormat.kif'],
  },
  full: {
    label: 'Full SUMO',
    files: [
      'english_format.kif', 'domainEnglishFormat.kif', 'ArabicCulture.kif', 'Anatomy.kif',
      'arteries.kif', 'Biography.kif', 'Cars.kif', 'Catalog.kif', 'Communications.kif',
      'ComputerInput.kif', 'ComputingBrands.kif', 'CountriesAndRegions.kif', 'Dining.kif',
      'Economy.kif', 'emotion.kif', 'engineering.kif', 'Facebook.kif', 'FinancialOntology.kif',
      'Food.kif', 'Geography.kif', 'Government.kif', 'Hotel.kif', 'Justice.kif', 'Languages.kif',
      'Law.kif', 'Media.kif', 'Medicine.kif', 'Merge.kif', 'Mid-level-ontology.kif',
      'MilitaryDevices.kif', 'Military.kif', 'MilitaryPersons.kif', 'MilitaryProcesses.kif',
      'Music.kif', 'development/Muscles.kif', 'naics.kif', 'People.kif', 'pictureList.kif',
      'pictureList-ImageNet.kif', 'QoSontology.kif', 'Sports.kif', 'TransnationalIssues.kif',
      'Transportation.kif', 'TransportDetail.kif', 'UXExperimentalTerms.kif',
      'VirusProteinAndCellPart.kif', 'Weather.kif', 'WMD.kif', 'capabilities.kif',
    ],
  },
};

async function loadPreset(key) {
  const preset = PRESETS[key];
  const buttons = [$('loadMinimal'), $('loadFull')];
  buttons.forEach((b) => { b.disabled = true; });
  const note = $('presetNote');
  try {
    // A preset describes a whole KB, so it replaces rather than merges.
    state.constituents = [];
    state.savedConstituents = [];
    localStorage.setItem(SUMO_FILE_SETTING, JSON.stringify(state.savedConstituents));
    await call('newSession');
    renderConstituents();

    const total = preset.files.length;
    note.style.color = '';
    note.textContent = `Fetching ${preset.label} — 0/${total}…`;
    const texts = await fetchAllTexts(preset.files, 6,
      (n) => { note.textContent = `Fetching ${preset.label} — ${n}/${total}…`; });

    const failed = [];
    for (let i = 0; i < preset.files.length; i++) {
      const name = preset.files[i], text = texts[i];
      if (text instanceof Error) { failed.push(`${name}: ${text.message}`); continue; }
      note.textContent = `Reading ${name} (${i + 1}/${total})…`;
      try { await ingestConstituent(name, text, 'sumo'); }
      catch (e) { failed.push(`${name}: ${e.message || e}`); }
    }
    renderConstituents();
    note.textContent = `Axiomatizing ${state.constituents.length} constituent(s)…`;
    await reprocess();

    note.style.color = failed.length ? 'var(--bad)' : '';
    note.textContent = failed.length
      ? `${preset.label}: loaded ${state.constituents.length}/${total}, ${failed.length} failed — ${failed[0]}`
      : `${preset.label} loaded — ${state.constituents.length} constituents.`;
  } catch (e) {
    note.style.color = 'var(--bad)';
    note.textContent = String(e && e.message || e);
  } finally {
    buttons.forEach((b) => { b.disabled = false; });
  }
}

$('loadMinimal').onclick = () => loadPreset('minimal');
$('loadFull').onclick = () => loadPreset('full');

// -- The upstream file catalog ------------------------------------------------

export async function loadSumoCatalog() {
  if (state.sumoCatalog) return;
  $('pickerStatus').textContent = 'loading file list…';
  try {
    // Via the shared client so a rate-limited response raises rather than
    // silently yielding `undefined.tree`.
    const tree = await githubApi(`/repos/${SUMO.owner}/${SUMO.repo}/git/trees/${SUMO.ref}?recursive=1`);
    state.sumoCatalog = (tree.tree || [])
      .filter((e) => e.type === 'blob' && /\.kif(\.tq)?$/i.test(e.path))
      .map((e) => e.path)
      .sort();
    renderPicker();
  } catch (e) {
    $('pickerStatus').textContent = 'could not load file list: ' + (e.message || e);
  }
}

export function renderPicker() {
  const filter = $('fileFilter').value.toLowerCase();
  const loaded = new Set(state.constituents.filter((c) => c.origin === 'sumo').map((c) => c.name));
  for (const name of loadedSumoTestNames()) loaded.add(name);
  const avail = state.sumoCatalog.filter((p) => !loaded.has(p) && p.toLowerCase().includes(filter));
  $('sumoPicker').innerHTML = avail.map((p) => `<option value="${esc(p)}">${esc(p)}</option>`).join('');
  $('pickerStatus').textContent = `${avail.length} file(s) available`;
}

$('fileFilter').addEventListener('input', () => { if (state.sumoCatalog) renderPicker(); });

// -- Import channels ----------------------------------------------------------

$('addSumo').onclick = (e) => withBusy(e.target, async () => {
  const paths = [...$('sumoPicker').selectedOptions].map((o) => o.value);
  if (!paths.length) { $('kbLog').textContent = 'Select one or more files first.'; return; }
  // Ingest (fetch + parse) under the busy button — no toast yet. Fetches run
  // batched, like the presets: a multi-select of a dozen files is otherwise a
  // dozen serial round-trips.
  let added = 0, notices = 0; const failed = [];
  $('kbLog').style.color = '';
  const texts = await fetchAllTexts(paths, 6,
    (n) => { $('kbLog').textContent = `Fetching — ${n}/${paths.length}…`; });
  for (let i = 0; i < paths.length; i++) {
    const path = paths[i], text = texts[i];
    if (text instanceof Error) { failed.push(`${path}: ${text.message}`); continue; }
    try {
      const r = isTestFile(path) ? await addTest(path, text, 'sumo') : await ingestConstituent(path, text);
      if (r.added) added += 1; notices += r.notices.length;
    }
    catch (err) { failed.push(`${path}: ${err.message || err}`); }
  }
  renderConstituents();
  $('kbLog').textContent = `Ingested ${added}/${paths.length} constituent(s); axiomatizing…`;
  await reprocess();   // toast → promote → validate → untoast
  if (failed.length) { $('kbLog').style.color = 'var(--bad)'; $('kbLog').textContent = `Added ${added}/${paths.length}; ${failed.length} failed — ${failed[0]}`; }
  else $('kbLog').textContent = `Added ${added}/${paths.length} constituent(s)` + (notices ? ` (${notices} load notice(s))` : '') + '.';
});

$('addUrl').onclick = (e) => withBusy(e.target, async () => {
  const url = $('kbUrl').value.trim();
  if (!url) { $('kbLog').textContent = 'Enter a URL first.'; return; }
  const text = await fetchText(url);
  $('kbLog').style.color = '';
  if (isTestFile(url)) {
    const r = await addTest(url, text, 'url');
    $('kbLog').textContent = r.added ? `Imported test ${url}.` : r.notices.join(' | ');
    return;
  }
  const r = await ingestConstituent(url, text, 'url');
  renderConstituents();
  $('kbLog').textContent = r.added ? `Ingested ${url}; axiomatizing…` : r.notices.join(' | ');
  if (r.added) await reprocess();
});

$('kbFile').onchange = (e) => withBusy($('addUrl'), async () => {
  const file = e.target.files[0];
  if (!file) return;
  const text = await file.text();
  if (state.opfsRoot === null) throw new Error('File system not yet initialized');
  const handle = await state.opfsRoot.getFileHandle(file.name, { create: true });
  const stream = await handle.createWritable();
  await stream.write(text);
  await stream.close();
  $('kbLog').style.color = '';
  if (isTestFile(file.name)) {
    const r = await addTest(file.name, text, 'file');
    $('kbLog').textContent = r.added ? `Imported test ${file.name}.` : r.notices.join(' | ');
    return;
  }
  const r = await ingestConstituent(file.name, text, 'file');
  renderConstituents();
  $('kbLog').textContent = r.added ? `Ingested ${file.name}; axiomatizing…` : r.notices.join(' | ');
  if (r.added) await reprocess();
});
