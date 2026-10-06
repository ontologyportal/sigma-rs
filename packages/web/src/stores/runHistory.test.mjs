import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { createPinia, setActivePinia, defineStore } from "pinia";

const source = ts.transpileModule(
  readFileSync(new URL("./runHistory.ts", import.meta.url), "utf8"),
  {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  },
).outputText;

/** A fresh module over `stored` (the localStorage value, or null). */
function load(stored = null) {
  setActivePinia(createPinia());
  const storage = new Map(stored == null ? [] : [["history", stored]]);
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    require: (name) =>
      name === "pinia"
        ? { defineStore }
        : name === "../constants"
          ? { ASK_HISTORY_KEY: "history", TEST_HISTORY_KEY: "tests" }
          : {},
    localStorage: {
      getItem: (k) => storage.get(k) ?? null,
      setItem: (k, v) => storage.set(k, v),
    },
  });
  return { ...exports, storage };
}

const run = (status, query = "(p)") => ({
  lang: "kif",
  assertions: "",
  query,
  goal: query,
  told: 0,
  status,
  backend: "SUPr",
});

test("countForms counts top-level forms, skipping comments and strings", () => {
  const { countForms } = load();
  assert.equal(countForms(""), 0);
  assert.equal(countForms("(instance Rex Dog)\n(subclass Dog Mammal)"), 2);
  assert.equal(countForms("(=> (a ?X) (b ?X))"), 1);
  assert.equal(countForms('; (not a form)\n(doc X "a ( paren")'), 1);
});

test("runs are newest first, capped, and survive a reload", () => {
  const m = load();
  const h = m.useAskHistoryStore();
  for (let i = 0; i < m.MAX_ENTRIES + 5; i++)
    h.record(run("Proved", `(q${i})`));
  assert.equal(h.runs.length, m.MAX_ENTRIES);
  assert.equal(h.runs[0].query, `(q${m.MAX_ENTRIES + 4})`);
  const again = load(m.storage.get("history")).useAskHistoryStore();
  assert.equal(again.runs.length, m.MAX_ENTRIES);
  assert.equal(again.runs[0].query, h.runs[0].query);
  again.clear();
  assert.equal(again.runs.length, 0);
});

test("corrupt or foreign storage is ignored", () => {
  assert.equal(load("not json").useAskHistoryStore().runs.length, 0);
  const mixed = JSON.stringify([
    { nope: 1 },
    { ...run("Unknown"), id: "a", at: 1 },
  ]);
  assert.equal(load(mixed).useAskHistoryStore().runs.length, 1);
});

test("Ask/Tell and Inference Tests keep separate feeds", () => {
  const m = load();
  m.useRunHistory("ask").record(run("Proved"));
  m.useRunHistory("test").record({ ...run("Unknown"), title: "a.kif.tq" });
  assert.equal(m.useAskHistoryStore().runs.length, 1);
  assert.equal(m.useTestHistoryStore().runs[0].title, "a.kif.tq");
  assert.ok(m.storage.has("history") && m.storage.has("tests"));
});
