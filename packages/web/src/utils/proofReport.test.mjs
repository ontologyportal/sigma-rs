import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { webcrypto } from "node:crypto";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";

function load(name) {
  const exports = {};
  vm.runInNewContext(
    ts.transpileModule(readFileSync(new URL(name, import.meta.url), "utf8"), {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    }).outputText,
    {
      exports,
      crypto: webcrypto,
      TextEncoder,
      TextDecoder,
      require: (name) => load(name + ".ts"),
    },
  );
  return exports;
}
const {
  formatProofReport,
  parseProofReport,
  assertProofCompatible,
  proofReplayOutcome,
  proofIssueBody,
  proofReportFromIssue,
} = load("./proofReport.ts");
const { verifyReportSources } = load("./auditReplay.ts");

const parse = (_file, text) => text.split(" ").filter(Boolean);

test("snapshot verification rejects stale, missing, duplicate and additional sources", () => {
  const files = [{ name: "custom.kif", text: "a b" }];
  const live = [{ file: "custom.kif", keys: ["b", "a"] }];
  verifyReportSources(files, live, parse);
  assert.throws(
    () =>
      verifyReportSources(files, [{ file: "custom.kif", keys: ["a"] }], parse),
    /differs/,
  );
  assert.throws(() => verifyReportSources(files, [], parse), /differs/);
  assert.throws(
    () => verifyReportSources([...files, ...files], live, parse),
    /Duplicate/,
  );
  assert.throws(() => verifyReportSources([], live, parse), /additional/);
  assert.throws(
    () =>
      verifyReportSources(files, live, () => {
        throw Error("parse error");
      }),
    /parse error/,
  );
  verifyReportSources([], [], parse);
});

const engine = { commit: "b".repeat(40), fingerprint: "c".repeat(64) };
const axiom = {
  file: "custom.kif",
  line: 2,
  rule: "axiom",
  kif: "(instance Rex Dog)",
  index: 0,
  premises: [],
};
const source = {
  repository: "github:ontologyportal/sumo",
  branch: "master",
  path: "custom.kif",
  commit: "a".repeat(40),
};
const manifest = {
  constituents: [{ name: "custom.kif", sha256: "d".repeat(64), source }],
  fingerprint: "e".repeat(64),
};
const report = {
  format: "sigma-proof-report",
  version: 1,
  created: "2026-10-21",
  engine,
  prover: { backend: "native", version: engine.commit, binaries: {} },
  problem: { command: "audit", request: { seed: 7, step: 3, count: 1 } },
  config: { backend: "native" },
  constituents: [
    { name: "custom.kif", text: "PRIVATE UNRELATED CONTENT", source },
  ],
  result: {
    steps: [axiom],
    raw_output: "UNRELATED OUTPUT",
    input_tptp: "UNRELATED INPUT",
  },
  unavailable: "",
};

test("verified report exports pinned hashes and relevant proof without file contents", async () => {
  const text = await formatProofReport(report, manifest);
  const parsed = parseProofReport(text);
  assert.equal(parsed.version, 3);
  assert.equal(parsed.constituents[0].source.commit, source.commit);
  assert.equal(parsed.axioms[0].kif, axiom.kif);
  assert.ok(!text.includes("UNRELATED"));
  assert.ok(!text.includes('"text"'));
  await assert.rejects(formatProofReport(report), /Verify branch HEAD/);
  await assert.rejects(
    formatProofReport({ ...report, unavailable: "stale snapshot" }, manifest),
    /stale snapshot/,
  );
});

test("reports retain a test's minimum translation language", async () => {
  const text = await formatProofReport(
    { ...report, config: { ...report.config, minLang: "tff" } },
    manifest,
  );
  assert.equal(parseProofReport(text).config.minLang, "tff");
  const invalid = JSON.parse(text);
  invalid.config.minLang = "typo";
  assert.throws(
    () => parseProofReport(JSON.stringify(invalid)),
    /configuration/,
  );
});

test("a SUMO issue carries the compact report and rejects missing or oversized payloads", async () => {
  const json = await formatProofReport(report, manifest);
  const body = proofIssueBody(json, "This contradiction affects Oregon.");
  assert.match(body, /This contradiction affects Oregon/);
  assert.ok(!body.includes("PRIVATE UNRELATED CONTENT"));
  assert.equal(proofReportFromIssue(body).fingerprint, manifest.fingerprint);
  assert.throws(
    () => proofReportFromIssue("Ordinary SUMO issue"),
    /no reproducible/,
  );
  assert.throws(() =>
    proofReportFromIssue(body.replace('"version":3', '"version":2')),
  );
  assert.throws(() => proofIssueBody(json, "x".repeat(65_000)), /too large/);
});

test("parser rejects legacy reports, unsafe sources, duplicate files, altered proofs and unsupported settings", async () => {
  const json = await formatProofReport(report, manifest);
  for (const mutate of [
    (r) => {
      r.version = 2;
    },
    (r) => {
      r.constituents[0].source.path = "../private.kif";
    },
    (r) => {
      r.constituents[0].source.repository = "https://localhost/private";
    },
    (r) => {
      r.constituents[0].source.commit = "master";
    },
    (r) => {
      r.constituents[0].source.branch = "branch with space";
    },
    (r) => {
      r.constituents[0].text = "unexpected file content";
    },
    (r) => {
      r.constituents.push(r.constituents[0]);
    },
    (r) => {
      r.axioms[0].kif = "FALSE";
    },
    (r) => {
      r.config.backend = "shell";
    },
    (r) => {
      r.problem.request.at = { file: "missing.kif", offset: 10 };
    },
    (r) => {
      r.problem.request.count = -1;
    },
    (r) => {
      r.problem.command = "newSession";
    },
  ]) {
    const changed = JSON.parse(json);
    mutate(changed);
    assert.throws(() => parseProofReport(JSON.stringify(changed)));
  }
});

test("compatibility requires exact engine and external prover identities", async () => {
  const r = parseProofReport(await formatProofReport(report, manifest));
  assertProofCompatible(r, engine, {});
  assert.throws(() => assertProofCompatible(r, null, {}), /engine/);
  assert.throws(
    () =>
      assertProofCompatible(r, { ...engine, fingerprint: "f".repeat(64) }, {}),
    /engine/,
  );
  r.prover = {
    backend: "vampire",
    binaries: { "vampire/vampire.wasm": "a".repeat(64) },
  };
  assert.throws(() => assertProofCompatible(r, engine, {}), /prover/);
  assertProofCompatible(r, engine, r.prover.binaries);
});

test("replay compares source axioms and does not call a timeout a repaired contradiction", async () => {
  const r = parseProofReport(await formatProofReport(report, manifest));
  assert.match(
    proofReplayOutcome(r, {
      contradictions: [{ steps: [axiom] }],
      batches: [],
    }),
    /reproduced with/,
  );
  assert.match(
    proofReplayOutcome(r, {
      contradictions: [],
      batches: [{ status: "Timeout" }],
    }),
    /Inconclusive/,
  );
  assert.match(
    proofReplayOutcome(r, {
      contradictions: [],
      batches: [{ status: "Consistent", stop_reason: null }],
    }),
    /did not reproduce/,
  );
});

test("conjecture timeouts remain inconclusive even when the report also timed out", async () => {
  const r = parseProofReport(await formatProofReport(report, manifest));
  r.problem = { command: "prove", query: "(instance Rex Dog)" };
  r.result.status = "Timeout";
  assert.match(
    proofReplayOutcome(r, { status: "Timeout", proved: false }),
    /Inconclusive/,
  );
});
