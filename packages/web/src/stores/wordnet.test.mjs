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

function fixture({ enabled = true, fetchHook } = {}) {
  setActivePinia(createPinia());
  const downloads = [];
  const commands = [];
  const exports = {};
  const imports = {
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
        return "mapping text";
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
  });
  return { store: exports.useWordNetStore(), downloads, commands };
}

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
