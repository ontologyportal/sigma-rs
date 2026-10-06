import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createHash, webcrypto } from "node:crypto";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";

function load(path, imports = {}, globals = {}) {
  const exports = {};
  vm.runInNewContext(
    ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    }).outputText,
    {
      exports,
      require: (name) => imports[name],
      crypto: webcrypto,
      TextEncoder,
      TextDecoder,
      ...globals,
    },
  );
  return exports;
}
const utils = load("../utils/auditReplay.ts");
const commit = "a".repeat(40);
const engine = { commit: "b".repeat(40), fingerprint: "c".repeat(64) };

function fixture({
  stale = false,
  advance = false,
  corrupt = false,
  manifest = "Merge.kif\n",
  failFetch = false,
} = {}) {
  const text = "(instance A B)\n";
  const bytes = new TextEncoder().encode(text);
  const hash = createHash("sha256").update(bytes).digest();
  const replay = {
    version: 1,
    complete: true,
    sumo_commit: commit,
    run_id: "12",
    run_attempt: 1,
    engine,
    fingerprint: createHash("sha256")
      .update("Merge.kif\0")
      .update(hash)
      .digest("hex"),
    constituents: [{ name: "Merge.kif", sha256: hash.toString("hex") }],
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
  const json = JSON.stringify(replay, null, 2);
  const events = [];
  const constituents = [];
  let checks = 0;
  const service = load(
    "./audit-replay.ts",
    {
      "../api/github": {
        fetchAuditMasterSha: async () =>
          stale || (advance && ++checks > 1) ? "d".repeat(40) : commit,
        fetchLatestMasterAudit: async () => ({
          id: 12,
          run_attempt: 1,
          head_sha: commit,
          html_url: "https://github.com/run/12",
        }),
      },
      "../constants": {
        AUDIT_ENGINE: engine,
        SUMO: { owner: "ontologyportal", repo: "sumo" },
      },
      "../models/Origin": {
        GitOrigin: class {
          constructor(service, owner, repo, branch) {
            Object.assign(this, { service, owner, repo, branch });
          }
        },
      },
      "../stores/kb": {
        useKBStore: () => ({
          constituents,
          replaceAll: async () => {
            events.push("replace");
          },
          ingest: async (name, content, origin) => {
            events.push({ name, content, ref: origin.branch });
            constituents.push({ file: name, text: content });
            return { added: true };
          },
          reprocess: async () => {
            events.push("promote");
          },
        }),
      },
      "../stores/changes": {
        useChangesStore: () => ({
          forget: async (name) => {
            events.push(`forget:${name}`);
          },
        }),
      },
      "../utils/auditReplay": utils,
    },
    {
      fetch: async (url) => {
        events.push(url);
        if (url.endsWith("latest-contradictions.json"))
          return new Response(json);
        if (url.endsWith("full-sumo.txt")) return new Response(manifest);
        return new Response(corrupt ? "local edits" : bytes, {
          status: failFetch ? 404 : 200,
        });
      },
    },
  );
  return { service, replay, events, text };
}

test("viewing a report is read-only, including when stale", async () => {
  const f = fixture({ stale: true });
  const result = await f.service.latestAuditReport();
  assert.match(result.unavailable, /master has changed/);
  assert.deepEqual(JSON.parse(result.json), f.replay);
  assert.ok(!f.events.includes("replace"));
});

test("confirmed replay loads only pinned, verified files and discards conflicting edits", async () => {
  const f = fixture();
  await f.service.loadAuditReplay(f.replay, () => {});
  assert.equal(f.events.filter((e) => e === "replace").length, 1);
  assert.ok(
    f.events.indexOf("replace") >
      f.events.findIndex(
        (e) => typeof e === "string" && e.endsWith("full-sumo.txt"),
      ),
  );
  assert.ok(f.events.includes("forget:Merge.kif"));
  assert.deepEqual(
    JSON.parse(JSON.stringify(f.events.find((e) => typeof e === "object"))),
    { name: "Merge.kif", content: f.text, ref: commit },
  );
  assert.equal(f.events.at(-1), "promote");
});

for (const [name, options, reason] of [
  ["stale master", { stale: true }, /master has changed/],
  ["master changes during download", { advance: true }, /master has changed/],
  ["corrupt constituent", { corrupt: true }, /file mismatch/],
  ["manifest differs", { manifest: "Animals.kif\n" }, /constituent list/],
  ["constituent fetch fails", { failFetch: true }, /HTTP 404/],
])
  test(`${name} cannot overwrite work`, async () => {
    const f = fixture(options);
    await assert.rejects(
      f.service.loadAuditReplay(f.replay, () => {}),
      reason,
    );
    assert.ok(!f.events.includes("replace"));
    assert.ok(
      !f.events.some((e) => typeof e === "string" && e.startsWith("forget:")),
    );
  });
