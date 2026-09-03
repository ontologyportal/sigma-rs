/**
 * Shared proof rendering (Ask/Tell and Audit).
 *
 * Both a refutation proof and an audit contradiction are the same thing — a
 * `{index, rule, premises, kif, file, line}[]` transcript — so they render
 * through one code path: rule + derivation, highlighted KIF, source citation.
 */

import { state } from './state.ts';
import { call } from './rpc.ts';
import { esc, escAttr } from './dom.ts';
import { highlightKif } from './kif-highlight.ts';
import { locLink, ghAnchor } from './citations.ts';

/** "(from steps 1, 3)" — the premise back-references the graph draws as edges.
 *  Step indices are 0-based on the wire; the <ol> and the graph both label from
 *  1, so shift for display. */
function premiseRefs(s) {
  if (!s.premises || !s.premises.length) return '';
  const label = s.premises.length === 1 ? 'step' : 'steps';
  return ` <span class="hint">(from ${label} ${s.premises.map((p) => p + 1).join(', ')})</span>`;
}

/** One KIF-citation row (an <li> for an `ol.refs` list), shared by the man-page
 *  reference list and the proof/contradiction step list: an optional `header`
 *  line, the highlighted KIF (with `focusSymbol` subtly highlighted), and a
 *  `file:line` + blame footer shown only when a source location is known.
 *  Clicking the row expands a natural-language paraphrase of the formula,
 *  rendered lazily in the currently selected language. */
interface KifCiteRowOpts {
  kif: string;
  file?: string;
  line?: number;
  header?: string;
  focusSymbol?: string;
}

export function kifCiteRow({ kif, file, line, header, focusSymbol }: KifCiteRowOpts) {
  const loc = locLink(file, line);
  const gh = ghAnchor(file, line);
  const meta = loc || gh ? `<div class="ref-meta">${loc}${gh}</div>` : '';
  return `<li>
    <details class="cite">
      <summary>
        ${header ? `<div class="hint">${header}</div>` : ''}
        <pre class="ref-kif">${highlightKif(kif, { focusSymbol, linkSymbols: true }).replace(/\n$/, '')}</pre>
      </summary>
      <div class="nl" data-kif="${escAttr(kif)}"></div>
    </details>
    ${meta}
  </li>`;
}

// Lazily render a citation row's natural-language paraphrase when it is
// expanded, re-rendering if the language changed since the last time it opened.
// `toggle` doesn't bubble, so listen in the capture phase.
document.addEventListener('toggle', (e) => {
  const d = e.target;
  if (!(d instanceof HTMLDetailsElement) || !d.classList.contains('cite') || !d.open) return;
  const nl = d.querySelector<HTMLElement>('.nl');
  if (!nl || nl.dataset.lang === state.uiLanguage) return;
  nl.dataset.lang = state.uiLanguage;
  nl.textContent = 'rendering…';
  call('renderNl', { kif: nl.dataset.kif, language: state.uiLanguage, genericVars: state.genericVars })
    .then(({ text }) => { nl.textContent = text && text.trim() ? text : 'no paraphrase available'; })
    // A failed call is not a verdict — clear the language stamp so the next
    // expand retries instead of treating the failure as a cached answer.
    .catch(() => { nl.dataset.lang = ''; nl.textContent = 'paraphrase failed — reopen to retry'; });
}, true);

// The generic-vars settings toggle changes what an already-open paraphrase
// should say — clear every open row's language stamp so the toggle listener
// above re-renders it on the next open, and re-render any already open now.
document.getElementById('genericVarsToggle')?.addEventListener('change', (e) => {
  state.genericVars = (e.target as HTMLInputElement).checked;
  for (const d of document.querySelectorAll<HTMLDetailsElement>('details.cite[open]')) {
    const nl = d.querySelector<HTMLElement>('.nl');
    if (nl) nl.dataset.lang = '';
    d.dispatchEvent(new Event('toggle'));
  }
});

/** One proof/contradiction step as an <li>. `pos` is the 0-based fallback when a
 *  step carries no explicit `index` — the number shown must match `premiseRefs`
 *  and the graph node labels, which both count from 1. */
function proofStepRow(s, pos) {
  const n = (s.index != null ? s.index : pos) + 1;
  const header = `<span class="step-num">${n}.</span> ${esc(s.rule)}${premiseRefs(s)}`;
  return kifCiteRow({ kif: s.kif, file: s.file, line: s.line, header });
}

export function renderProofSteps(steps) {
  return steps.map((s, i) => proofStepRow(s, i)).join('');
}

/** The "shown by bare name" note under a prose block, or '' when nothing is missing. */
function proseMissingNote(missing) {
  return missing && missing.length
    ? `${missing.length} symbol(s) shown by bare name (no format/termFormat in EnglishLanguage): ${missing.join(', ')}`
    : '';
}

/** A collapsible plain-English rendering of a transcript. Both Ask/Tell and
 *  Audit render through this. */
export function proseDetails(prose, missing) {
  return `<details class="prose-details" style="margin-top:10px">
    <summary class="hint">proof in plain English</summary>
    <div class="prose">${esc(prose || '')}</div>
    ${missing && missing.length ? `<div class="hint" style="margin-top:6px">${esc(proseMissingNote(missing))}</div>` : ''}
  </details>`;
}
