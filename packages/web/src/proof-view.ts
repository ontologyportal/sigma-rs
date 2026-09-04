/**
 * Shared proof rendering (Ask/Tell and Audit).
 *
 * Both a refutation proof and an audit contradiction are the same thing — a
 * `{index, rule, premises, kif, tptp, file, line}[]` transcript — so they
 * render through one code path: rule + derivation, highlighted formula,
 * source citation. The proof-language toggle (`prover-config.ts`'s
 * `proofLanguage`) only changes which of `kif`/`tptp` is the displayed
 * formula text — the surrounding block, citation, and paraphrase-on-click
 * (always keyed to the underlying KIF) stay the same either way.
 */

import { state } from './state.ts';
import { call } from './rpc.ts';
import { esc, escAttr } from './dom.ts';
import { highlightKif } from './kif-highlight.ts';
import { highlightTptp } from './tptp-highlight.ts';
import { locLink, ghAnchor } from './citations.ts';

/** "(from steps 1, 3)" — the premise back-references the graph draws as edges.
 *  Step indices are 0-based on the wire; the <ol> and the graph both label from
 *  1, so shift for display. */
function premiseRefs(s) {
  if (!s.premises || !s.premises.length) return '';
  const label = s.premises.length === 1 ? 'step' : 'steps';
  return ` <span class="hint">(from ${label} ${s.premises.map((p) => p + 1).join(', ')})</span>`;
}

/** One formula-citation row (an <li> for an `ol.refs` list), shared by the
 *  man-page reference list and the proof/contradiction step list: an
 *  optional `header` line, the highlighted formula (with `focusSymbol`
 *  subtly highlighted), and a `file:line` + blame footer shown only when a
 *  source location is known. Clicking the row expands a natural-language
 *  paraphrase, rendered lazily in the currently selected language --
 *  always derived from `kif`, regardless of which dialect is displayed.
 *  `lang: 'tptp'` displays `tptp` (falling back to `kif` when absent);
 *  every other/omitted value displays `kif`. */
interface KifCiteRowOpts {
  kif: string;
  tptp?: string;
  lang?: 'kif' | 'tptp';
  file?: string;
  line?: number;
  header?: string;
  focusSymbol?: string;
}

export function kifCiteRow({ kif, tptp, lang, file, line, header, focusSymbol }: KifCiteRowOpts) {
  const loc = locLink(file, line);
  const gh = ghAnchor(file, line);
  const meta = loc || gh ? `<div class="ref-meta">${loc}${gh}</div>` : '';
  const shown = lang === 'tptp' && tptp != null
    ? highlightTptp(tptp, { focusSymbol, linkSymbols: true }).replace(/\n$/, '')
    : highlightKif(kif, { focusSymbol, linkSymbols: true }).replace(/\n$/, '');
  return `<li>
    <details class="cite">
      <summary>
        ${header ? `<div class="hint">${header}</div>` : ''}
        <pre class="ref-kif">${shown}</pre>
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
 *  and the graph node labels, which both count from 1. `lang` selects the
 *  displayed formula dialect (see `kifCiteRow`). */
function proofStepRow(s, pos, lang) {
  const n = (s.index != null ? s.index : pos) + 1;
  const header = `<span class="step-num">${n}.</span> ${esc(s.rule)}${premiseRefs(s)}`;
  return kifCiteRow({ kif: s.kif, tptp: s.tptp, lang, file: s.file, line: s.line, header });
}

export function renderProofSteps(steps, lang: 'kif' | 'tptp' = 'kif') {
  return steps.map((s, i) => proofStepRow(s, i, lang)).join('');
}

/** A distinguished, non-step `<li>` for whole-proof TPTP material that
 *  doesn't belong to any single step — e.g. TFF's `$i`-monomorphic
 *  type-declaration preamble (see `AskResultView.proof_tptp_prologue` /
 *  `ContradictionView.proof_tptp_prologue` in the sdk). */
function tptpPrologueRow(prologue) {
  return `<li>
    <div class="hint">type declarations</div>
    <pre class="ref-kif">${highlightTptp(prologue, { linkSymbols: true }).replace(/\n$/, '')}</pre>
  </li>`;
}

/** A single `<li>` of unstyled plain text -- the whole proof, one formula per
 *  line (the whole-proof TPTP preamble first, when present), no headers,
 *  citations, paraphrase, syntax highlighting, or symbol links. Backs the
 *  settings panel's "plain proof" checkbox (`prover-config.ts`'s
 *  `plainProof`). */
function plainProofRow(steps, prologue, lang: 'kif' | 'tptp') {
  const lines = [];
  if (lang === 'tptp' && prologue) lines.push(prologue);
  for (const s of steps) lines.push(lang === 'tptp' && s.tptp != null ? s.tptp : s.kif);
  return `<li><pre class="ref-kif">${esc(lines.join('\n'))}</pre></li>`;
}

/** The proof/contradiction body for the `#pProof`/`.refs`-shaped `<ol>`'s
 *  inner HTML, in whichever language the shared proof-language select (see
 *  `prover-config.ts`'s `proofLanguage`) is set to. Every step renders
 *  through the same citation-row shape regardless of language -- source
 *  citation and paraphrase-on-click included -- only the displayed formula
 *  dialect changes; a whole-proof TPTP preamble, when present, renders once
 *  as a leading row ahead of the per-step rows. `plain` (the settings
 *  panel's "plain proof" checkbox) overrides all of that with a single
 *  unstyled text block -- see `plainProofRow`. */
export function renderProofBody(steps, prologue, lang: 'kif' | 'tptp', plain = false) {
  if (plain) return plainProofRow(steps, prologue, lang);
  const pre = lang === 'tptp' && prologue ? tptpPrologueRow(prologue) : '';
  return pre + renderProofSteps(steps, lang);
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
