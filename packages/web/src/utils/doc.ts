/** Documentation-string helpers shared by the man-page layouts. */

import { esc } from "./format";

/** Turn `&%Symbol` cross-reference markers in documentation text into man-page links. */
export function linkifyDoc(text: unknown): string {
  return String(text)
    .split(/(&%[A-Za-z0-9_-]+)/)
    .map((part) => {
      const m = part.match(/^&%([A-Za-z0-9_-]+)$/);
      return m
        ? `<a class="open xref" data-sym="${esc(m[1])}">${esc(m[1])}</a>`
        : esc(part);
    })
    .join("");
}

/** Entries in `language`, falling back to the engine's default language then
 *  to all, so a symbol never renders blank just because it lacks the chosen
 *  language. */
export function entriesForLanguage<T extends { language: string }>(
  entries: T[],
  language: string,
  defaultLanguage: string,
): T[] {
  const pick = (lang: string) => entries.filter((d) => d.language === lang);
  return pick(language).length
    ? pick(language)
    : pick(defaultLanguage).length
      ? pick(defaultLanguage)
      : entries;
}
