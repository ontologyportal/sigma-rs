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

function fixture({ catalog = [], entries = [], edited = {} } = {}) {
  setActivePinia(createPinia());
  const fetched = [];
  const history = [];
  const files = new Map();
  const calls = [];
  const edits = [];
  const configs = [];
  const profiles = [];
  let chosen = "copy.kif.tq";
  class LocalOrigin {
    kind = "file";
  }
  const imports = {
    pinia: { defineStore },
    "../constants": { TQ_SETTING: "tests" },
    "../models/Origin": {
      LocalOrigin,
      RemoteOrigin: class {
        kind = "url";
      },
      originId: (o) => o.kind,
      serializeOrigin: (o) => ({ ...o }),
      parseOrigin: (o) => o,
    },
    "../services/sigma": {
      async call(method, args) {
        calls.push(method);
        if (method === "prove")
          return { result: { status: "Proved", proved: true } };
        const { text } = args;
        if (!text.trim() || text === "malformed")
          throw new Error("Invalid test");
        return { test: { queryKif: text } };
      },
    },
    "../services/sources": {
      fromOrigin: async (name) => {
        fetched.push(name);
        return name === "bad.p" ? "malformed" : `text of ${name}`;
      },
    },
    "../utils/format": { errMsg: (e) => e.message },
    "./prover": {
      useProverStore: () => ({
        async loadDefaults() {},
        config: (name, overrides) => {
          profiles.push(name);
          configs.push(overrides);
          return { ...overrides };
        },
      }),
    },
    "./runHistory": {
      countForms: (text) => (text.match(/^\(/gm) || []).length,
      useTestHistoryStore: () => ({ record: (run) => history.push(run) }),
    },
    "./changes": {
      useChangesStore: () => ({
        index: edited,
        async recordSave(name, kind, text, pristine) {
          edits.push([name, kind, text, pristine]);
        },
      }),
    },
    "./library": {
      originForRepo: () => ({ kind: "sumo", nameFor: (p) => p }),
      useLibraryStore: () => ({
        entries,
        repos: [{}],
        catalogs: { repo: catalog },
        repoId: () => "repo",
        async loadCatalogs() {},
        async writeLocal(name, text) {
          files.set(name, text);
        },
        ensureEntry() {},
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
    fetched,
    history,
    files,
    calls,
    edits,
    configs,
    profiles,
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

test("remote tests save in place as a local edit, never to the library", async () => {
  const f = fixture();
  const origin = { kind: "url", url: "https://example.org/a.kif.tq" };
  await f.store.add("a.kif.tq", "old", origin);
  f.choose(null); // a prompt would cancel the save
  const r = await f.store.saveCurrent("new", "kif", {
    name: "a.kif.tq",
    origin,
  });
  assert.equal(r.saved, true);
  assert.equal(r.overwritten, true);
  assert.equal(f.store.find("a.kif.tq").text, "new");
  assert.equal(f.store.find("a.kif.tq").origin.kind, "url");
  assert.deepEqual(f.edits, [["a.kif.tq", "url", "new", "old"]]);
  assert.equal(f.files.size, 0);
  await assert.rejects(
    f.store.saveCurrent("malformed", "kif", { name: "a.kif.tq", origin }),
    /Invalid test/,
  );
  assert.equal(f.store.find("a.kif.tq").text, "new");
  assert.equal(f.edits.length, 1);
});

test("a remote test of the other dialect still saves as a new local file", async () => {
  const f = fixture();
  const origin = { kind: "sumo" };
  await f.store.add("t.kif.tq", "old", origin);
  f.choose("t.p");
  await f.store.saveCurrent("problem", "tptp", { name: "t.kif.tq", origin });
  assert.equal(f.store.find("t.p").origin.kind, "file");
  assert.equal(f.edits.length, 0);
});

test("a test overrides the time limit only when it has a (time) directive", async () => {
  const f = fixture();
  const run = (parsed) =>
    f.store.run({
      name: "t.kif.tq",
      parsed: { queryKif: "(p)", axiomKif: "", ...parsed },
    });
  await run({ timeout: 30, timeGiven: false });
  await run({ timeout: 12, timeGiven: true });
  assert.deepEqual(JSON.parse(JSON.stringify(f.configs)), [
    {},
    { timeLimitSecs: 12 },
  ]);
  assert.deepEqual([...f.profiles], ["test", "test"]);
});

test("runAll flags the store as running only while it runs", async () => {
  const f = fixture();
  await f.store.add("a.kif.tq", "q", f.local);
  const seen = [];
  await f.store.runAll(() => seen.push(f.store.running));
  assert.deepEqual(seen, [true]);
  assert.equal(f.store.running, false);
});

test("the library is listed at load, but no repo test is fetched", async () => {
  const f = fixture({
    catalog: [
      { path: "a.kif.tq", size: 1 },
      { path: "b.p", size: 1 },
      { path: "Merge.kif", size: 1 },
    ],
  });
  await f.store.load();
  assert.equal(f.store.loaded, true);
  assert.equal(f.store.tests.length, 0);
  assert.deepEqual(
    [...f.store.available.map((t) => t.name)],
    ["a.kif.tq", "b.p"],
  );
  assert.equal(f.fetched.length, 0);
});

test("load fetches local test files and tests with a tracked edit", async () => {
  const f = fixture({
    catalog: [
      { path: "edited.kif.tq", size: 1 },
      { path: "plain.kif.tq", size: 1 },
    ],
    entries: [{ kind: "file", name: "mine.kif.tq", size: 1 }],
    edited: { x: { name: "edited.kif.tq", origin: "sumo" } },
  });
  let settled = false;
  const waiting = f.store.whenLoaded().then(() => {
    settled = true;
  });
  await f.store.load();
  await waiting;
  assert.equal(settled, true);
  assert.deepEqual([...f.fetched].sort(), ["edited.kif.tq", "mine.kif.tq"]);
});

test("ensure fetches a test once; a broken one is recorded, not refetched", async () => {
  const f = fixture({
    catalog: [
      { path: "good.kif.tq", size: 1 },
      { path: "bad.p", size: 1 },
    ],
  });
  const t = await f.store.ensure("good.kif.tq");
  assert.equal(t.text, "text of good.kif.tq");
  await f.store.ensure("good.kif.tq");
  await assert.rejects(f.store.ensure("bad.p"), /Invalid test/);
  await assert.rejects(f.store.ensure("bad.p"), /Invalid test/);
  assert.deepEqual([...f.fetched], ["good.kif.tq", "bad.p"]);
  assert.equal(f.store.failed.length, 1);
  await assert.rejects(f.store.ensure("missing.kif.tq"), /No test named/);
});

test("runAll loads the tests it is given, and counts a broken one as run", async () => {
  const f = fixture({
    catalog: [
      { path: "good.kif.tq", size: 1 },
      { path: "bad.p", size: 1 },
    ],
  });
  const refs = f.store.available.map((t) => ({
    name: t.name,
    origin: t.origin,
  }));
  const r = await f.store.runAll(undefined, refs);
  assert.equal(r.ran, 2);
  assert.ok(f.store.find("good.kif.tq").outcome); // proved, then graded
  assert.equal(f.store.failed[0].name, "bad.p");
});

test("every test a bulk run touches lands in the history, broken ones too", async () => {
  const f = fixture({
    catalog: [
      { path: "good.kif.tq", size: 1 },
      { path: "bad.p", size: 1 },
    ],
  });
  const refs = f.store.available.map((t) => ({
    name: t.name,
    origin: t.origin,
  }));
  await f.store.runAll(undefined, refs);
  assert.deepEqual(
    f.history.map((r) => [r.title, r.status]),
    [
      ["good.kif.tq", "Proved"],
      ["bad.p", "Error"],
    ],
  );
});
