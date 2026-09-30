import type { ProverConfig } from "../stores/prover";

export interface ReplayAxiom {
  file: string | null;
  line: number | null;
  kif: string;
}

export interface AuditReplay {
  version: 1;
  complete: true;
  sumo_commit: string;
  run_id: string;
  run_attempt: number;
  engine: { commit: string; fingerprint: string };
  fingerprint: string;
  constituents: { name: string; sha256: string }[];
  config: Required<Omit<ProverConfig, "vampireArgs">>;
  request: { count: 1; batch: 1; limit: number };
  findings: { seed: number; step: number; axioms: ReplayAxiom[] }[];
}

const sha = (value: unknown, length: number): value is string =>
  typeof value === "string" && new RegExp(`^[0-9a-f]{${length}}$`).test(value);
const integer = (value: unknown, min = 0): value is number =>
  Number.isSafeInteger(value) &&
  Number(value) >= min &&
  Number(value) <= 0xffffffff;
const record = (v: unknown): v is Record<string, unknown> =>
  v !== null && typeof v === "object" && !Array.isArray(v);

/** Read the versioned replay block; prose and source-axiom code fences are not executable. */
export function parseAuditReplay(markdown: string): AuditReplay {
  const blocks = [
    ...markdown.matchAll(/^```sigma-audit-replay\r?\n([\s\S]*?)^```\s*$/gm),
  ];
  if (blocks.length !== 1)
    throw new Error(
      "This report has no supported replay metadata. A new master audit is required.",
    );
  const r: unknown = JSON.parse(blocks[0][1]);
  if (
    !record(r) ||
    r.version !== 1 ||
    r.complete !== true ||
    !sha(r.sumo_commit, 40) ||
    typeof r.run_id !== "string" ||
    !/^\d+$/.test(r.run_id) ||
    !integer(r.run_attempt, 1) ||
    !sha(r.fingerprint, 64) ||
    !record(r.engine) ||
    !sha(r.engine.commit, 40) ||
    !sha(r.engine.fingerprint, 64)
  )
    throw new Error("Incomplete or invalid audit replay metadata.");
  if (
    !Array.isArray(r.constituents) ||
    !r.constituents.length ||
    r.constituents.length > 1000 ||
    r.constituents.some(
      (c: unknown) =>
        !record(c) ||
        typeof c.name !== "string" ||
        !/^[A-Za-z0-9_-]+(?:\/[A-Za-z0-9_-]+)*\.kif$/.test(c.name) ||
        !sha(c.sha256, 64),
    ) ||
    new Set(r.constituents.map((c: { name: string }) => c.name)).size !==
      r.constituents.length
  )
    throw new Error("Invalid or duplicate replay constituents.");
  const c = r.config;
  if (
    !record(c) ||
    c.backend !== "native" ||
    !integer(c.timeLimitSecs, 1) ||
    !integer(c.maxSteps, 1) ||
    !integer(c.maxLits, 1) ||
    c.forwardClose !== true ||
    c.wantProof !== true ||
    c.profile !== false ||
    c.selectionTolerancePct !== 0 ||
    !record(r.request) ||
    r.request.count !== 1 ||
    r.request.batch !== 1 ||
    !integer(r.request.limit, 1)
  )
    throw new Error("Unsupported replay prover settings.");
  if (
    !Array.isArray(r.findings) ||
    r.findings.some(
      (f: unknown) =>
        !record(f) ||
        !integer(f.seed) ||
        !integer(f.step) ||
        !Array.isArray(f.axioms) ||
        !f.axioms.length ||
        f.axioms.some(
          (a: unknown) =>
            !record(a) ||
            typeof a.kif !== "string" ||
            !(a.file === null || typeof a.file === "string") ||
            !(a.line === null || integer(a.line, 1)),
        ),
    )
  )
    throw new Error(
      "Invalid contradiction replay coordinates or source axioms.",
    );
  return r as unknown as AuditReplay;
}

/** Fail closed before replacing the KB, including when master advanced during a download. */
export function assertReplayCompatible(
  replay: AuditReplay,
  master: string | null,
  engine: { fingerprint: string } | null,
  run: { id: number | bigint; run_attempt?: number; head_sha: string },
): void {
  if (!master || replay.sumo_commit !== master || run.head_sha !== master)
    throw new Error(
      "Invalid report: SUMO master has changed since this audit. Wait for a new master audit.",
    );
  if (
    String(run.id) !== replay.run_id ||
    run.run_attempt !== replay.run_attempt
  )
    throw new Error(
      "The latest completed master audit's report is not available yet.",
    );
  if (!engine || engine.fingerprint !== replay.engine.fingerprint)
    throw new Error(
      "This app's engine does not match the workflow engine. Rebuild or update the app and audit from matching engine sources.",
    );
}

const hex = (bytes: Uint8Array) =>
  Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");

/** Hash original UTF-8 bytes in manifest order, using the nightly runner's algorithm. */
export async function verifyReplayFiles(
  replay: AuditReplay,
  files: { name: string; bytes: Uint8Array<ArrayBuffer> }[],
): Promise<string[]> {
  if (files.length !== replay.constituents.length)
    throw new Error("Replay constituent count mismatch.");
  const encoder = new TextEncoder();
  const parts: Uint8Array[] = [];
  const texts: string[] = [];
  for (let i = 0; i < files.length; i++) {
    const file = files[i];
    const expected = replay.constituents[i];
    const hash = new Uint8Array(
      await crypto.subtle.digest("SHA-256", file.bytes),
    );
    if (file.name !== expected.name || hex(hash) !== expected.sha256)
      throw new Error(`Replay file mismatch: ${expected.name}.`);
    parts.push(encoder.encode(file.name + "\0"), hash);
    texts.push(
      new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(
        file.bytes,
      ),
    );
  }
  const all = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let offset = 0;
  for (const part of parts) {
    all.set(part, offset);
    offset += part.length;
  }
  if (
    hex(new Uint8Array(await crypto.subtle.digest("SHA-256", all))) !==
    replay.fingerprint
  )
    throw new Error("Replay KB fingerprint mismatch.");
  return texts;
}

/** One call for each reported sweep position, without visiting the gaps. */
export function replayPositions(
  replay: AuditReplay,
): { seed: number; step: number }[] {
  return [
    ...new Map(
      replay.findings.map(({ seed, step }) => [
        `${seed}:${step}`,
        { seed, step },
      ]),
    ).values(),
  ];
}

/** Compare source axiom sets, not proof ordering or derivation length. */
export function replayAxiomKey(axioms: ReplayAxiom[]): string {
  return JSON.stringify(
    [
      ...new Set(
        axioms.map((a) =>
          JSON.stringify([
            a.file,
            a.line,
            // CLI uses flat KIF; WASM pretty-prints it. Keep quoted strings exact.
            (
              a.kif.match(/"(?:\\[\s\S]|[^"\\])*"|;[^\n]*|[()]|[^\s()";]+/g) ??
              []
            ).filter((token) => !token.startsWith(";")),
          ]),
        ),
      ),
    ].sort(),
  );
}
