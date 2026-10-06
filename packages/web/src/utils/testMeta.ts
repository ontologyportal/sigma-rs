/**
 * The `.kif.tq` harness directives Ask/Tell edits beside its panes: note,
 * categories, expected answer and required files, as form fields (lists
 * comma-separated). A test's `(time N)` is not here -- Ask/Tell keeps it as
 * the prover options' time limit, so there is one time input.
 *
 * Pure: no Vue, type-only imports (unit-tested in isolation).
 */

import type { ParsedTest } from "sigmakee/sdk";

export type ExpectedAnswer = "yes" | "no" | "bindings" | "none";

export interface TestMeta {
  note: string;
  categories: string;
  answer: ExpectedAnswer;
  /** Space-separated, for `answer: "bindings"`. */
  bindings: string;
  files: string;
}

/** A new test's details: expects a proof, nothing else set. */
export const emptyTestMeta = (): TestMeta => ({
  note: "",
  categories: "",
  answer: "yes",
  bindings: "",
  files: "",
});

/** A comma- (or newline-) separated form field as a list. */
export const splitList = (s: string): string[] =>
  s
    .split(/[,\n]/)
    .map((x) => x.trim())
    .filter(Boolean);

/** The form for a parsed test (a default note -- the file name -- is left
 *  blank). */
export function metaFromTest(p: ParsedTest): TestMeta {
  return {
    note: p.noteGiven ? p.note : "",
    categories: (p.categories ?? []).join(", "),
    answer: p.expectedAnswer?.length
      ? "bindings"
      : p.expectedProof === true
        ? "yes"
        : p.expectedProof === false
          ? "no"
          : "none",
    bindings: (p.expectedAnswer ?? []).join(" "),
    files: (p.extraFiles ?? []).join(", "),
  };
}

/** The proof outcome `m` expects (bindings imply a proof); null for none. */
export const expectedProof = (m: TestMeta): boolean | null =>
  m.answer === "none" ? null : m.answer !== "no";

/** The details' one-line summary for their fold. */
export function metaSummary(m: TestMeta): string {
  const parts: string[] = [];
  if (m.note) parts.push(m.note);
  const cats = splitList(m.categories);
  if (cats.length) parts.push(cats.join(", "));
  if (m.answer !== "none")
    parts.push(
      `expects ${m.answer === "bindings" ? m.bindings || "bindings" : m.answer}`,
    );
  return parts.length ? `Test details — ${parts.join(" · ")}` : "Test details";
}

/** `m` as `formatTest`'s directive options. */
export function testDirectives(m: TestMeta) {
  return {
    note: m.note.trim(),
    categories: splitList(m.categories),
    extraFiles: splitList(m.files),
    expectedProof: expectedProof(m),
    expectedAnswer:
      m.answer === "bindings" ? m.bindings.split(/\s+/).filter(Boolean) : null,
  };
}
