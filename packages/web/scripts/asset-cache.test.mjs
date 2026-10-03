import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtemp, mkdir, readFile, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import vm from "node:vm";
import assetCache, { isFeatureAsset } from "./asset-cache.mjs";

test("feature manifest includes workers and public provers, excludes server data", async () => {
  const root = await mkdtemp(join(tmpdir(), "sigma-assets-"));
  try {
    await mkdir(join(root, "dist", "assets"), { recursive: true });
    await mkdir(join(root, "dist", "eprover"));
    for (const file of [
      "assets/tab.js",
      "assets/editor.worker.js",
      "assets/font.ttf",
      "eprover/runner.mjs",
      "eprover/eprover.wasm",
      "index.html",
      "version.json",
      "_headers",
    ])
      await writeFile(join(root, "dist", file), file);
    const plugin = assetCache();
    plugin.configResolved({
      root,
      build: { outDir: "dist" },
      base: "/browse/",
    });
    await plugin.closeBundle();
    const first = await readFile(
      join(root, "dist", "asset-cache-sw.js"),
      "utf8",
    );
    assert.match(first, /\/browse\/assets\/editor.worker.js/);
    assert.match(first, /\/browse\/eprover\/eprover.wasm/);
    assert.doesNotMatch(first, /version.json|index.html|_headers/);
    await plugin.closeBundle();
    assert.equal(
      await readFile(join(root, "dist", "asset-cache-sw.js"), "utf8"),
      first,
    );
    await writeFile(
      join(root, "dist", "eprover", "eprover.wasm"),
      "new binary",
    );
    await plugin.closeBundle();
    assert.notEqual(
      await readFile(join(root, "dist", "asset-cache-sw.js"), "utf8"),
      first,
    );
  } finally {
    await rm(root, { recursive: true, force: true });
  }
  assert.equal(isFeatureAsset("something.map"), false);
});

async function workerHarness({ fail = false } = {}) {
  const listeners = {};
  const storage = new Map();
  const caches = {
    async keys() {
      return [...storage.keys()];
    },
    async delete(key) {
      return storage.delete(key);
    },
    async open(key) {
      if (!storage.has(key)) storage.set(key, new Map());
      const entries = storage.get(key);
      return {
        async put(key, value) {
          entries.set(typeof key === "string" ? key : key.url, value);
        },
        async match(key) {
          return entries.get(typeof key === "string" ? key : key.url);
        },
      };
    },
  };
  let online = true;
  const context = {
    VERSION: "test",
    ASSETS: ["/browse/assets/tab.js", "/browse/eprover/eprover.wasm"],
    self: {
      registration: { scope: "https://example.test/browse/" },
      location: { origin: "https://example.test" },
      clients: { async claim() {} },
      addEventListener(type, fn) {
        listeners[type] = fn;
      },
    },
    caches,
    URL,
    AbortSignal,
    async fetch(url) {
      if (!online || fail) throw new Error("disconnected");
      return new Response(String(url), {
        headers: { "content-type": "application/octet-stream" },
      });
    },
  };
  vm.runInNewContext(
    await readFile(new URL("./asset-cache-sw.js", import.meta.url), "utf8"),
    context,
  );
  return {
    caches,
    disconnect() {
      online = false;
    },
    lifecycle(type) {
      let promise;
      listeners[type]({
        waitUntil(p) {
          promise = p;
        },
      });
      return promise;
    },
    request(path, overrides = {}) {
      let promise;
      listeners.fetch({
        request: {
          url: new URL(path, "https://example.test").href,
          method: "GET",
          mode: "cors",
          ...overrides,
        },
        respondWith(p) {
          promise = p;
        },
      });
      return promise;
    },
  };
}

test("installed feature assets survive server failure; API, navigation, and external data bypass cache", async () => {
  const worker = await workerHarness();
  await worker.lifecycle("install");
  await worker.lifecycle("activate");
  worker.disconnect();
  for (const path of ["/browse/assets/tab.js", "/browse/eprover/eprover.wasm"])
    assert.equal((await worker.request(path)).status, 200);
  for (const path of [
    "/api/me",
    "/browse/version.json",
    "https://api.github.com/repos",
  ])
    assert.equal(worker.request(path), undefined);
  assert.equal(
    worker.request("/browse/assets/tab.js", { method: "POST" }),
    undefined,
  );
  assert.equal(worker.request("/browse/", { mode: "navigate" }), undefined);
});

test("failed precache is rejected and discarded", async () => {
  const worker = await workerHarness({ fail: true });
  await assert.rejects(worker.lifecycle("install"), /disconnected/);
  assert.deepEqual(await worker.caches.keys(), []);
});

test("activation retires only this deployment's old caches", async () => {
  const worker = await workerHarness();
  await worker.caches.open("sigma-features:https://example.test/browse/:old");
  await worker.caches.open("sigma-features:https://example.test/other/:old");
  await worker.caches.open("unrelated-cache");
  await worker.lifecycle("install");
  // Installing a new version must not remove assets used by open old tabs.
  assert.ok(
    (await worker.caches.keys()).includes(
      "sigma-features:https://example.test/browse/:old",
    ),
  );
  await worker.lifecycle("activate");
  assert.deepEqual(await worker.caches.keys(), [
    "sigma-features:https://example.test/other/:old",
    "unrelated-cache",
    "sigma-features:https://example.test/browse/:test",
  ]);
});
