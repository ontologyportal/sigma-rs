/**
 * Tests: `.kif.tq` (SUO-KIF harness directives) and `.p`/`.tptp` (standalone
 * TPTP problems, parsed via `parseTptpTest`).
 *
 * Tests share the constituent import channels (GitHub picker / URL) but are a
 * separate collection: a test's (query …) must never be ingested as an axiom.
 * Running one reuses the prove pipeline with the test's own axioms, query,
 * and (time N) budget. The Ask/Tell tab's "Load test" / "Open test" / "Save
 * test" own the upload/browse/save-back workflow (see `prover.ts`); this
 * module owns the imported-tests collection and its OPFS/localStorage
 * persistence.
 */

import { formatTest } from 'sigmakee/sdk';
import { TQ_SETTING } from '../constants.ts';
import { state } from '../state.ts';
import { call } from '../rpc.ts';
import { $, esc, withBusy } from '../dom.ts';
import { fromOrigin } from '../sources.ts';
import { proverConfig } from '../prover-config.ts';
import { ensureProverEditors, setProverPanes, paneValue } from './prover.ts';

let savedTests = JSON.parse(localStorage.getItem(TQ_SETTING) || '[]');   // [{ name, origin }]
let tqTests = [];   // [{ name, origin, text, parsed, outcome }]

/** The test currently loaded into the Ask/Tell panes, if any -- `{ name,
 *  origin }`. Set by opening, loading, or saving-as a test; cleared if that
 *  test is removed. */
let openTest = null;

export const isTestFile = (name) => /\.(tq|p|tptp)$/i.test(name);

/** Names of the imported tests that came from the upstream repo — the KB tab's
 *  picker hides them alongside the loaded constituents. */
export function loadedSumoTestNames() {
  return tqTests.filter((t) => t.origin === 'sumo').map((t) => t.name);
}

/** The RPC that understands `name`'s dialect: `.kif.tq` (SUO-KIF harness
 *  directives) vs `.p`/`.tptp` (a standalone TPTP problem -- see
 *  `parse_tptp_test_content` in core). Both return the same `TestCaseView`
 *  shape (KIF text either way; a TPTP test's theory/conjecture come back
 *  translated). */
function testParseRpc(name) {
  return /\.tq$/i.test(name) ? 'parseTest' : 'parseTptpTest';
}

export async function addTest(name, text, origin) {
  if (tqTests.some((t) => t.name === name)) return { added: false, notices: [`${name}: already imported`] };
  const { test } = await call(testParseRpc(name), { name, text });
  tqTests.push({ name, origin, text, parsed: test, outcome: null });
  if (!savedTests.some((t) => t.name === name && t.origin === origin)) {
    savedTests.push({ name, origin });
    localStorage.setItem(TQ_SETTING, JSON.stringify(savedTests));
  }
  renderTests();
  return { added: true, notices: [] };
}

async function removeTest(name, origin) {
  tqTests = tqTests.filter((t) => t.name !== name || t.origin !== origin);
  savedTests = savedTests.filter((t) => t.name !== name || t.origin !== origin);
  localStorage.setItem(TQ_SETTING, JSON.stringify(savedTests));
  if (origin === 'file') {
    try { const h = await state.opfsRoot.getFileHandle(name); await h.remove(); } catch { /* already gone */ }
  }
  if (openTest && openTest.name === name && openTest.origin === origin) {
    openTest = null;
    renderOpenTestHint();
  }
  renderTests();
}

/** Write `text` to `name` in OPFS, creating or overwriting it -- shared by
 *  test upload and save-back. */
async function writeOpfsFile(name, text) {
  if (state.opfsRoot === null) throw new Error('File system not yet initialized');
  const handle = await state.opfsRoot.getFileHandle(name, { create: true });
  const stream = await handle.createWritable();
  await stream.write(text);
  await stream.close();
}

/** The test currently open in the Ask/Tell panes, or `null`. */
export function getOpenTest() {
  return openTest;
}

/** Refresh the Ask/Tell tab's "editing: <name>" hint under the button row. */
export function renderOpenTestHint() {
  const el = $('openTestHint');
  if (el) el.textContent = openTest ? `Editing test: ${openTest.name}` : '';
}

/** Load `assertions`/`query` from a `TestCaseView`'s parsed fields into the
 *  Ask/Tell panes -- the "open" action's core, shared by clicking an
 *  imported test and finishing a fresh upload. Every test's panes carry KIF
 *  text regardless of source dialect (`testParseRpc`'s `.p`/`.tptp` path
 *  translates); if the proof-language toggle is on TPTP's single-pane mode,
 *  switch it back to KIF, or that text would be misread as one whole TPTP
 *  problem with its query pane hidden. */
async function openParsedInPanes(parsed) {
  await ensureProverEditors().catch(() => {});
  setProverPanes(parsed.axiomKif || '', parsed.queryKif || '');
  const lang = $('cfgProofLang') as HTMLSelectElement | null;
  if (lang && lang.value === 'tptp') {
    lang.value = 'kif';
    lang.dispatchEvent(new Event('change'));
  }
}

/** Import a freshly uploaded test file (Ask/Tell's "Load test"): persist it
 *  to OPFS, parse + add it to the imported-tests list, open it in the panes,
 *  and mark it as the currently open test. Throws on a non-test extension or
 *  a parse failure (the caller shows the message; nothing is left half-added
 *  on failure since `addTest` only pushes after a successful parse). */
export async function loadTestFile(name, text) {
  if (!isTestFile(name)) throw new Error(`${name}: not a .kif.tq / .p / .tptp test file`);
  await writeOpfsFile(name, text);
  const r = await addTest(name, text, 'file');
  if (r.added) {
    const t = tqTests.find((x) => x.name === name && x.origin === 'file');
    await openParsedInPanes(t.parsed);
    openTest = { name, origin: 'file' };
    renderOpenTestHint();
  }
  return r;
}

/** Save the Ask/Tell assertions/query as a test. `formattedTq` is the
 *  `.kif.tq` text the caller built (via `formatTest`) from the current
 *  panes.  Overwrites the currently open test in place when it's a `.tq`
 *  file-origin import (round-trips cleanly, same format); otherwise --
 *  nothing open, or the open test is a `.p`/`.tptp` import or came from a
 *  read-only origin (`sumo`/`url`) that can't be written back -- prompts for
 *  a new file name and saves as a new local test.  Returns `{ saved, name,
 *  overwritten }`; `saved: false` means the user cancelled the name prompt. */
export async function saveCurrentTest(formattedTq) {
  const canOverwrite = !!openTest && openTest.origin === 'file' && /\.tq$/i.test(openTest.name);
  let name = openTest ? openTest.name : 'test.kif.tq';
  if (!canOverwrite) {
    name = /\.tq$/i.test(name) ? name : name.replace(/\.(p|tptp)$/i, '') + '.kif.tq';
    const chosen = (window.prompt('Save test as:', name) || '').trim();
    if (!chosen) return { saved: false };
    name = /\.tq$/i.test(chosen) ? chosen : `${chosen}.kif.tq`;
  }
  await writeOpfsFile(name, formattedTq);
  const existing = tqTests.find((t) => t.name === name && t.origin === 'file');
  if (existing) {
    existing.text = formattedTq;
    const { test } = await call('parseTest', { name, text: formattedTq });
    existing.parsed = test;
    existing.outcome = null;
    renderTests();
    openTest = { name, origin: 'file' };
    renderOpenTestHint();
    return { saved: true, name, overwritten: true };
  }
  const r = await addTest(name, formattedTq, 'file');
  if (r.added) {
    openTest = { name, origin: 'file' };
    renderOpenTestHint();
  }
  return { saved: r.added, name, overwritten: false, notices: r.notices };
}

export async function restoreTests() {
  for (const { name, origin } of savedTests) {
    try { await addTest(name, await fromOrigin(origin, name), origin); }
    catch (e) { console.warn(`test ${name}: ${e.message || e}`); }
  }
}

function gradeTest(parsed, result) {
  const exp = parsed.expectedProof;
  const conclusiveNo = ['Disproved', 'CounterSatisfiable', 'Consistent'].includes(result.status);
  if (exp === true) {
    return result.proved ? { cls: 'ok', label: 'pass' } : { cls: 'bad', label: `no proof (${result.status})` };
  }
  if (exp === false) {
    if (result.proved) return { cls: 'bad', label: 'proved — expected no' };
    return conclusiveNo ? { cls: 'ok', label: 'pass' } : { cls: 'mut', label: result.status };
  }
  return { cls: 'mut', label: result.status };   // no yes/no expectation: informational
}

async function runTest(t) {
  const cfg = proverConfig(t.parsed.timeout ? { timeLimitSecs: t.parsed.timeout } : {});
  const { result } = await call('prove', {
    assertions: t.parsed.axiomKif,
    query: t.parsed.queryKif,
    config: cfg,
    session: '__tq_test__',
  });
  t.outcome = { ...gradeTest(t.parsed, result), status: result.status };
}

export function renderTests() {
  const list = $('testsList');
  if (!list) return;
  $('testsEmpty').hidden = tqTests.length > 0;
  $('runAllTests').hidden = tqTests.length === 0;
  list.innerHTML = tqTests.map((t, i) => {
    const p = t.parsed;
    const missing = (p.extraFiles || []).filter((f) => !state.constituents.some((c) => c.name.endsWith(f)));
    const o = t.outcome;
    return `
    <li class="loaded-row">
      <span>
        <span class="sym">${esc(t.name)}</span>
        ${p.note ? `<span class="hint">${esc(p.note)}</span>` : ''}
        ${p.queryKif ? `<code class="hint">${esc(p.queryKif.length > 60 ? p.queryKif.slice(0, 60) + '…' : p.queryKif)}</code>` : '<span class="hint">no (query)</span>'}
        ${p.expectedProof != null ? `<span class="hint">expects ${p.expectedProof ? 'yes' : 'no'}</span>` : ''}
        ${p.expectedAnswer ? `<span class="hint">answer: ${esc(p.expectedAnswer.join(' '))}</span>` : ''}
        ${missing.length ? `<span class="hint" style="color:var(--warn)">needs ${esc(missing.join(', '))}</span>` : ''}
        ${o ? `<span class="tq-badge tq-${o.cls}">${esc(o.label)}</span>` : ''}
      </span>
      <span>
        ${p.queryKif ? `<a class="tq-run" data-i="${i}">run</a> · ` : ''}
        <a class="tq-open" data-i="${i}">open</a> ·
        <a class="rm tq-rm" data-i="${i}">remove</a>
      </span>
    </li>`;
  }).join('');
}

$('testsList')?.addEventListener('click', async (e) => {
  const a = e.target.closest('a[data-i]');
  if (!a) return;
  const t = tqTests[Number(a.dataset.i)];
  if (!t) return;
  if (a.classList.contains('tq-rm')) return removeTest(t.name, t.origin);
  if (a.classList.contains('tq-open')) {
    await openParsedInPanes(t.parsed);
    openTest = { name: t.name, origin: t.origin };
    renderOpenTestHint();
    $('testsPanel').hidden = true;
    $('openTestBtn')?.setAttribute('aria-expanded', 'false');
    return;
  }
  if (a.classList.contains('tq-run')) {
    a.textContent = 'running…';
    try { await runTest(t); } catch (err) { t.outcome = { cls: 'bad', label: String(err.message || err).slice(0, 60) }; }
    renderTests();
  }
});

$('runAllTests')?.addEventListener('click', (e) => withBusy(e.target, async () => {
  let pass = 0, ran = 0;
  for (const t of tqTests) {
    if (!t.parsed.queryKif) continue;
    $('testsLog').textContent = `Running ${t.name}…`;
    try { await runTest(t); } catch (err) { t.outcome = { cls: 'bad', label: String(err.message || err).slice(0, 60) }; }
    ran += 1;
    if (t.outcome.cls === 'ok') pass += 1;
    renderTests();
  }
  $('testsLog').textContent = `${pass}/${ran} passed.`;
}));

// -- Ask/Tell: Open test / Load test / Save test ------------------------------

$('openTestBtn')?.addEventListener('click', () => {
  const panel = $('testsPanel');
  const open = panel.hidden;
  panel.hidden = !open;
  $('openTestBtn').setAttribute('aria-expanded', String(open));
});

$('loadTestBtn')?.addEventListener('click', () => $('loadTestFile').click());

$('loadTestFile')?.addEventListener('change', (e) =>
  withBusy($('loadTestBtn'), async () => {
    const input = e.target as HTMLInputElement;
    const file = input.files?.[0];
    input.value = ''; // allow re-picking the same file name later
    if (!file) return;
    try {
      const text = await file.text();
      const r = await loadTestFile(file.name, text);
      $('testsLog').textContent = r.added
        ? `Imported and opened ${file.name}.`
        : r.notices.join(' | ');
    } catch (e) {
      $('testsLog').textContent = String(e && e.message || e);
    }
  }));

$('saveTestBtn')?.addEventListener('click', () => withBusy($('saveTestBtn'), async () => {
  const query = paneValue('query').trim();
  if (!query) { $('proverCfgSummary').textContent = 'Enter a query first.'; return; }
  const formatted = formatTest({
    timeout: proverConfig().timeLimitSecs,
    assertions: paneValue('assertions'),
    query,
    expectedProof: true,
  });
  try {
    const r = await saveCurrentTest(formatted);
    if (!r.saved) return; // cancelled the save-as prompt
    $('testsLog').textContent = r.overwritten
      ? `Saved changes to ${r.name}.`
      : (r.notices && r.notices.length ? r.notices.join(' | ') : `Saved as ${r.name}.`);
  } catch (e) {
    $('testsLog').textContent = String(e && e.message || e);
  }
}));

renderOpenTestHint();
