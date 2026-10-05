import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
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
  const imports = {
    pinia: { defineStore },
    "../constants": { EDITS_KEY: "edits", rawUrl: (p) => p },
    "../models/Origin": { originForKind: (kind) => ({ kind }) },
    "../api/github": {},
    "./boot": { useBootStore: () => ({}) },
    "./kb": { useKBStore: () => kb },
    "./tests": {
      isTestFile: (n) => /\.(tq|p|tptp)$/i.test(n),
      testDialect: (n) => (/\.tq$/i.test(n) ? "kif" : "tptp"),
      useTestsStore: () => tests,
    },
  };
  const index = {
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
    require: (name) => imports[name],
    localStorage: { getItem: () => JSON.stringify(index), setItem() {} },
  });
  return { store: exports.useChangesStore(), saves };
}

const plain = (v) => JSON.parse(JSON.stringify(v));

test("edited upstream tests and local test files are change rows", () => {
  const { store } = fixture();
  const rows = plain(store.rows.map((r) => [r.name, r.origin, r.state]));
  assert.deepEqual(rows, [
    ["mine.kif.tq", "file", "local"],
    ["tests/t1.kif.tq", "sumo", "modified"],
  ]);
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
