import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";
import * as vue from "vue";
import { createPinia, setActivePinia, defineStore } from "pinia";

const transpile = (path) =>
  ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  }).outputText;

const options = {};
vm.runInNewContext(transpile("../utils/proverOptions.ts"), {
  exports: options,
});

function fixture(saved) {
  setActivePinia(createPinia());
  const exports = {};
  vm.runInNewContext(transpile("./prover.ts"), {
    exports,
    require: (name) =>
      ({
        pinia: { defineStore },
        vue,
        "../constants": { PROVER_SETTINGS_KEY: "prover" },
        "../utils/proverOptions": options,
        "../services/sigma": {
          call: async () => ({
            selection: {},
            presets: [
              { name: "default", strategy: { pick_ratio: 5, demod: false } },
              {
                name: "goal-directed",
                strategy: { pick_ratio: 5, goal_dist: true },
              },
            ],
          }),
        },
      })[name],
    localStorage: {
      getItem: () => JSON.stringify(saved),
      setItem() {},
    },
  });
  return exports.useProverStore();
}

test("a saved preset the engine no longer has falls back to the default", async () => {
  const store = fixture({
    profiles: {
      ask: {
        preset: "renamed-lane",
        strategy: { pick_ratio: 3, removed_knob: 1 },
      },
      audit: { preset: "goal-directed", strategy: {} },
    },
  });
  await store.loadDefaults();
  assert.equal(store.profiles.ask.preset, "default");
  assert.deepEqual(JSON.parse(JSON.stringify(store.profiles.ask.strategy)), {
    pick_ratio: 3,
  });
  assert.equal(store.profiles.audit.preset, "goal-directed");
  // The repaired profile builds a config instead of throwing.
  assert.equal(store.config("ask").strategy.pick_ratio, 3);
});
