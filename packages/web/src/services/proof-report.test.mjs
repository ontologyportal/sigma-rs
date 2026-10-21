import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import { webcrypto } from "node:crypto";
import ts from "typescript";

const code = ts.transpileModule(
  readFileSync(new URL("./proof-report.ts", import.meta.url), "utf8"),
  {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  },
).outputText;

test("report retains run-time custom edits and configuration after the UI changes", async () => {
  const exports = {};
  const constituent = { file: "uploads/local.kif", text: "saved" };
  let finish;
  let sent;
  const imports = {
    "../models/Origin": { GitOrigin: class {}, originId: () => "source" },
    "../stores/changes": { useChangesStore: () => ({ record: () => null }) },
    "./sigma": {
      call: (_cmd, args) => {
        sent = structuredClone(args);
        return new Promise((resolve) => {
          finish = resolve;
        });
      },
    },
    "./lsp": { lspSyncedText: () => "unsaved edit" },
    "../stores/kb": { useKBStore: () => ({ constituents: [constituent] }) },
    "../stores/shell": {
      useShellStore: () => ({ version: { version: "1", commit: "abc" } }),
    },
    "../constants": {
      AUDIT_ENGINE: { commit: "abc", fingerprint: "def" },
      PROVER_BUILDS: { "vampire/vampire.wasm": "123" },
    },
  };
  vm.runInNewContext(code, { exports, require: (name) => imports[name] });
  const args = {
    config: { backend: "vampire", timeLimitSecs: 10 },
    query: "original",
  };
  const pending = exports.runWithReport("prove", args);
  args.query = "changed";
  args.config.timeLimitSecs = 99;
  constituent.text = "changed";
  finish({
    result: {
      status: "Theorem",
      raw_output: "% Version: Vampire 4.9",
      proof: [],
    },
    reportContext: {
      config: sent.config,
      problem: { command: "prove", query: sent.query },
      defaults: {},
      error: "",
    },
  });
  const { result } = await pending;
  assert.equal(result.shareReport.constituents[0].text, "unsaved edit");
  assert.equal(result.shareReport.config.timeLimitSecs, 10);
  assert.equal(result.shareReport.problem.query, "original");
  assert.equal(
    result.shareReport.prover.binaries["vampire/vampire.wasm"],
    "123",
  );
  assert.match(result.shareReport.prover.version, /Vampire 4.9/);
  assert.equal(result.shareReport.result.shareReport, undefined);
});

function utility(name) {
  const exports = {};
  vm.runInNewContext(
    ts.transpileModule(
      readFileSync(new URL(`../utils/${name}.ts`, import.meta.url), "utf8"),
      {
        compilerOptions: {
          module: ts.ModuleKind.CommonJS,
          target: ts.ScriptTarget.ES2022,
        },
      },
    ).outputText,
    {
      exports,
      crypto: webcrypto,
      TextEncoder,
      TextDecoder,
      require: (n) => utility(n.replace("./", "")),
    },
  );
  return exports;
}
const utils = utility("proofReport");
const replay = utility("auditReplay");
const head = "a".repeat(40);
const engine = { commit: "b".repeat(40), fingerprint: "c".repeat(64) };
const input = "(subclass Dog Mammal)";
const axiom = {
  index: 0,
  rule: "axiom",
  premises: [],
  file: "custom.kif",
  line: 1,
  kif: input,
};
const snapshot = () => ({
  format: "sigma-proof-report",
  version: 1,
  created: "2026-10-21",
  engine,
  prover: { backend: "native", version: engine.commit, binaries: {} },
  problem: { command: "audit", request: { seed: 1, step: 2, count: 1 } },
  config: { backend: "native", timeLimitSecs: 10 },
  constituents: [
    {
      name: "custom.kif",
      text: input,
      source: {
        repository: "github:owner/repo",
        branch: "main",
        path: "custom.kif",
      },
    },
  ],
  result: { steps: [axiom] },
  unavailable: "",
});
function fixture({
  remote = input,
  moved = false,
  compatible = true,
  preflightError = false,
} = {}) {
  const exports = {};
  const events = [];
  let heads = 0;
  const kb = {
    constituents: [],
    replaceAll: async () => {
      events.push("replace");
      kb.constituents = [];
    },
    ingest: async (name, text, origin) => {
      events.push({ origin });
      kb.constituents.push({ file: name, text });
      return { added: true };
    },
    reprocess: async () => events.push("promote"),
  };
  const imports = {
    "../api/github": {
      fetchBranchHead: async () => {
        events.push("head");
        return moved && ++heads > 1 ? "d".repeat(40) : head;
      },
    },
    "../constants": {
      AUDIT_ENGINE: compatible ? engine : null,
      PROVER_BUILDS: {},
    },
    "../utils/proofReport": utils,
    "../utils/auditReplay": replay,
    "../models/Origin": {
      GitOrigin: class {
        constructor(service, owner, repo, branch) {
          Object.assign(this, { service, owner, repo, branch });
        }
      },
    },
    "../stores/kb": { useKBStore: () => kb },
    "../stores/changes": {
      useChangesStore: () => ({
        forget: async (name) => events.push(`forget:${name}`),
      }),
    },
    "../stores/prover": {
      useProverStore: () => ({
        loadDefaults: async () => {},
        adoptConfig: () => events.push("settings"),
      }),
    },
    "./sigma": {
      call: async (cmd, _args) => {
        events.push(cmd);
        if (cmd === "validateProofSetup" && preflightError)
          throw Error("Invalid KIF");
        if (cmd === "audit")
          return {
            result: { contradictions: [{ steps: [axiom] }], batches: [] },
            reportContext: { error: "" },
          };
        return { ok: true };
      },
    },
  };
  vm.runInNewContext(code, {
    exports,
    require: (n) => imports[n],
    TextEncoder,
    fetch: async (url) => {
      events.push(url);
      return new Response(remote);
    },
  });
  return { ...exports, events, kb };
}

test("sharing compares every input byte against live HEAD and emits immutable references", async () => {
  const f = fixture();
  const exported = JSON.parse(await f.exportProofReport(snapshot()));
  assert.equal(exported.constituents[0].source.commit, head);
  assert.match(exported.constituents[0].sha256, /^[0-9a-f]{64}$/);
  assert.equal(exported.constituents[0].text, undefined);
  assert.equal(f.events.filter((e) => e === "head").length, 2);
  assert.ok(
    f.events.some(
      (e) => typeof e === "string" && e.includes(`/${head}/custom.kif`),
    ),
  );
});

test("sharing rejects unpushed edits, local files, missing branches and racing branch updates", async () => {
  await assert.rejects(
    fixture({ remote: input + "\n; unrelated change" }).exportProofReport(
      snapshot(),
    ),
    /do not match branch HEAD/,
  );
  await assert.rejects(
    fixture({ moved: true }).exportProofReport(snapshot()),
    /branch moved/,
  );
  const local = snapshot();
  local.constituents[0].source = null;
  await assert.rejects(
    fixture().exportProofReport(local),
    /pushed GitHub branch/,
  );
  const detached = snapshot();
  detached.constituents[0].source.branch = head;
  await assert.rejects(fixture().exportProofReport(detached), /detached/);
});

test("import uses pinned SHA without querying current HEAD and verifies before replacing", async () => {
  const report = JSON.parse(await fixture().exportProofReport(snapshot()));
  const f = fixture({ moved: true });
  const result = await f.importProofReport(report);
  assert.equal(result.contradictions.length, 1);
  assert.ok(!f.events.includes("head"));
  assert.ok(
    f.events.indexOf("validateProofSetup") < f.events.indexOf("replace"),
  );
  assert.ok(f.events.indexOf("promote") < f.events.indexOf("audit"));
  assert.equal(f.kb.constituents[0].text, input);
  assert.equal(f.events.find((e) => e.origin)?.origin.branch, head);
});

test("bad downloads, incompatible engines and malformed KIF preserve the current workspace", async () => {
  const report = JSON.parse(await fixture().exportProofReport(snapshot()));
  for (const options of [
    { remote: "tampered" },
    { compatible: false },
    { preflightError: true },
  ]) {
    const f = fixture(options);
    await assert.rejects(f.importProofReport(report));
    assert.ok(!f.events.includes("replace"));
  }
});
