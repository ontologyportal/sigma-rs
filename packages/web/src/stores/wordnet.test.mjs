import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { createPinia, setActivePinia, defineStore } from "pinia";
import { toRaw } from "vue";

const source = ts.transpileModule(
  readFileSync(new URL("./wordnet.ts", import.meta.url), "utf8"),
  {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  },
).outputText;

const helpers = {};
vm.runInNewContext(
  ts.transpileModule(
    readFileSync(
      new URL("../utils/wordnetMappings.ts", import.meta.url),
      "utf8",
    ),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    },
  ).outputText,
  { exports: helpers },
);

const NOUN = [
  ";; header",
  "02084071 05 n 01 dog 0 000 | a domesticated canine &%Canine+",
  "03100490 06 n 01 car 0 000 | a motor vehicle &%Automobile=",
].join("\n");

function fixture({ enabled = true, fetchHook, edits = {} } = {}) {
  setActivePinia(createPinia());
  const downloads = [];
  const commands = [];
  const recorded = [];
  const timers = [];
  const exports = {};
  const imports = {
    "../models/Origin": {
      GitOrigin: { default: () => ({ kind: "sumo" }) },
    },
    "./changes": {
      useChangesStore: () => ({
        readEdit: async (name) => edits[name] ?? null,
        recordSave: async (name, origin, text, pristine) => {
          recorded.push({ name, origin, text, pristine });
        },
        fetchUpstreamText: async (path) => `upstream ${path}`,
      }),
    },
    "../utils/wordnetMappings": helpers,
    pinia: { defineStore },
    vue: { toRaw },
    "../services/sigma": {
      call: async (cmd) => {
        commands.push(cmd);
        return {};
      },
    },
    "../services/sources": {
      fetchText: async (url) => {
        downloads.push(url);
        await fetchHook?.(url);
        return url.endsWith("-noun.txt") ? NOUN : "mapping text";
      },
    },
    "../constants": {
      rawUrl: (path) => path,
      WORDNET_DIR: "WordNetMappings",
      WORDNET_ENABLED_KEY: "wordnet",
    },
  };
  vm.runInNewContext(source, {
    exports,
    require: (name) => imports[name],
    TextEncoder,
    localStorage: { getItem: () => String(enabled), setItem() {} },
    console: { warn() {} },
    setTimeout: (fn) => timers.push(fn),
    clearTimeout: () => {},
  });
  const flushTimers = () => timers.splice(0).forEach((fn) => fn());
  return {
    store: exports.useWordNetStore(),
    downloads,
    commands,
    recorded,
    flushTimers,
  };
}

const NOUN_PATH = "WordNetMappings/WordNetMappings30-noun.txt";

test("a saved mapping edit outranks the upstream download", async () => {
  const { store, downloads } = fixture({ edits: { [NOUN_PATH]: "edited" } });
  await store.fetchPayload();
  assert.equal(store.fileText(NOUN_PATH), "edited");
  assert.ok(!downloads.includes(NOUN_PATH));
  assert.equal(store.pristine["WordNetMappings30-noun.txt"], undefined);
  assert.equal(await store.baseText(NOUN_PATH), `upstream ${NOUN_PATH}`);
  assert.equal(store.trackedFiles.length, 4);
  assert.equal(store.trackedFiles[0].name, NOUN_PATH);
});

test("setMappings rewrites one record, records the save, and reloads the lexicon", async () => {
  const { store, commands, recorded, flushTimers } = fixture();
  await store.install();
  await store.setMappings("WordNetMappings30-noun.txt", 2, "02084071-n", [
    { term: "DomesticDog", suffix: "=" },
    { term: "Pet", suffix: "+" },
  ]);
  const text = store.fileText(NOUN_PATH);
  assert.equal(
    text.split("\n")[1],
    "02084071 05 n 01 dog 0 000 | a domesticated canine &%DomesticDog= &%Pet+",
  );
  assert.equal(text.split("\n")[2], NOUN.split("\n")[2]);
  assert.equal(
    store.lineAt("WordNetMappings30-noun.txt", 2),
    text.split("\n")[1],
  );
  assert.equal(recorded.length, 1);
  assert.equal(recorded[0].name, NOUN_PATH);
  assert.equal(recorded[0].text, text);
  assert.equal(recorded[0].pristine, NOUN);
  assert.equal(store.revision, 1);

  assert.deepEqual(commands, ["loadWordNet"]);
  flushTimers();
  assert.deepEqual(commands, ["loadWordNet", "loadWordNet"]);
});

test("setMappings refuses a line that no longer holds the synset", async () => {
  const { store, recorded } = fixture();
  await store.install();
  await assert.rejects(
    store.setMappings("WordNetMappings30-noun.txt", 3, "02084071-n", []),
    /changed since 02084071-n/,
  );
  await assert.rejects(
    store.setMappings("WordNetMappings30-noun.txt", 2, "02084071-n", [
      { term: "Not a term", suffix: "=" },
    ]),
    /Invalid SUMO term/,
  );
  assert.equal(recorded.length, 0);
  assert.equal(store.fileText(NOUN_PATH), NOUN);
});

test("startup and unused session replacement never download WordNet", async () => {
  const { store, downloads, commands } = fixture();
  assert.equal(store.installed, false);
  await store.reinstall();
  assert.deepEqual(downloads, []);
  assert.deepEqual(commands, []);
});

test("first use shares one download and install; later use and recovery reuse the payload", async () => {
  const { store, downloads, commands } = fixture();
  await Promise.all([store.install(), store.install()]);
  assert.equal(downloads.length, 7);
  assert.deepEqual(commands, ["loadWordNet"]);
  assert.equal(store.installed, true);
  assert.equal(store.loading, false);
  await store.install();
  assert.equal(commands.length, 1);
  await store.reinstall();
  assert.equal(downloads.length, 7);
  assert.deepEqual(commands, ["loadWordNet", "loadWordNet"]);
});

test("disabled WordNet stays unloaded until explicitly enabled", async () => {
  const { store, downloads, commands } = fixture({ enabled: false });
  await store.install();
  assert.deepEqual(downloads, []);
  store.setEnabled(true);
  await store.install();
  store.setEnabled(false);
  await store.clear();
  assert.equal(store.installed, false);
  await store.reinstall();
  assert.deepEqual(commands, ["loadWordNet", "clearWordNet"]);
  assert.equal(downloads.length, 7);
});

test("failed first use can be retried without leaving loading stuck", async () => {
  let fail = true;
  const { store, commands } = fixture({
    fetchHook() {
      if (fail) throw new Error("disconnected");
    },
  });
  await store.install();
  assert.equal(store.installed, false);
  assert.equal(store.loading, false);
  assert.deepEqual(commands, []);
  fail = false;
  await store.install();
  assert.equal(store.installed, true);
});

test("disabling WordNet during download does not install it afterwards", async () => {
  let release;
  const gate = new Promise((resolve) => {
    release = resolve;
  });
  const { store, commands } = fixture({ fetchHook: () => gate });
  const install = store.install();
  store.setEnabled(false);
  const clear = store.clear();
  release();
  await Promise.all([install, clear]);
  assert.equal(store.installed, false);
  assert.equal(store.loading, false);
  assert.deepEqual(commands, ["clearWordNet"]);
});
