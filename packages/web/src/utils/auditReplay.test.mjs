import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createHash, webcrypto } from "node:crypto";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";

const exports = {};
vm.runInNewContext(
  ts.transpileModule(
    readFileSync(new URL("./auditReplay.ts", import.meta.url), "utf8"),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    },
  ).outputText,
  { exports, crypto: webcrypto, TextEncoder, TextDecoder },
);
const {
  parseAuditReplay,
  assertReplayCompatible,
  verifyReplayFiles,
  replayPositions,
  replayAxiomKey,
} = exports;
const hash = (b) => createHash("sha256").update(b).digest();
const commit = "a".repeat(40);
const engine = { commit: "b".repeat(40), fingerprint: "c".repeat(64) };
const run = { id: 12, run_attempt: 1, head_sha: commit };

export function fixture() {
  const files = [
    {
      name: "Merge.kif",
      bytes: new TextEncoder().encode("(instance A B)\r\n"),
    },
    {
      name: "Animals.kif",
      bytes: new TextEncoder().encode(
        '(documentation A EnglishLanguage "café")\n',
      ),
    },
  ];
  const digest = createHash("sha256");
  for (const file of files)
    digest.update(file.name + "\0").update(hash(file.bytes));
  const replay = {
    version: 1,
    complete: true,
    sumo_commit: commit,
    run_id: "12",
    run_attempt: 1,
    engine,
    fingerprint: digest.digest("hex"),
    constituents: files.map((f) => ({
      name: f.name,
      sha256: hash(f.bytes).toString("hex"),
    })),
    config: {
      backend: "native",
      timeLimitSecs: 10,
      maxSteps: 500000,
      maxLits: 12,
      forwardClose: true,
      wantProof: true,
      profile: false,
      selectionTolerancePct: 0,
    },
    request: { count: 1, batch: 1, limit: 64 },
    findings: [
      {
        seed: 0,
        step: 42,
        axioms: [{ file: "Merge.kif", line: 1, kif: "(instance A B)" }],
      },
    ],
  };
  return { files, replay };
}
const report = (r) =>
  `# Report\n\n\`\`\`lisp\n(instance A B)\n\`\`\`\n\n\`\`\`sigma-audit-replay\n${JSON.stringify(r)}\n\`\`\`\n`;

test("reads versioned metadata and preserves exact replay settings", () => {
  const { replay } = fixture();
  assert.deepEqual(
    JSON.parse(JSON.stringify(parseAuditReplay(report(replay)))),
    replay,
  );
  assert.throws(() => parseAuditReplay("# Legacy report"), /metadata/);
  assert.throws(
    () => parseAuditReplay(report(replay) + report(replay)),
    /metadata/,
  );
});

test("rejects incomplete, malformed, unsafe, or ambiguous replay inputs", () => {
  for (const change of [
    (r) => {
      r.complete = false;
    },
    (r) => {
      r.version = 2;
    },
    (r) => {
      r.constituents[0].name = "../Merge.kif";
    },
    (r) => {
      r.constituents.push(r.constituents[0]);
    },
    (r) => {
      r.findings[0].step = -1;
    },
    (r) => {
      r.findings[0].seed = 2 ** 32;
    },
    (r) => {
      r.request.batch = 2;
    },
    (r) => {
      r.config.backend = "vampire";
    },
    (r) => {
      r.config.timeLimitSecs = 0;
    },
    (r) => {
      r.findings[0].axioms = [];
    },
  ]) {
    const { replay } = fixture();
    change(replay);
    assert.throws(() => parseAuditReplay(report(replay)));
  }
});

test("master, workflow attempt, and actual engine inputs must match", () => {
  const { replay } = fixture();
  assertReplayCompatible(replay, commit, engine, run);
  assert.throws(
    () => assertReplayCompatible(replay, "d".repeat(40), engine, run),
    /master has changed/,
  );
  assert.throws(
    () =>
      assertReplayCompatible(replay, commit, engine, {
        ...run,
        run_attempt: 2,
      }),
    /not available/,
  );
  assert.throws(
    () => assertReplayCompatible(replay, commit, engine, { ...run, id: 13 }),
    /not available/,
  );
  assert.throws(
    () => assertReplayCompatible(replay, commit, null, run),
    /engine/,
  );
  assert.throws(
    () => assertReplayCompatible(replay, commit, { fingerprint: "other" }, run),
    /engine/,
  );
});

test("verifies every byte, constituent order, and the combined fingerprint", async () => {
  const { replay, files } = fixture();
  const texts = await verifyReplayFiles(replay, files);
  assert.equal(texts[0], "(instance A B)\r\n");
  assert.match(texts[1], /café/);
  await assert.rejects(
    verifyReplayFiles(replay, files.slice(1)),
    /count mismatch/,
  );
  await assert.rejects(
    verifyReplayFiles(replay, [...files].reverse()),
    /file mismatch/,
  );
  await assert.rejects(
    verifyReplayFiles({ ...replay, fingerprint: "0".repeat(64) }, files),
    /fingerprint mismatch/,
  );
  files[0] = {
    ...files[0],
    bytes: new TextEncoder().encode("(instance LocalWork Changed)"),
  };
  await assert.rejects(verifyReplayFiles(replay, files), /file mismatch/);
});

test("sparse replay skips gaps, deduplicates positions, and preserves different seeds", () => {
  const { replay } = fixture();
  replay.findings = [42, 900, 42].map((step) => ({ seed: 0, step }));
  replay.findings.push({ seed: 1, step: 42 });
  assert.equal(
    JSON.stringify(replayPositions(replay)),
    JSON.stringify([
      { seed: 0, step: 42 },
      { seed: 0, step: 900 },
      { seed: 1, step: 42 },
    ]),
  );
});

test("proof comparison ignores order and repeated source axioms", () => {
  const a = { file: "Merge.kif", line: 1, kif: "(p)" };
  const b = { file: "Animals.kif", line: 2, kif: "(not (p))" };
  assert.equal(replayAxiomKey([a, b]), replayAxiomKey([b, a, a]));
  assert.notEqual(replayAxiomKey([a, b]), replayAxiomKey([a]));
});

test("CLI flat and browser pretty KIF compare equally without changing quoted strings", () => {
  const a = {
    file: "Merge.kif",
    line: 1,
    kif: '(=> (p ?X) (q "two  words" ?X))',
  };
  const b = { ...a, kif: '(=>\n  (p ?X)\n  (q "two  words" ?X))' };
  assert.equal(replayAxiomKey([a]), replayAxiomKey([b]));
  assert.notEqual(
    replayAxiomKey([a]),
    replayAxiomKey([{ ...b, kif: b.kif.replace("two  words", "two words") }]),
  );
});
