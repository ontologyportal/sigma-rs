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

function fixture() {
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
    },
    "../utils/format": {
      errMsg: (e) => e.message,
    },
    "./wordnet": { useWordNetStore: () => ({ install: async () => {} }) },
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
