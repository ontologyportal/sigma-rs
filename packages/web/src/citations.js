/** `file:line` source citations, shared by diagnostics, proof steps, audit
 *  contradictions, and man-page references. */

import { SUMO } from './constants.js';
import { state } from './state.js';
import { esc } from './dom.js';
import { navigate } from './router.js';

/**
 * GitHub *blame* deep-link for a SUMO-sourced constituent, else null.
 *
 * Blame rather than blob: it lands on the same line but with per-line author,
 * date and commit attribution — "who last changed this axiom" — for free. The
 * API route to that data is GraphQL `Blob.blame`, which requires a token even
 * for public repos and so is unusable from a static, unauthenticated page.
 */
export function ghLink(file, line) {
  const c = state.constituents.find((x) => x.name === file);
  if (!c || c.origin !== 'sumo') return null;
  return `https://github.com/${SUMO.owner}/${SUMO.repo}/blame/${SUMO.ref}/${file}#L${line}`;
}

/** The blame anchor for a citation, or '' when the source is not on GitHub. */
export function ghAnchor(file, line) {
  const url = ghLink(file, line);
  return url
    ? `<a class="hint gh" href="${url}" target="_blank" rel="noopener"
         title="Who last changed this line (GitHub blame)">blame ↗</a>`
    : '';
}

/** Open `file` in the Edit tab with the caret on `line`. Routed through the URL
 *  so the jump is a real history entry and the resulting view is shareable. */
export function openInEditor(file, line) {
  return navigate('edit', { file, l: line > 0 ? line : null });   // Back returns here
}

/**
 * A `file:line` citation. Rendered as a link that opens the editor there when
 * the file is a loaded constituent, and as plain text otherwise — a proof can
 * cite a synthetic/CNF source, or an axiom from a file the user has since
 * removed, and neither is openable. `extraClass` carries the caller's styling.
 */
export function locLink(file, line, extraClass = 'hint ref-loc') {
  if (!file) return '';
  const label = `${esc(file)}:${line}`;
  if (!state.constituents.some((c) => c.name === file)) return `<span class="${extraClass}">${label}</span>`;
  return `<a class="${extraClass} jump-src" data-file="${esc(file)}" data-line="${line}">${label}</a>`;
}

// One delegated handler for every file:line citation — diagnostics, proof
// steps, audit contradictions, and man-page references all route through here.
document.addEventListener('click', (e) => {
  if (e.target.closest('a.gh')) return;   // let the GitHub link open normally
  const a = e.target.closest('a.jump-src');
  if (!a) return;
  e.preventDefault();
  openInEditor(a.dataset.file, Number(a.dataset.line));
});
