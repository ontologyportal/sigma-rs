/** TPTP syntax highlighting for rendered (non-editable) formulas -- the TPTP
 *  rendering of proof steps when the proof-language toggle (see
 *  `stores/prover.ts`'s `proofLang`) is set to `"tptp"`. Mirrors
 *  `highlight-kif.ts`'s span-class vocabulary and `linkSymbols`/`focusSymbol`
 *  behavior so a proof step looks the same either way, just in a different
 *  formula dialect. */

import { esc, escAttr } from "./format";
import { isInternalSymbol } from "./highlight-kif";

// Statement framing / annotation vocabulary -- not SUMO symbols, never linked.
const TPTP_KEYWORDS = new Set([
  "fof",
  "cnf",
  "tff",
  "thf",
  "tpi",
  "axiom",
  "hypothesis",
  "conjecture",
  "negated_conjecture",
  "plain",
  "lemma",
  "theorem",
  "corollary",
  "definition",
  "assumption",
  "unknown",
  "type",
  "include",
  "inference",
  "introduced",
  "file",
  "status",
  "esa",
  "thm",
  "cth",
  "sab",
]);

const TPTP_TOKEN_RE =
  /(%[^\n]*)|("(?:[^"\\]|\\.)*")|('(?:[^'\\]|\\.)*')|([()[\]])|([A-Z][A-Za-z0-9_]*)|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)|(<=>|<~>|<=|=>|~[|&]|!=|[!?:,.~&|=])|([a-z][A-Za-z0-9_]*)/g;

/** Strip a TPTP single-quoted atom's surrounding quotes and its backslash
 *  escapes, recovering the underlying symbol name for `data-sym`/`focusSymbol`. */
function unquoteAtom(src: string): string {
  return src.slice(1, -1).replace(/\\(.)/g, "$1");
}

/** Tokenize TPTP `src` into `tok-*` spans, with the same `focusSymbol` /
 *  `linkSymbols` treatment as `highlightKif`. */
export function highlightTptp(
  src: string,
  {
    focusSymbol,
    linkSymbols,
  }: { focusSymbol?: string; linkSymbols?: boolean } = {},
): string {
  let out = "",
    last = 0,
    m: RegExpExecArray | null,
    afterOpenParen = false;
  TPTP_TOKEN_RE.lastIndex = 0;
  while ((m = TPTP_TOKEN_RE.exec(src))) {
    out += esc(src.slice(last, m.index));
    const [, comment, dstr, qstr, paren, variable, num, op, word] = m;
    if (comment) {
      out += `<span class="tok-com">${esc(comment)}</span>`;
      afterOpenParen = false;
    } else if (dstr) {
      out += `<span class="tok-str">${esc(dstr)}</span>`;
      afterOpenParen = false;
    } else if (qstr) {
      const name = unquoteAtom(qstr);
      let tok = `<span class="tok-str">${esc(qstr)}</span>`;
      if (focusSymbol && name === focusSymbol) {
        tok = `<span class="sym-focus">${tok}</span>`;
      } else if (linkSymbols && !isInternalSymbol(name)) {
        tok = `<a class="sym-link" data-sym="${escAttr(name)}">${tok}</a>`;
      }
      out += tok;
      afterOpenParen = false;
    } else if (paren) {
      out += `<span class="tok-paren">${esc(paren)}</span>`;
      afterOpenParen = paren === "(" || paren === "[";
    } else if (variable) {
      out += `<span class="tok-var">${esc(variable)}</span>`;
      afterOpenParen = false;
    } else if (num) {
      out += `<span class="tok-num">${esc(num)}</span>`;
      afterOpenParen = false;
    } else if (op) {
      out += `<span class="tok-kw">${esc(op)}</span>`;
      afterOpenParen = false;
    } else if (word) {
      const isKw = TPTP_KEYWORDS.has(word);
      let tok: string;
      if (isKw) tok = `<span class="tok-kw">${esc(word)}</span>`;
      else if (afterOpenParen)
        tok = `<span class="tok-fn">${esc(word)}</span>`; // relation/function symbol
      else tok = esc(word);
      if (!isKw && focusSymbol && word === focusSymbol) {
        tok = `<span class="sym-focus">${tok}</span>`;
      } else if (linkSymbols && !isKw && !isInternalSymbol(word)) {
        tok = `<a class="sym-link" data-sym="${escAttr(word)}">${tok}</a>`;
      }
      out += tok;
      afterOpenParen = false;
    }
    last = TPTP_TOKEN_RE.lastIndex;
  }
  out += esc(src.slice(last));
  return out + "\n";
}
