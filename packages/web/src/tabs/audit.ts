/** Audit tab: a whole-KB consistency check, rendered as the same proof
 *  transcript Ask/Tell uses — one card per distinct contradiction. */

import { call } from '../rpc.ts';
import { $, esc } from '../dom.ts';
import { proverConfig, vampireSelected } from '../prover-config.ts';
import { wireProofGraph } from '../proof-graph.ts';
import { renderProofSteps, proseDetails } from '../proof-view.ts';

$('runAudit').onclick = async () => {
  const btn = $('runAudit');
  const vampire = vampireSelected();
  btn.disabled = true; btn.textContent = 'Auditing…';
  try {
    // Audit inherits the Ask/Tell prover settings (including backend, via
    // the shared #proverSettings panel both tabs toggle), but keeps its own
    // time limit.
    const { result } = vampire
      ? await call('auditVampire', {
          timeLimitSecs: $('auditTime').value,
          extraArgs: $('cfgVampireArgs').value.trim(),
        })
      : await call('audit', {
          config: proverConfig({ timeLimitSecs: $('auditTime').value }),
          limit: Math.max(1, Number($('auditLimit').value) || 5),
        });
    renderAudit(result, vampire ? 'Vampire' : 'SUPr');
  } catch (e) {
    $('auditResult').innerHTML = `<div class="card hint" style="color:var(--bad)">${esc(String(e && e.message || e))}</div>`;
  } finally {
    btn.disabled = false; btn.textContent = 'Run audit';
  }
};

function renderAudit(r, backendLabel) {
  const badge = `<span class="audit-status ${esc(r.status)}">${esc(r.status)}</span>`;
  const backend = backendLabel ? `<span class="hint">via ${esc(backendLabel)}</span>` : '';
  const steps = r.given_steps != null ? `<span class="hint">${r.given_steps} given-clause steps</span>` : '';

  let verdict;
  if (r.status === 'Consistent') {
    verdict = 'No contradiction found — the loaded KB saturated cleanly.';
  } else if (r.inconsistent && !r.contradictions.length) {
    // Vampire's one-shot run yields at most a single contradiction (no
    // enumerator, unlike the native audit's driver) — this covers the case
    // where even that single witness failed to parse; say so without
    // implying "zero".
    verdict = 'Contradiction found — see raw engine output for the derivation.';
  } else if (r.inconsistent) {
    verdict = `${r.contradictions.length} distinct contradiction${r.contradictions.length === 1 ? '' : 's'} found.`;
  } else {
    verdict = 'No contradiction found within budget — inconclusive (raise the time limit and try again).';
  }

  let html = `
    <div class="card">
      <div class="inline" style="gap:10px">${badge}${backend}${steps}</div>
      <div class="hint" style="margin-top:8px">${esc(verdict)}</div>
      <details style="margin-top:10px"><summary class="hint">raw engine output</summary><pre>${esc(r.raw_output || '(none)')}</pre></details>
    </div>`;

  html += r.contradictions.map((c, i) => `
    <div class="card">
      <div class="contradiction-hd">Contradiction #${i + 1} — ${c.steps.length} step${c.steps.length === 1 ? '' : 's'}</div>
      <ol class="refs">${renderProofSteps(c.steps)}</ol>
      ${proseDetails(c.prose, c.prose_missing)}
      <details class="proof-graph-details" style="margin-top:10px">
        <summary class="hint">proof graph</summary>
        <div class="graph-container"></div>
        <div class="hint graph-tip"></div>
        <details class="graph-dot-toggle"><summary>graphviz (DOT) source</summary><pre>${esc(c.graphviz || '(none)')}</pre></details>
      </details>
    </div>`).join('');

  $('auditResult').innerHTML = html;

  document.querySelectorAll('#auditResult .proof-graph-details').forEach((details, i) => {
    wireProofGraph(
      details,
      details.querySelector('.graph-container'),
      details.querySelector('.graph-tip'),
      () => r.contradictions[i].steps,
    );
  });
}
