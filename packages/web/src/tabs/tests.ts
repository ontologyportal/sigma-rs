/**
 * .kif.tq tests.
 *
 * Tests share the constituent import channels (GitHub picker / URL / upload)
 * but are a separate collection: a test's (query …) must never be ingested as
 * an axiom. Running one reuses the prove pipeline with the test's own axioms,
 * query, and (time N) budget.
 */

import { TQ_SETTING } from '../constants.ts';
import { state } from '../state.ts';
import { call } from '../rpc.ts';
import { $, esc, withBusy } from '../dom.ts';
import { fromOrigin } from '../sources.ts';
import { proverConfig } from '../prover-config.ts';
import { ensureProverEditors, setProverPanes } from './prover.ts';
import { showTab } from '../router.ts';

let savedTests = JSON.parse(localStorage.getItem(TQ_SETTING) || '[]');   // [{ name, origin }]
let tqTests = [];   // [{ name, origin, text, parsed, outcome }]

export const isTestFile = (name) => /\.tq$/i.test(name);

/** Names of the imported tests that came from the upstream repo — the KB tab's
 *  picker hides them alongside the loaded constituents. */
export function loadedSumoTestNames() {
  return tqTests.filter((t) => t.origin === 'sumo').map((t) => t.name);
}

export async function addTest(name, text, origin) {
  if (tqTests.some((t) => t.name === name)) return { added: false, notices: [`${name}: already imported`] };
  const { test } = await call('parseTest', { name, text });
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
  renderTests();
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
    await ensureProverEditors().catch(() => {});
    setProverPanes(t.parsed.axiomKif || '', t.parsed.queryKif || '');
    showTab('prover');
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
