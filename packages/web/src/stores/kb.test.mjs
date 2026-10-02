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

function fixture({ cached = {} } = {}) {
  setActivePinia(createPinia());
  const fetched = [];
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
    "../api/github": { fetchRepoLastCommit: async () => "commit" },
    "../services/sigma": { call: async () => ({}) },
    "../services/sources": {
      fetchAllTexts: async () => [],
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
      useChangesStore: () => ({}),
    },
    "./library": { useLibraryStore: () => ({}) },
  };
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    require: (name) => imports[name],
    localStorage: {
      getItem: () => null,
      setItem: () => {},
    },
    performance,
    setTimeout,
  });
  return { store: exports.useKBStore(), fetched };
}

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
