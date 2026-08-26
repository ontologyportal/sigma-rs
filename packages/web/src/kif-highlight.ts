/** KIF syntax highlighting for rendered (non-editable) formulas — man-page
 *  references, proof steps, audit contradictions. The Edit tab's live buffer is
 *  tokenized by Monaco instead (see editor/monaco.ts's KIF_MONARCH, which
 *  mirrors this). */

import { esc, escAttr } from './dom.ts';

const KIF_KEYWORDS = new Set(['and', 'or', 'not', 'forall', 'exists', 'equal']);

/** Prover-internal vocabulary that has no man page: Skolem constants
 *  (`sK1`, `esk2_0`, …) and scope-qualified variable interning keys
 *  (`Human__15551`) that can leak into prover-emitted KIF. Linking them
 *  would only offer dead ends. */
function isInternalSymbol(word) {
  return /^(sK|esk|epred)\d/.test(word) || /__\d+$/.test(word);
}

const KIF_TOKEN_RE = /(;[^\n]*)|("(?:[^"\\]|\\.)*")|([()])|([?@][A-Za-z0-9_-]+)|(-?\d+(?:\.\d+)?)|(<=>|=>|=)|([A-Za-z_][A-Za-z0-9_-]*)/g;

export function highlightKif(src: string, { focusSymbol, linkSymbols }: { focusSymbol?: string; linkSymbols?: boolean } = {}) {
  let out = '', last = 0, m, afterOpenParen = false;
  KIF_TOKEN_RE.lastIndex = 0;
  while ((m = KIF_TOKEN_RE.exec(src))) {
    out += esc(src.slice(last, m.index));
    const [, comment, str, paren, variable, num, op, word] = m;
    if (comment) { out += `<span class="tok-com">${esc(comment)}</span>`; afterOpenParen = false; }
    else if (str) { out += `<span class="tok-str">${esc(str)}</span>`; afterOpenParen = false; }
    else if (paren) { out += `<span class="tok-paren">${esc(paren)}</span>`; afterOpenParen = paren === '('; }
    else if (variable) { out += `<span class="tok-var">${esc(variable)}</span>`; afterOpenParen = false; }
    else if (num) { out += `<span class="tok-num">${esc(num)}</span>`; afterOpenParen = false; }
    else if (op) { out += `<span class="tok-kw">${esc(op)}</span>`; afterOpenParen = false; }
    else if (word) {
      const isKw = KIF_KEYWORDS.has(word);
      let tok;
      if (isKw) tok = `<span class="tok-kw">${esc(word)}</span>`;
      else if (afterOpenParen) tok = `<span class="tok-fn">${esc(word)}</span>`; // relation/function symbol
      else tok = esc(word);
      if (focusSymbol && word === focusSymbol) {
        tok = `<span class="sym-focus">${tok}</span>`;      // the viewed symbol: highlight, don't self-link
      } else if (linkSymbols && !isKw && !isInternalSymbol(word)) {
        tok = `<a class="sym-link" data-sym="${escAttr(word)}">${tok}</a>`;  // symbol → its man page
      }
      out += tok;
      afterOpenParen = false;
    }
    last = KIF_TOKEN_RE.lastIndex;
  }
  out += esc(src.slice(last));
  return out + '\n'; // trailing line so a source ending in \n doesn't collapse height vs. the textarea
}
