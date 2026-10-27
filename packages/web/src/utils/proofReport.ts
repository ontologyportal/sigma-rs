import type {
  AuditRequest,
  AuditResult,
  AskResult,
  AuditStep,
} from "sigmakee/sdk";
import { inputSteps } from "./contradictionReport";
import type { ProverConfig } from "../worker/handlers";

import {
  replayAxiomKey,
  type ReplayAxiom,
  type ReportFile,
} from "./auditReplay";

/** Immutable inputs and output of one worker invocation. */
export interface ProofReport {
  format: "sigma-proof-report";
  version: 1;
  created: string;
  engine: { commit: string; fingerprint: string } | null;
  app: unknown;
  prover: {
    backend: string;
    version: string | null;
    binaries: Record<string, string | null>;
  };
  problem: unknown;
  config: ProverConfig;
  defaults: unknown;
  constituents: ReportFile[];
  result: unknown;
  unavailable: string;
}

export interface PinnedConstituent {
  name: string;
  sha256: string;
  source: { repository: string; branch: string; path: string; commit: string };
}

export type SharedProblem =
  | {
      command: "prove";
      assertions?: string;
      query: string;
      session?: string;
      tptp?: boolean;
    }
  | { command: "audit"; request: AuditRequest };

/** Version 3 contains only pinned references, never file contents. */
export interface SharedProofReport {
  format: "sigma-proof-report";
  version: 3;
  created: string;
  engine: { commit: string; fingerprint: string };
  prover: ProofReport["prover"];
  app: unknown;
  problem: SharedProblem;
  config: ProverConfig;
  constituents: PinnedConstituent[];
  fingerprint: string;
  axioms: ReplayAxiom[];
  result: { status: string; proof: AuditStep[] };
}

const ISSUE_REPORT_START = "<!-- sigma-proof-report:v3:start -->";
const ISSUE_REPORT_END = "<!-- sigma-proof-report:v3:end -->";

/** GitHub issue bodies carry the same validated, pinned report as a JSON download. */
export function proofIssueBody(json: string, description: string): string {
  const compact = JSON.stringify(parseProofReport(json));
  const body = [
    description.trim(),
    "",
    "<details>",
    "<summary>Reproducible proof report (JSON)</summary>",
    "",
    ISSUE_REPORT_START,
    "````json",
    compact,
    "````",
    ISSUE_REPORT_END,
    "</details>",
  ].join("\n");
  if (body.length > 65_000)
    throw new Error(
      "The proof report is too large for a GitHub issue. Copy or download the JSON report instead.",
    );
  return body;
}

/** Extract the pinned report from a SUMO issue before any workspace changes. */
export function proofReportFromIssue(body: string): SharedProofReport {
  const start = body.lastIndexOf(ISSUE_REPORT_START);
  const end = body.lastIndexOf(ISSUE_REPORT_END);
  if (start < 0 || end <= start)
    throw new Error("This issue has no reproducible proof report.");
  const block = body.slice(start + ISSUE_REPORT_START.length, end).trim();
  const match = /^````json\n([\s\S]*?)\n````$/.exec(block);
  if (!match) throw new Error("The issue's proof report is malformed.");
  return parseProofReport(match[1]);
}

const record = (v: unknown): v is Record<string, unknown> =>
  !!v && typeof v === "object" && !Array.isArray(v);
const hash = (v: unknown, n: number): v is string =>
  typeof v === "string" && new RegExp(`^[0-9a-f]{${n}}$`).test(v);
const uint = (v: unknown): v is number =>
  Number.isSafeInteger(v) && Number(v) >= 0 && Number(v) <= 0xffffffff;
const str = (v: unknown): v is string => typeof v === "string";
const only = (v: Record<string, unknown>, keys: string[]) =>
  Object.keys(v).every((k) => keys.includes(k));
const scalar = (v: unknown): boolean =>
  v === null ||
  typeof v === "boolean" ||
  (typeof v === "number" && Number.isFinite(v)) ||
  (str(v) && v.length < 1000);

/** GitHub-only source identifiers keep imported reports away from arbitrary URLs. */
export function reportRepository(repository: unknown): {
  owner: string;
  repo: string;
} {
  const m =
    str(repository) &&
    /^github:([A-Za-z0-9_-]+)\/([A-Za-z0-9_.-]+)$/.exec(repository);
  if (!m || m[2] === "." || m[2] === "..")
    throw new Error(
      "Reports require a GitHub repository for every constituent.",
    );
  return { owner: m[1], repo: m[2] };
}

export function reportPath(v: unknown): v is string {
  return (
    str(v) &&
    v.length <= 1000 &&
    v.endsWith(".kif") &&
    v
      .split("/")
      .every(
        (p) =>
          /^[A-Za-z0-9_& .()-]+$/.test(p) &&
          p !== "." &&
          p !== ".." &&
          p.trim() === p,
      )
  );
}

export function reportBranch(v: unknown): v is string {
  return (
    str(v) &&
    v.length > 0 &&
    v.length < 256 &&
    !/^[0-9a-f]{40}$/i.test(v) &&
    !/[\\\s~^:?*[\]@]/.test(v) &&
    !v.includes("..") &&
    v
      .split("/")
      .every(
        (p) =>
          !!p && !p.startsWith(".") && !p.endsWith(".") && !p.endsWith(".lock"),
      )
  );
}

/** Validate an untrusted report before any download or workspace mutation. */
export function parseProofReport(json: string): SharedProofReport {
  if (json.length > 8_000_000)
    throw new Error("Proof report exceeds the 8 MB limit.");
  let r: unknown;
  try {
    r = JSON.parse(json);
  } catch {
    throw new Error("Invalid proof report JSON.");
  }
  if (!record(r) || r.format !== "sigma-proof-report" || r.version !== 3)
    throw new Error(
      "This report has no verified pinned inputs. Share it again using the updated app (version 3 report).",
    );
  if (
    !str(r.created) ||
    !record(r.engine) ||
    !hash(r.engine.commit, 40) ||
    !hash(r.engine.fingerprint, 64) ||
    !hash(r.fingerprint, 64)
  )
    throw new Error("Missing or invalid engine/KB identity.");
  if (!Array.isArray(r.constituents) || r.constituents.length > 1000)
    throw new Error("Invalid constituent list.");
  const names = new Set<string>();
  const refs = new Map<string, string>();
  for (const f of r.constituents) {
    if (
      !record(f) ||
      !only(f, ["name", "sha256", "source"]) ||
      !reportPath(f.name) ||
      names.has(f.name) ||
      !hash(f.sha256, 64) ||
      !record(f.source)
    )
      throw new Error("Invalid or duplicate report constituent.");
    names.add(f.name);
    const src = f.source;
    const { owner, repo } = reportRepository(src.repository);
    if (
      !only(src, ["repository", "branch", "path", "commit"]) ||
      !reportBranch(src.branch) ||
      !reportPath(src.path) ||
      !hash(src.commit, 40)
    )
      throw new Error("Invalid pinned source reference.");
    if (f.name !== src.path && f.name !== `${owner}/${repo}/${src.path}`)
      throw new Error("Constituent name does not match its repository path.");
    const key = `${src.repository}@${src.branch}`;
    if (refs.has(key) && refs.get(key) !== src.commit)
      throw new Error("Conflicting commits for one branch.");
    refs.set(key, src.commit);
  }
  const c = r.config;
  const nums = [
    "timeLimitSecs",
    "maxSteps",
    "maxLits",
    "selectionTolerancePct",
    "selectionBudget",
    "auditSubsetLimit",
    "selectionTimeLimitSecs",
  ];
  const bools = ["forwardClose", "wantProof", "profile", "auditAxfilter"];
  if (
    !record(c) ||
    !only(c, [
      ...nums,
      ...bools,
      "backend",
      "minLang",
      "vampireArgs",
      "selection",
      "strategy",
    ]) ||
    nums.some((k) => k in c && !uint(c[k])) ||
    bools.some((k) => k in c && typeof c[k] !== "boolean") ||
    (c.backend !== undefined &&
      !["native", "vampire", "e"].includes(String(c.backend))) ||
    (c.minLang !== undefined &&
      !["fof", "tff", "thf"].includes(String(c.minLang))) ||
    (c.vampireArgs !== undefined &&
      (!str(c.vampireArgs) || c.vampireArgs.length > 10000)) ||
    ["selection", "strategy"].some(
      (k) =>
        k in c &&
        (!record(c[k]) ||
          Object.entries(c[k]).some(
            ([key, val]) => !/^[a-z][a-z0-9_]*$/.test(key) || !scalar(val),
          )),
    )
  )
    throw new Error("Invalid prover configuration.");
  if (
    !record(r.prover) ||
    r.prover.backend !== (c.backend ?? "native") ||
    !record(r.prover.binaries) ||
    Object.values(r.prover.binaries).some((v) => !hash(v, 64))
  )
    throw new Error("Invalid prover build identity.");
  const p = r.problem;
  if (!record(p)) throw new Error("Invalid problem.");
  if (p.command === "prove") {
    if (
      !only(p, ["command", "assertions", "query", "session", "tptp"]) ||
      !str(p.query) ||
      (p.assertions !== undefined && !str(p.assertions)) ||
      (p.session !== undefined && !str(p.session)) ||
      (p.tptp !== undefined && typeof p.tptp !== "boolean")
    )
      throw new Error("Invalid conjecture setup.");
  } else if (p.command === "audit") {
    const q = p.request;
    const coords = [
      "seed",
      "step",
      "count",
      "batch",
      "limit",
      "budget",
      "depth",
    ];
    if (
      !only(p, ["command", "request"]) ||
      !record(q) ||
      !only(q, [...coords, "scope", "at", "focus"]) ||
      coords.some((k) => k in q && !uint(q[k])) ||
      (q.scope !== undefined && !names.has(String(q.scope))) ||
      (q.at !== undefined &&
        (!record(q.at) ||
          !only(q.at, ["file", "offset"]) ||
          !names.has(String(q.at.file)) ||
          !uint(q.at.offset))) ||
      (q.focus !== undefined &&
        (!record(q.focus) ||
          !only(q.focus, ["file", "source", "kif", "unchanged"]) ||
          !names.has(String(q.focus.file)) ||
          !hash(q.focus.source, 16) ||
          !str(q.focus.kif) ||
          typeof q.focus.unchanged !== "boolean"))
    )
      throw new Error("Invalid audit coordinates.");
  } else throw new Error("Unsupported proof operation.");
  if (
    !record(r.result) ||
    !str(r.result.status) ||
    !Array.isArray(r.result.proof) ||
    r.result.proof.length > 100000 ||
    r.result.proof.some(
      (s) =>
        !record(s) ||
        !uint(s.index) ||
        !str(s.kif) ||
        !str(s.rule) ||
        !Array.isArray(s.premises) ||
        s.premises.some((v) => !uint(v)) ||
        (s.file != null && !str(s.file)) ||
        (s.line != null && (!uint(s.line) || s.line === 0)) ||
        (s.tptp != null && !str(s.tptp)),
    )
  )
    throw new Error("Invalid proof steps.");
  if (
    !Array.isArray(r.axioms) ||
    r.axioms.some(
      (a) =>
        !record(a) ||
        !str(a.kif) ||
        (a.file !== null && !str(a.file)) ||
        (a.line !== null && (!uint(a.line) || a.line === 0)),
    )
  )
    throw new Error("Invalid source axioms.");
  const expected = inputSteps(r.result.proof as unknown as AuditStep[]).map(
    (s) => ({ file: s.file ?? null, line: s.line ?? null, kif: s.kif }),
  );
  if (
    replayAxiomKey(expected) !==
      replayAxiomKey(r.axioms as unknown as ReplayAxiom[]) ||
    (p.command === "audit" && expected.length === 0)
  )
    throw new Error("Report axioms do not match the proof.");
  return r as unknown as SharedProofReport;
}

/** Require the recorded engine and backend binary before mutating the KB. */
export function assertProofCompatible(
  report: SharedProofReport,
  engine: ProofReport["engine"],
  binaries: Record<string, string | null>,
) {
  if (!engine || engine.fingerprint !== report.engine.fingerprint)
    throw new Error(
      "This app's engine differs from the report. Use a matching engine build before importing.",
    );
  const required =
    report.prover.backend === "vampire"
      ? ["vampire/vampire.wasm"]
      : report.prover.backend === "e"
        ? ["eprover/eprover.wasm", "eprover/e_axfilter.wasm"]
        : [];
  if (
    required.some(
      (key) => !binaries[key] || report.prover.binaries[key] !== binaries[key],
    )
  )
    throw new Error(
      "The external prover build differs from the report or is unavailable.",
    );
}

/** Export only after callers have verified the run's files against branch HEAD. */
export async function formatProofReport(
  report: ProofReport,
  manifest: { constituents: PinnedConstituent[]; fingerprint: string },
): Promise<string> {
  if (report.unavailable) throw new Error(report.unavailable);
  if (!manifest)
    throw new Error("Verify branch HEAD before sharing this result.");
  const result = report.result as {
    status?: string;
    proof?: AuditStep[];
    steps?: AuditStep[];
    contradictions?: { steps: AuditStep[] }[];
  };
  const steps =
    result.proof ??
    result.steps ??
    result.contradictions?.flatMap((c) => c.steps) ??
    [];
  const axioms = [
    ...new Map(
      inputSteps(steps).map((step) => {
        const axiom = {
          file: step.file ?? null,
          line: step.line ?? null,
          kif: step.kif,
        };
        return [JSON.stringify(axiom), axiom];
      }),
    ).values(),
  ];
  const json = JSON.stringify(
    {
      format: report.format,
      version: 3,
      created: report.created,
      engine: report.engine,
      app: report.app,
      prover: report.prover,
      problem: report.problem,
      config: report.config,
      ...manifest,
      axioms,
      result: { status: result.status ?? "Inconsistent", proof: steps },
    },
    null,
    2,
  );
  parseProofReport(json);
  return json;
}

/** Report an observed replay outcome without treating a timeout as a repair. */
export function proofReplayOutcome(
  report: SharedProofReport,
  result: AskResult | AuditResult,
): string {
  if (report.problem.command === "audit" && "contradictions" in result) {
    const key = replayAxiomKey(report.axioms);
    if (
      result.contradictions.some(
        (c) =>
          replayAxiomKey(
            inputSteps(c.steps).map((s) => ({
              file: s.file ?? null,
              line: s.line ?? null,
              kif: s.kif,
            })),
          ) === key,
      )
    )
      return "Contradiction reproduced with the reported source axioms.";
    if (
      !result.batches.length ||
      result.batches.some((b) => b.status !== "Consistent" || b.stop_reason)
    )
      return "Inconclusive: the reported contradiction was not reproduced within this run's limits.";
    return "The reported contradiction did not reproduce in the recorded checks.";
  }
  if (report.problem.command === "prove" && "proved" in result) {
    if (["Timeout", "Unknown", "InputError"].includes(result.status))
      return `Inconclusive: ${result.status}. The recorded result was ${report.result.status}.`;
    if (result.status === report.result.status)
      return `Result reproduced: ${result.status}.`;
    return `Result differs: expected ${report.result.status}, received ${result.status}. This does not establish that the original result was wrong.`;
  }
  return "Inconclusive: unexpected replay result.";
}
