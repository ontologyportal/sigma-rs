import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import { parse, compileScript } from "@vue/compiler-sfc";
import * as vue from "vue";
import ts from "typescript";

const transpile = (text) =>
  ts.transpileModule(text, {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  }).outputText;
const meta = {};
vm.runInNewContext(
  transpile(
    readFileSync(new URL("../utils/testMeta.ts", import.meta.url), "utf8"),
  ),
  { exports: meta },
);
const { descriptor } = parse(
  readFileSync(new URL("./AskTellTab.vue", import.meta.url), "utf8"),
);
const source = transpile(compileScript(descriptor, { id: "ask-test" }).content);

function fixture() {
  const entries = vue.reactive(new Map());
  const tests = vue.reactive({
    openTest: null,
    dialect: (name) => (name.endsWith(".tq") ? "kif" : "tptp"),
    find: (name) => entries.get(name),
    setOpen(t) {
      this.openTest = t ? { name: t.name, origin: t.origin } : null;
    },
    async saveCurrent(text) {
      entries.get(this.openTest.name).text = text;
      return { saved: true, name: this.openTest.name, overwritten: true };
    },
  });
  const prover = vue.reactive({
    proofLang: "kif",
    profiles: { ask: { timeLimitSecs: 30 } },
  });
  const status = {
    set() {},
    clear() {},
    fail(e) {
      throw e;
    },
  };
  const imports = {
    vue: { ...vue, onActivated() {} },
    "sigmakee/sdk": { formatTest: (options) => JSON.stringify(options) },
    "../stores/tests": { useTestsStore: () => tests },
    "../stores/prover": {
      useProverStore: () => prover,
      profileDefaults: () => ({ timeLimitSecs: 30 }),
    },
    "../stores/shell": { useShellStore: () => ({ isCompact: false }) },
    "../stores/runHistory": { useAskHistoryStore: () => ({}) },
    "../composables/useStatus": { useStatus: () => status },
    "../composables/useElapsed": { useElapsed: () => ({}) },
    "../composables/useScratchValidation": {
      useScratchValidation: () => ({ paneErrors: async () => ({}) }),
    },
    "../composables/useTabQuery": {
      useTabQuery: () => ({ onQuery() {}, query: vue.ref({}) }),
    },
    "../utils/testMeta": meta,
  };
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    require: (name) => imports[name] ?? {},
    navigator: { platform: "Win32" },
  });
  const scope = vue.effectScope();
  const view = scope.run(() => exports.default.setup({}, { expose() {} }));
  const entry = (name, text, parsed = {}) => ({
    name,
    text,
    origin: { kind: "file" },
    parsed: { axiomKif: "(old axiom)", queryKif: "(old query)", ...parsed },
  });
  return { view, tests, entries, prover, entry, scope };
}

test("raw saves update AskTell even when the save finishes after activation", async () => {
  const f = fixture();
  try {
    const original = f.entry("test.kif.tq", "original");
    f.entries.set(original.name, original);
    f.view.openTest(f.entries.get(original.name));
    await vue.nextTick();
    f.entries.set(
      original.name,
      f.entry(original.name, "saved raw", {
        axiomKif: "(new axiom)",
        queryKif: "(new query)",
        noteGiven: true,
        note: "updated note",
        categories: ["updated category"],
        expectedProof: false,
        minLang: "thf",
        timeGiven: true,
        timeout: 12,
      }),
    );
    await vue.nextTick();
    assert.equal(f.view.assertions.value, "(new axiom)");
    assert.equal(f.view.query.value, "(new query)");
    assert.equal(f.view.meta.value.note, "updated note");
    assert.equal(f.view.meta.value.categories, "updated category");
    assert.equal(f.view.meta.value.answer, "no");
    assert.equal(f.view.meta.value.minLang, "thf");
    assert.equal(f.prover.profiles.ask.timeLimitSecs, 12);
    f.entries.get(original.name).text = "without time";
    f.entries.get(original.name).parsed.timeGiven = false;
    await vue.nextTick();
    assert.equal(f.prover.profiles.ask.timeLimitSecs, 30);
  } finally {
    f.scope.stop();
  }
});

test("TPTP raw saves refresh the whole problem in AskTell", async () => {
  const f = fixture();
  try {
    const original = f.entry("test.p", "fof(q, conjecture, old).");
    f.entries.set(original.name, original);
    f.view.openTest(f.entries.get(original.name));
    await vue.nextTick();
    f.entries.get(original.name).text = "fof(q, conjecture, updated).";
    await vue.nextTick();
    assert.equal(f.prover.proofLang, "tptp");
    assert.equal(f.view.assertions.value, "fof(q, conjecture, updated).");
    assert.equal(f.view.query.value, "");
  } finally {
    f.scope.stop();
  }
});

test("saving another raw file does not replace the test being edited in AskTell", async () => {
  const f = fixture();
  try {
    for (const name of ["a.kif.tq", "b.kif.tq"])
      f.entries.set(name, f.entry(name, name));
    f.view.openTest(f.entries.get("b.kif.tq"));
    await vue.nextTick();
    f.view.query.value = "(draft query)";
    f.entries.get("a.kif.tq").text = "saved a";
    await vue.nextTick();
    assert.equal(f.tests.openTest.name, "b.kif.tq");
    assert.equal(f.view.query.value, "(draft query)");
  } finally {
    f.scope.stop();
  }
});

test("AskTell's own save does not reload its panes or discard edits typed during saving", async () => {
  const f = fixture();
  let finish;
  try {
    const original = f.entry("test.kif.tq", "original");
    f.entries.set(original.name, original);
    f.view.openTest(f.entries.get(original.name));
    await vue.nextTick();
    f.tests.saveCurrent = async (text) => {
      await new Promise((resolve) => {
        finish = resolve;
      });
      f.entries.get(original.name).text = text;
      return { saved: true, name: original.name, overwritten: true };
    };
    const saving = f.view.saveTest();
    f.view.query.value = "(typed during save)";
    finish();
    await saving;
    await vue.nextTick();
    assert.equal(f.view.query.value, "(typed during save)");
  } finally {
    f.scope.stop();
  }
});
