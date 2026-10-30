import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import { webcrypto, createHash } from "node:crypto";
import ts from "typescript";
import { createPinia, setActivePinia, defineStore } from "pinia";

const source = ts.transpileModule(
  readFileSync(new URL("./changes.ts", import.meta.url), "utf8"),
  {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  },
).outputText;

function fixture() {
  setActivePinia(createPinia());
  const saves = [];
  const kb = {
    constituents: [
      { name: "Merge.kif", origin: { kind: "sumo" }, text: "(merge)" },
    ],
    find: (name, kind) =>
      kb.constituents.find((c) => c.name === name && c.origin.kind === kind),
    async updateConstituentText(name, text, origin) {
      saves.push(["kb", name, text, origin.kind]);
    },
  };
  const tests = {
    tests: [
      {
        name: "tests/t1.kif.tq",
        origin: { kind: "sumo" },
        text: "(query (p))",
      },
      { name: "mine.kif.tq", origin: { kind: "file" }, text: "(query (q))" },
    ],
    async saveCurrent(text, dialect, target) {
      saves.push(["tests", target.name, text, dialect, target.origin.kind]);
      return { saved: true };
    },
  };
  const WN = "WordNetMappings/WordNetMappings30-noun.txt";
  const wordnet = {
    trackedFiles: [{ name: WN, origin: { kind: "sumo" }, text: "noun text" }],
    fileText: (path) => (path === WN ? "noun text" : null),
    async saveFile(path, text) {
      saves.push(["wordnet", path, text]);
    },
  };
  const imports = {
    pinia: { defineStore },
    "./wordnet": {
      isWordNetFile: (n) => n === WN,
      useWordNetStore: () => wordnet,
    },
    "../constants": { EDITS_KEY: "edits", rawUrl: (p) => p },
    "../models/Origin": { originForKind: (kind) => ({ kind }) },
    "../api/github": {
      fetchSumoTree: async () => [
        { type: "blob", path: "alice/sumo/Merge.kif", sha: "saved" },
      ],
      fetchRepoTree: async () => [
        { type: "blob", path: "Merge.kif", sha: "fork-tip" },
      ],
    },
    "./boot": { useBootStore: () => ({}) },
    "./kb": { useKBStore: () => kb },
    "./tests": {
      isTestFile: (n) => /\.(tq|p|tptp)$/i.test(n),
      testDialect: (n) => (/\.tq$/i.test(n) ? "kif" : "tptp"),
      useTestsStore: () => tests,
    },
  };
  const index = {
    [`sumo:${WN}`]: {
      name: WN,
      origin: "sumo",
      path: WN,
      baseBlobSha: "wn-base",
      savedBlobSha: "wn-saved",
      savedAt: 1,
      proposed: null,
      prClosed: null,
    },
    "sumo:tests/t1.kif.tq": {
      name: "tests/t1.kif.tq",
      origin: "sumo",
      path: "tests/t1.kif.tq",
      baseBlobSha: "a",
      savedBlobSha: "b",
      savedAt: 1,
      proposed: null,
      prClosed: null,
    },
  };
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    crypto: webcrypto,
    TextEncoder,
    require: (name) => imports[name],
    localStorage: { getItem: () => JSON.stringify(index), setItem() {} },
  });
  return { store: exports.useChangesStore(), saves, kb, tests };
}

const plain = (v) => JSON.parse(JSON.stringify(v));

test("push completion clears uploaded edits but preserves edits saved during upload", async () => {
  const { store } = fixture();
  const row = { name: "tests/t1.kif.tq", origin: "sumo", blobSha: "b" };
  store.index["sumo:tests/t1.kif.tq"].savedBlobSha = "newer";
  await store.markPushed([row]);
  assert.equal(store.index["sumo:tests/t1.kif.tq"].savedBlobSha, "newer");
  assert.equal(store.index["sumo:tests/t1.kif.tq"].baseBlobSha, "b");
  await store.markPushed([{ ...row, blobSha: "newer" }]);
  assert.equal(store.index["sumo:tests/t1.kif.tq"], undefined);
});

test("edited upstream tests and local test files are change rows", () => {
  const { store } = fixture();
  const rows = plain(store.rows.map((r) => [r.name, r.origin, r.state]));
  assert.deepEqual(rows, [
    ["mine.kif.tq", "file", "local"],
    ["tests/t1.kif.tq", "sumo", "modified"],
    ["WordNetMappings/WordNetMappings30-noun.txt", "sumo", "modified"],
  ]);
});

test("WordNet mapping files are tracked through the WordNet store", async () => {
  const { store, saves } = fixture();
  const row = {
    name: "WordNetMappings/WordNetMappings30-noun.txt",
    origin: "sumo",
  };
  assert.equal(store.trackedText(row), "noun text");
  assert.equal(store.trackedOrigin(row).kind, "sumo");
  await store.saveTracked(row, "upstream noun");
  assert.deepEqual(plain(saves), [
    ["wordnet", "WordNetMappings/WordNetMappings30-noun.txt", "upstream noun"],
  ]);
});

test("the KB snapshot fingerprint ignores WordNet mapping edits", () => {
  const { store } = fixture();
  assert.equal(store.fingerprint, "sumo:tests/t1.kif.tq:b");
});

test("tracked text and saves go to the store that owns the file", async () => {
  const { store, saves } = fixture();
  assert.equal(
    store.trackedText({ name: "tests/t1.kif.tq", origin: "sumo" }),
    "(query (p))",
  );
  assert.equal(
    store.trackedText({ name: "Merge.kif", origin: "sumo" }),
    "(merge)",
  );
  assert.equal(store.trackedText({ name: "gone.kif.tq", origin: "sumo" }), "");
  await store.saveTracked(
    { name: "tests/t1.kif.tq", origin: "sumo" },
    "upstream",
  );
  await store.saveTracked({ name: "Merge.kif", origin: "sumo" }, "(merge2)");
  assert.deepEqual(plain(saves), [
    ["tests", "tests/t1.kif.tq", "upstream", "kif", "sumo"],
    ["kb", "Merge.kif", "(merge2)", "sumo"],
  ]);
  await assert.rejects(
    store.saveTracked({ name: "gone.kif.tq", origin: "sumo" }, "x"),
    /not imported/,
  );
});

test("fork edits are checked against their source branch before dropping matching records", async () => {
  const { store, kb } = fixture();
  const name = "alice/sumo/Merge.kif";
  const origin = {
    kind: "sumo",
    owner: "alice",
    repo: "sumo",
    branch: "work/ontology",
    id: "fork",
    isDefault: false,
    pathOf: () => "Merge.kif",
  };
  kb.constituents.push({ name, origin, text: "(fork)" });
  store.index[`sumo:${name}`] = {
    name,
    origin: "sumo",
    path: name,
    savedBlobSha: "saved",
    baseBlobSha: "old",
    proposed: null,
  };
  await store.refreshUpstreamShas({ force: true });
  assert.equal(store.upstreamShas[name], "fork-tip");
  assert.ok(store.index[`sumo:${name}`]);
});

test("pushed local files keep their branch and base for later saves without appearing unpushed", async () => {
  const { store, tests } = fixture();
  const name = "mine.kif.tq";
  const text = store.trackedText({ name, origin: "file" });
  const sha = createHash("sha1")
    .update(`blob ${Buffer.byteLength(text)}\0${text}`)
    .digest("hex");
  const target = { owner: "alice", repo: "sumo", branch: "work/ontology" };
  await store.markPushed(
    [{ name, origin: "file", path: "tests/new.kif.tq", blobSha: sha }],
    target,
  );
  assert.ok(!store.rows.some((row) => row.name === name));
  const rec = store.record(name, "file");
  assert.deepEqual(plain(rec.pushed), target);
  assert.equal(rec.path, "tests/new.kif.tq");
  tests.tests.find((t) => t.name === name).text = "(query (changed))";
  await store.recordSave(name, "file", "(query (changed))");
  assert.equal(store.rows.find((row) => row.name === name).state, "modified");
  assert.equal(store.record(name, "file").baseBlobSha, sha);
  assert.deepEqual(plain(store.record(name, "file").pushed), target);
  await store.recordSave(name, "file", text);
  assert.ok(!store.rows.some((row) => row.name === name));
});
