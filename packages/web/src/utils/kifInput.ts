/**
 * Editor-style helpers for typing a KIF formula into a single-line input:
 * paren auto-pairing, balance checking, and term completion at the caret.
 * Pure: no Vue, no DOM.
 */

/** An edit result: the new text and where the caret goes. */
export interface Edit {
  text: string;
  caret: number;
}

/** `true` when `text` is formula input rather than a term search. */
export function isFormula(text: string): boolean {
  return text.trimStart().startsWith("(");
}

/** `true` when every paren outside a string literal is closed, in order. */
export function parensBalanced(text: string): boolean {
  let depth = 0;
  let inString = false;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (inString) {
      if (c === "\\") i++;
      else if (c === '"') inString = false;
    } else if (c === '"') inString = true;
    else if (c === "(") depth++;
    else if (c === ")" && --depth < 0) return false;
  }
  return depth === 0 && !inString;
}

/** `true` when `text` holds nothing but parens and whitespace, e.g. `()`. */
export function isBlankFormula(text: string): boolean {
  return /^[\s()]*$/.test(text);
}

/** Type `(` over the selection `[start, end)`: wrap it in a paren pair, caret
 *  just inside the `)`. */
export function openParen(text: string, start: number, end: number): Edit {
  const inner = text.slice(start, end);
  return {
    text: `${text.slice(0, start)}(${inner})${text.slice(end)}`,
    caret: start + 1 + inner.length,
  };
}

/** Type `(` in term mode: the whole term becomes the formula's body. */
export function termToFormula(text: string): Edit {
  return openParen(text, 0, text.length);
}

/** Type `)` at `caret`: step over an existing `)` instead of adding one.
 *  `null` when the next character is not `)` (type it normally). */
export function closeParen(text: string, caret: number): Edit | null {
  return text[caret] === ")" ? { text, caret: caret + 1 } : null;
}

/** Backspace at `caret` inside an empty `()` pair deletes both halves.
 *  `null` when the caret is not between a pair (delete normally). */
export function deletePair(text: string, caret: number): Edit | null {
  if (caret < 1 || text[caret - 1] !== "(" || text[caret] !== ")") return null;
  return {
    text: text.slice(0, caret - 1) + text.slice(caret + 1),
    caret: caret - 1,
  };
}

/** The symbol-name word ending at `caret`, as `[start, end)` (extending past
 *  the caret to the end of the word). `null` when the caret is not on a
 *  word, or the word is a variable (`?X`, `@ROW`). */
export function wordAt(
  text: string,
  caret: number,
): { start: number; end: number; word: string } | null {
  const isWordChar = (c: string | undefined) => !!c && /[\w-]/.test(c);
  let start = caret;
  while (isWordChar(text[start - 1])) start--;
  let end = caret;
  while (isWordChar(text[end])) end++;
  if (start === caret) return null;
  const before = text[start - 1];
  if (before === "?" || before === "@" || before === '"') return null;
  return { start, end, word: text.slice(start, end) };
}

/** Replace the word `[start, end)` with `symbol`, caret after it. */
export function acceptCompletion(
  text: string,
  start: number,
  end: number,
  symbol: string,
): Edit {
  return {
    text: text.slice(0, start) + symbol + text.slice(end),
    caret: start + symbol.length,
  };
}

/** Completion candidates for `word` from search-hit symbols: deduplicated,
 *  only names containing `word` (case-insensitive), prefix matches first,
 *  then shorter names, at most `limit`. */
export function rankCompletions(
  symbols: string[],
  word: string,
  limit = 8,
): string[] {
  const w = word.toLowerCase();
  const seen = new Set<string>();
  const out: { s: string; prefix: boolean }[] = [];
  for (const s of symbols) {
    const l = s.toLowerCase();
    if (seen.has(s) || !l.includes(w)) continue;
    seen.add(s);
    out.push({ s, prefix: l.startsWith(w) });
  }
  out.sort(
    (a, b) =>
      Number(b.prefix) - Number(a.prefix) ||
      a.s.length - b.s.length ||
      a.s.localeCompare(b.s),
  );
  return out.slice(0, limit).map((c) => c.s);
}
