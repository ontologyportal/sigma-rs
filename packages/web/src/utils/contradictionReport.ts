/**
 * Audit contradiction tooling: deciding whether a contradiction rests only
 * on official SUMO axioms and rendering it as a GitHub issue, and matching
 * validation diagnostics to the axioms it cites. Pure -- callers supply the
 * store-derived facts (which files are pristine upstream, file texts, the
 * current diagnostics).
 */

import type { AuditStep } from "sigmakee/sdk";
import type { Diagnostic } from "../stores/kb";

/** GitHub rejects issue bodies over 65536 characters; leave headroom. */
const MAX_BODY = 60_000;

/** Why a contradiction can or cannot be reported upstream. */
export interface Reportability {
  ok: boolean;
  /** Every distinct cited source file, sorted. */
  files: string[];
  /** Human-readable reasons it is not reportable (empty when `ok`). */
  blockers: string[];
}

/** Rules that mark a step as an input rather than an inference. */
const INPUT_RULES = new Set(["axiom", "hypothesis"]);

/** The input steps of a derivation: cited axioms, plus any input-rule step
 *  whose source could not be traced (a session assertion, for instance). */
const inputSteps = (steps: AuditStep[]) =>
  steps.filter((s) => s.file != null || INPUT_RULES.has(s.rule));

/**
 * A contradiction is reportable when every input axiom traces to a file for
 * which `isUpstream` holds. An input step with no source file blocks it:
 * its provenance is unknown, so it may be local.
 */
export function reportability(
  steps: AuditStep[],
  isUpstream: (file: string) => boolean,
): Reportability {
  const files = new Set<string>();
  const blocked = new Set<string>();
  let untraced = 0;
  for (const s of inputSteps(steps)) {
    if (s.file == null) {
      untraced++;
      continue;
    }
    files.add(s.file);
    if (!isUpstream(s.file)) blocked.add(s.file);
  }
  const blockers = [...blocked]
    .sort()
    .map((f) => `${f} is not an unmodified file from the SUMO repository`);
  if (untraced)
    blockers.push(
      `${untraced} axiom${untraced === 1 ? "" : "s"} could not be traced to a source file`,
    );
  if (!files.size && !untraced)
    blockers.push("the proof cites no source axioms");
  return { ok: blockers.length === 0, files: [...files].sort(), blockers };
}

/**
 * A short, stable id for a contradiction: SHA-256 over its cited axioms'
 * KIF, sorted, so the same contradiction found by different users (or after
 * unrelated line shifts) maps to the same id -- used to find an existing
 * report before filing a duplicate.
 */
export async function contradictionFingerprint(
  steps: AuditStep[],
): Promise<string> {
  const axioms = [...new Set(inputSteps(steps).map((s) => s.kif.trim()))];
  const data = new TextEncoder().encode(axioms.sort().join("\n"));
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", data));
  return Array.from(digest.slice(0, 8), (b) =>
    b.toString(16).padStart(2, "0"),
  ).join("");
}

/** The marker line embedded in every report, searched for to deduplicate. */
export const fingerprintMarker = (fp: string) =>
  `sigma-contradiction-id: ${fp}`;

export interface IssueInput {
  steps: AuditStep[];
  prose?: string;
  fingerprint: string;
  /** Link target for `file#Lline`, e.g. `https://github.com/o/r/blob/<sha>`. */
  blobBase: string;
  /** The commit the axioms were loaded at, when known. */
  commit?: string | null;
  /** Free-form context lines (app version, prover, time limit...). */
  context: string[];
  /** Validation findings on the cited axioms (see `axiomDiagnostics`),
   *  listed as possible causes; omitted from the body when empty. */
  diagnostics?: Diagnostic[];
}

const stepNumber = (s: AuditStep, pos: number) =>
  (s.index != null ? s.index : pos) + 1;

/** One proof step as plain text: `N. rule (from steps a, b)` + indented KIF. */
function stepText(s: AuditStep, pos: number): string {
  const from = s.premises?.length
    ? ` (from step${s.premises.length === 1 ? "" : "s"} ${s.premises.map((p) => p + 1).join(", ")})`
    : "";
  const src = s.file ? `  [${s.file}:${s.line ?? "?"}]` : "";
  const kif = s.kif
    .split("\n")
    .map((l) => `   ${l}`)
    .join("\n");
  return `${stepNumber(s, pos)}. ${s.rule}${from}${src}\n${kif}`;
}

/** Render the issue title and Markdown body for a reportable contradiction. */
export function buildContradictionIssue({
  steps,
  prose,
  fingerprint,
  blobBase,
  commit,
  context,
  diagnostics = [],
}: IssueInput): { title: string; body: string } {
  const axioms = steps.filter((s) => s.file != null);
  const files = [...new Set(axioms.map((s) => s.file as string))].sort();
  const title = `Contradictory axioms in ${files.join(", ")}`;

  const axiomList = axioms
    .map((s) => {
      const loc = `${s.file}:${s.line ?? "?"}`;
      const link =
        s.line != null ? `[${loc}](${blobBase}/${s.file}#L${s.line})` : loc;
      return `- ${link}\n\n  \`\`\`lisp\n${s.kif
        .split("\n")
        .map((l) => `  ${l}`)
        .join("\n")}\n  \`\`\``;
    })
    .join("\n");

  const head = [
    "The Sigma consistency audit derived a contradiction using only axioms from this repository.",
    "",
    commit ? `Loaded at commit ${commit}.` : "",
    "",
    "### Axioms involved",
    "",
    axiomList,
    "",
  ];
  if (diagnostics.length)
    head.push(
      "### Diagnostics on these axioms",
      "",
      "Sigma's validator flags the following on the axioms above; they may be the source of the contradiction.",
      "",
      ...diagnostics.map((d) => {
        const loc = `${d.file}:${d.line}`;
        return `- [${loc}](${blobBase}/${d.file}#L${d.line}) **${d.severity}** \`${d.kind}/${d.code}\`: ${d.message}`;
      }),
      "",
    );
  const tail = [
    "",
    "<details><summary>Context</summary>",
    "",
    ...context.map((c) => `- ${c}`),
    "",
    "</details>",
    "",
    `<!-- ${fingerprintMarker(fingerprint)} -->`,
    `<sub>${fingerprintMarker(fingerprint)}</sub>`,
  ];

  let proof = steps.map(stepText).join("\n");
  const proseBlock = prose?.trim()
    ? [
        "",
        "<details><summary>In English</summary>",
        "",
        prose.trim(),
        "",
        "</details>",
      ]
    : [];
  const assemble = (p: string, extra: string[]) =>
    [
      ...head,
      `### Full proof (${steps.length} steps)`,
      "",
      "```",
      p,
      "```",
      ...extra,
      ...tail,
    ]
      .filter((l, i, a) => !(l === "" && a[i - 1] === ""))
      .join("\n");

  let body = assemble(proof, proseBlock);
  if (body.length > MAX_BODY) {
    body = assemble(proof, []);
    if (body.length > MAX_BODY) {
      const overflow = body.length - MAX_BODY + 200;
      proof =
        proof.slice(0, Math.max(0, proof.length - overflow)) +
        "\n... (truncated: proof exceeds GitHub's issue size limit)";
      body = assemble(proof, []);
    }
  }
  return { title, body };
}

/**
 * The 1-based `[start, end]` lines of the form that begins on `line` of
 * `text`, found by paren matching (skipping strings and `;` comments).
 * Falls back to `[line, line]` when no balanced form starts there.
 */
export function formLineSpan(text: string, line: number): [number, number] {
  let i = 0;
  for (let l = 1; l < line; l++) {
    const nl = text.indexOf("\n", i);
    if (nl < 0) return [line, line];
    i = nl + 1;
  }
  let cur = line;
  let depth = 0;
  for (; i < text.length; i++) {
    const ch = text[i];
    if (ch === "\n") cur++;
    else if (ch === ";") {
      const nl = text.indexOf("\n", i);
      if (nl < 0) break;
      i = nl - 1;
    } else if (ch === '"') {
      for (i++; i < text.length && text[i] !== '"'; i++) {
        if (text[i] === "\\") i++;
        else if (text[i] === "\n") cur++;
      }
    } else if (ch === "(") depth++;
    else if (ch === ")" && depth > 0 && --depth === 0) return [line, cur];
  }
  return [line, line];
}

/** One cited axiom and the diagnostics that fall inside its source span. */
export interface AxiomDiagnostics {
  step: AuditStep;
  span: [number, number];
  diagnostics: Diagnostic[];
}

const SEVERITY_RANK: Record<string, number> = {
  error: 0,
  warning: 1,
  info: 2,
  hint: 3,
};

/**
 * For each distinct cited axiom (by `file:line`), the diagnostics located
 * within its form's lines, most severe first. `textOf` supplies a file's
 * source text (null when unavailable, which narrows the span to the start
 * line).
 */
export function axiomDiagnostics(
  steps: AuditStep[],
  diagnostics: Diagnostic[],
  textOf: (file: string) => string | null,
): AxiomDiagnostics[] {
  const seen = new Set<string>();
  const out: AxiomDiagnostics[] = [];
  for (const step of steps) {
    if (step.file == null || step.line == null) continue;
    const key = `${step.file}:${step.line}`;
    if (seen.has(key)) continue;
    seen.add(key);
    const text = textOf(step.file);
    const span: [number, number] =
      text == null ? [step.line, step.line] : formLineSpan(text, step.line);
    const hits = diagnostics
      .filter(
        (d) =>
          d.file === step.file &&
          d.line != null &&
          d.line <= span[1] &&
          (d.end_line ?? d.line) >= span[0],
      )
      .sort(
        (a, b) =>
          (SEVERITY_RANK[a.severity] ?? 9) - (SEVERITY_RANK[b.severity] ?? 9) ||
          (a.line ?? 0) - (b.line ?? 0),
      );
    out.push({ step, span, diagnostics: hits });
  }
  return out;
}

/** A stable identity for a contradiction across sampled-audit chunks: its
 *  cited source locations, sorted. The same contradiction reached from two
 *  different subproblems cites the same axioms. */
export function contradictionKey(steps: AuditStep[]): string {
  return [
    ...new Set(
      steps.filter((s) => s.file != null).map((s) => `${s.file}:${s.line}`),
    ),
  ]
    .sort()
    .join("|");
}

/** How a sampled audit's subproblems ended, for the result summary. */
export interface BatchSummary {
  total: number;
  clean: number;
  contradictory: number;
  timeLimit: number;
  stepLimit: number;
  crashed: number;
  other: number;
}

export function summarizeBatches(
  batches: { status: string; stop_reason: string | null }[],
): BatchSummary {
  const s: BatchSummary = {
    total: batches.length,
    clean: 0,
    contradictory: 0,
    timeLimit: 0,
    stepLimit: 0,
    crashed: 0,
    other: 0,
  };
  for (const b of batches) {
    if (b.status === "Consistent") s.clean++;
    else if (b.status === "Inconsistent") s.contradictory++;
    else if (b.stop_reason === "TimeLimit") s.timeLimit++;
    else if (b.stop_reason === "StepLimit") s.stepLimit++;
    else if (b.status === "Crashed") s.crashed++;
    else s.other++;
  }
  return s;
}

/** The result line for a set of checks: "N checks · M clean · ...". */
export function batchBreakdown(s: BatchSummary): string {
  const parts = [`${s.total} check${s.total === 1 ? "" : "s"}`];
  if (s.clean) parts.push(`${s.clean} clean`);
  if (s.contradictory) parts.push(`${s.contradictory} contradictory`);
  if (s.timeLimit) parts.push(`${s.timeLimit} timed out`);
  if (s.stepLimit) parts.push(`${s.stepLimit} hit the step cap`);
  if (s.crashed) parts.push(`${s.crashed} crashed`);
  if (s.other) parts.push(`${s.other} gave up`);
  return parts.join(" · ");
}

/** How one check ended (`Crashed`: it never returned), as the check log
 *  words it. */
export function checkLabel(b: {
  status: string;
  stop_reason: string | null;
}): string {
  if (b.status === "Consistent") return "clean";
  if (b.status === "Inconsistent") return "contradiction";
  if (b.status === "Crashed") return "crashed";
  switch (b.stop_reason) {
    case "TimeLimit":
      return "timed out";
    case "StepLimit":
      return "step cap";
    case "IncompleteLoad":
      return "incomplete load";
    default:
      return "gave up";
  }
}

/** The check log's dot colour for one check. */
export function checkTone(b: { status: string }): "ok" | "warn" | "bad" {
  if (b.status === "Consistent") return "ok";
  if (b.status === "Inconsistent" || b.status === "Crashed") return "bad";
  return "warn";
}

/** The source axioms a proof cites, as `file:line`s (the first three, then
 *  a count of the rest); empty when it cites none. */
export function citedSources(steps: AuditStep[]): string {
  const locs = [
    ...new Set(
      steps.filter((s) => s.file).map((s) => `${s.file}:${s.line ?? "?"}`),
    ),
  ];
  if (!locs.length) return "";
  const shown = locs.slice(0, 3).join(", ");
  return locs.length > 3 ? `${shown} (+${locs.length - 3} more)` : shown;
}

/** A proof's fold summary: its size and what it cites. */
export function proofSummary(steps: AuditStep[]): string {
  const cited = citedSources(steps);
  return `proof (${steps.length} step${steps.length === 1 ? "" : "s"})${cited ? ` · cites ${cited}` : ""}`;
}
