import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { createPinia, setActivePinia, defineStore } from "pinia";

const source = ts.transpileModule(
  readFileSync(new URL("./kb.ts", import.meta.url), "utf8"),
  {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  },
).outputText;

function fixture({ cached = {}, downloads = [], sha = "commit" } = {}) {
  setActivePinia(createPinia());
  const fetched = [];
  const events = [];
  const imports = {
    pinia: { defineStore },
    "../constants": {
      MERGE: "Merge.kif",
      SUMO_FILE_SETTING: "sumo-files",
      UPDATE_BASELINES_KEY: "baselines",
      UPDATE_PREFS_KEY: "preferences",
    },
    "../models/Constituent": {
      Constituent: class {
        static defaults() {
          return [];
        }
      },
    },
    "../models/Origin": {
      GitOrigin: { default: () => ({ kind: "sumo" }) },
      isPersistedRow: () => true,
      originId: () => "origin",
      parseOrigin: (json) => json,
      serializeOrigin: (origin) => origin,
      sourceLabel: () => "source",
    },
    "../api/github": {
      fetchRepoLastCommit: async (...args) => {
        events.push(["head", ...args]);
        return { sha };
      },
    },
    "../services/sigma": { call: async () => ({}) },
    "../services/sources": {
      fetchAllTexts: async (entries, limit, progress, pin, upstream) => {
        events.push(["fetch", entries, pin(entries[0].origin), upstream]);
        progress(entries.length);
        return downloads;
      },
      fetchText: async () => "",
      fromOrigin: async (name) => {
        fetched.push(name);
        if (name === "Missing.kif") throw new Error("HTTP 404");
        return `(instance ${name} Entity)`;
      },
    },
    "../services/lsp": {
      lspReset: () => {},
      lspSyncDocument: async () => {},
    },
    "../services/kb-cache": {
      scheduleSave: () => {},
      flushSave: async () => {},
      readCachedText: async (name) => cached[name] ?? null,
    },
    "../utils/format": {
      errMsg: (e) => e.message,
    },
    "./wordnet": { useWordNetStore: () => ({ reinstall: async () => {} }) },
    "./changes": {
      blobSha: async () => "sha",
      useChangesStore: () => ({
        forget: async (name) => events.push(["forget", name]),
      }),
    },
    "./library": { useLibraryStore: () => ({ addRepo: async () => {} }) },
  };
  const exports = {};
  vm.runInNewContext(source, {
    Error,
    exports,
    require: (name) => imports[name],
    localStorage: {
      getItem: () => null,
      setItem: () => {},
    },
    performance,
    setTimeout,
  });
  return { store: exports.useKBStore(), fetched, events };
}

const branchOrigin = {
  kind: "sumo",
  owner: "alice",
  repo: "sumo",
  branch: "work/ontology",
  label: "alice/sumo@work/ontology",
  nameFor: (path) => `alice/sumo/${path}`,
};

test("branch replacement downloads one fresh commit before replacing and ignores old edits", async () => {
  const { store, events } = fixture({ downloads: ["(new)"] });
  store.updateBaselines.origin = "old";
  store.replaceAll = async () => events.push(["replace"]);
  store.applyChanges = async ({ add }) => {
    events.push(["apply", add]);
    return { added: add.length, failed: [] };
  };
  const result = await store.loadBranch(branchOrigin, ["Merge.kif"]);
  assert.equal(result.added, 1);
  assert.deepEqual(
    events.map((e) => e[0]),
    ["head", "fetch", "replace", "forget", "apply"],
  );
  assert.equal(events[0][3], "work/ontology");
  assert.equal(events[0][4].force, true);
  assert.equal(events[1][2], "commit");
  assert.equal(events[1][3], true);
  assert.equal(events[4][1][0].name, "alice/sumo/Merge.kif");
  assert.equal(events[4][1][0].text, "(new)");
  assert.equal(store.updateBaselines.origin, "commit");
});

test("a failed branch download preserves the existing KB and edits", async () => {
  const { store, events } = fixture({ downloads: [new Error("HTTP 404")] });
  store.replaceAll = async () => assert.fail("must not replace");
  await assert.rejects(store.loadBranch(branchOrigin, ["Missing.kif"]), /404/);
  assert.deepEqual(
    events.map((e) => e[0]),
    ["head", "fetch"],
  );
});

test("an empty selection or missing branch never replaces the KB", async () => {
  const { store, events } = fixture({ sha: null });
  store.replaceAll = async () => assert.fail("must not replace");
  await assert.rejects(store.loadBranch(branchOrigin, []), /constituent set/);
  assert.equal(events.length, 0);
  await assert.rejects(
    store.loadBranch(branchOrigin, ["Merge.kif"]),
    /Cannot resolve/,
  );
  assert.deepEqual(
    events.map((e) => e[0]),
    ["head"],
  );
});

test("saved constituent failures do not stop the remaining boot load", async () => {
  const { store, fetched } = fixture();
  store.saved = [
    { name: "First.kif", origin: { kind: "sumo" } },
    { name: "Missing.kif", origin: { kind: "sumo" } },
    { name: "Last.kif", origin: { kind: "sumo" } },
  ];
  const ingested = [];
  store.ingest = async (name) => {
    ingested.push(name);
    return { added: true, notices: [] };
  };

  const failed = await store.loadSavedConstituents();

  assert.deepEqual(fetched, ["First.kif", "Missing.kif", "Last.kif"]);
  assert.deepEqual(ingested, ["First.kif", "Last.kif"]);
  assert.deepEqual(Array.from(failed), ["Missing.kif: HTTP 404"]);
});

test("an unreachable file falls back to its cached copy, flagged stale", async () => {
  const { store } = fixture({ cached: { "Missing.kif": "(cached)" } });
  store.saved = [
    { name: "First.kif", origin: { kind: "sumo" } },
    { name: "Missing.kif", origin: { kind: "sumo" } },
  ];
  const ingested = [];
  store.ingest = async (name, text, origin, stale) => {
    ingested.push([name, text, stale]);
    return { added: true, notices: [] };
  };

  const failed = await store.loadSavedConstituents();

  assert.deepEqual(Array.from(failed), []);
  assert.deepEqual(JSON.parse(JSON.stringify(ingested)), [
    ["First.kif", "(instance First.kif Entity)", null],
    ["Missing.kif", "(cached)", "HTTP 404"],
  ]);
  assert.equal(store.unavailable.length, 0);
});

test("a file with no cached copy is unavailable and reported", async () => {
  const { store } = fixture();
  store.saved = [{ name: "Missing.kif", origin: { kind: "sumo" } }];
  store.ingest = async () => ({ added: true, notices: [] });
  const failed = await store.loadSavedConstituents();
  assert.deepEqual(Array.from(failed), ["Missing.kif: HTTP 404"]);
  assert.deepEqual(
    JSON.parse(
      JSON.stringify(store.unavailable.map((u) => [u.name, u.reason])),
    ),
    [["Missing.kif", "HTTP 404"]],
  );
});

test("replacing the KB clears unavailable files and alerts from the previous branch", async () => {
  const { store } = fixture();
  store.unavailable = [
    { name: "Old.kif", origin: { kind: "sumo" }, reason: "404" },
  ];
  store.updateAlerts = [{ key: "old", message: "Old branch changed" }];
  store.changedOnLoad = [{ key: "old", message: "Old branch changed" }];
  await store.replaceAll();
  assert.equal(store.unavailable.length, 0);
  assert.equal(store.updateAlerts.length, 0);
  assert.equal(store.changedOnLoad.length, 0);
});
