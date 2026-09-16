import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { createPinia, setActivePinia, defineStore } from "pinia";

const source = ts.transpileModule(
  readFileSync(new URL("./tests.ts", import.meta.url), "utf8"),
  {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  },
).outputText;

function fixture() {
  setActivePinia(createPinia());
  const files = new Map();
  const calls = [];
  let chosen = "copy.kif.tq";
  class LocalOrigin {
    kind = "file";
  }
  const imports = {
    pinia: { defineStore },
    "../constants": { TQ_SETTING: "tests" },
    "../models/Origin": {
      LocalOrigin,
      originId: (o) => o.kind,
      serializeOrigin: (o) => ({ ...o }),
      parseOrigin: (o) => o,
    },
    "../services/sigma": {
      async call(method, { text }) {
        calls.push(method);
        if (!text.trim() || text === "malformed")
          throw new Error("Invalid test");
        return { test: { queryKif: text } };
      },
    },
    "../services/sources": {},
    "../utils/format": {},
    "./prover": {},
    "./library": {
      useLibraryStore: () => ({
        async writeLocal(name, text) {
          files.set(name, text);
        },
      }),
    },
  };
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    require: (name) => imports[name],
    localStorage: { getItem: () => null, setItem() {} },
    window: { prompt: () => chosen },
  });
  const store = exports.useTestsStore();
  return {
    store,
    files,
    calls,
    local: new LocalOrigin(),
    choose: (s) => {
      chosen = s;
    },
  };
}

test("raw save preserves full text, reparses and clears the previous outcome", async () => {
  const f = fixture();
  await f.store.add("a.kif.tq", "old", f.local);
  f.store.find("a.kif.tq").outcome = { label: "pass", cls: "ok" };
  const raw =
    "; comment\n(time 12)\n(query (instance Rex Dog))\n(answer yes)\n";
  await f.store.saveCurrent(raw, "kif", { name: "a.kif.tq", origin: f.local });
  assert.equal(f.files.get("a.kif.tq"), raw);
  assert.equal(f.store.find("a.kif.tq").parsed.queryKif, raw);
  assert.equal(f.store.find("a.kif.tq").outcome, null);
});

for (const raw of ["", "malformed"])
  test(`invalid raw text ${JSON.stringify(raw)} does not overwrite storage`, async () => {
    const f = fixture();
    await f.store.add("a.kif.tq", "old", f.local);
    f.files.set("a.kif.tq", "old");
    await assert.rejects(
      f.store.saveCurrent(raw, "kif", { name: "a.kif.tq", origin: f.local }),
      /Invalid test/,
    );
    assert.equal(f.files.get("a.kif.tq"), "old");
    assert.equal(f.store.find("a.kif.tq").text, "old");
  });

test("explicit raw target does not overwrite the different test open in AskTell", async () => {
  const f = fixture();
  await f.store.add("a.kif.tq", "a", f.local);
  await f.store.add("b.kif.tq", "b", f.local);
  f.store.setOpen(f.store.find("b.kif.tq"));
  await f.store.saveCurrent("edited a", "kif", {
    name: "a.kif.tq",
    origin: f.local,
  });
  assert.equal(f.store.find("a.kif.tq").text, "edited a");
  assert.equal(f.store.find("b.kif.tq").text, "b");
});

test("AskTell still saves its current test without an explicit target", async () => {
  const f = fixture();
  await f.store.add("a.kif.tq", "old", f.local);
  f.store.setOpen(f.store.find("a.kif.tq"));
  await f.store.saveCurrent("new", "kif");
  assert.equal(f.store.find("a.kif.tq").text, "new");
});

test("TPTP raw saves use the TPTP parser", async () => {
  const f = fixture();
  await f.store.add("a.p", "old", f.local);
  await f.store.saveCurrent("fof(q, conjecture, p).", "tptp", {
    name: "a.p",
    origin: f.local,
  });
  assert.equal(f.calls.at(-1), "parseTptpTest");
  assert.equal(f.files.get("a.p"), "fof(q, conjecture, p).");
});

test("remote tests save as local copies and cancellation preserves the original", async () => {
  const f = fixture();
  const origin = { kind: "url", url: "https://example.org/a.kif.tq" };
  await f.store.add("a.kif.tq", "old", origin);
  const target = { name: "a.kif.tq", origin };
  f.choose(null);
  assert.equal((await f.store.saveCurrent("new", "kif", target)).saved, false);
  assert.equal(f.files.size, 0);
  f.choose("a.kif.tq");
  await assert.rejects(
    f.store.saveCurrent("new", "kif", target),
    /another source/,
  );
  assert.equal(f.files.size, 0);
  f.choose("copy.kif.tq");
  await f.store.saveCurrent("new", "kif", target);
  assert.equal(f.store.find("a.kif.tq").text, "old");
  assert.equal(f.store.find("copy.kif.tq").origin.kind, "file");
});
