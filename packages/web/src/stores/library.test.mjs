import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { createPinia, setActivePinia, defineStore } from "pinia";

const source = ts.transpileModule(
  readFileSync(new URL("./library.ts", import.meta.url), "utf8"),
  {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  },
).outputText;

/** An in-memory stand-in for an OPFS directory handle: files are strings,
 *  subdirectories are nested `FakeDir`s. */
class FakeDir {
  kind = "directory";
  constructor() {
    this.files = new Map();
    this.dirs = new Map();
  }
  async getDirectoryHandle(name, { create = false } = {}) {
    if (!this.dirs.has(name)) {
      if (!create) throw new Error(`NotFoundError: ${name}`);
      this.dirs.set(name, new FakeDir());
    }
    return this.dirs.get(name);
  }
  async getFileHandle(name, { create = false } = {}) {
    if (!this.files.has(name)) {
      if (!create) throw new Error(`NotFoundError: ${name}`);
      this.files.set(name, "");
    }
    const files = this.files;
    return {
      kind: "file",
      async getFile() {
        const text = files.get(name);
        return { text: async () => text, size: text.length };
      },
      async createWritable() {
        let buf = "";
        return {
          async write(t) {
            buf += t;
          },
          async close() {
            files.set(name, buf);
          },
        };
      },
    };
  }
  async removeEntry(name) {
    if (!this.files.delete(name) && !this.dirs.delete(name))
      throw new Error(`NotFoundError: ${name}`);
  }
  async *entries() {
    for (const name of this.files.keys())
      yield [name, await this.getFileHandle(name)];
    for (const [name, d] of this.dirs) yield [name, d];
  }
}

function fixture({ persisted = null } = {}) {
  setActivePinia(createPinia());
  const root = new FakeDir();
  const stored = new Map();
  if (persisted) stored.set("library", JSON.stringify(persisted));
  const imports = {
    pinia: { defineStore },
    "../constants": {
      LIBRARY_KEY: "library",
      SUMO: { owner: "o", repo: "r", branch: "b" },
    },
    "../models/Origin": {
      GitOrigin: class {
        kind = "sumo";
      },
      LocalOrigin: class {
        kind = "file";
      },
    },
    "../api/github": {},
    "../services/sources": {},
    "../utils/format": {},
    "./boot": { useBootStore: () => ({ opfsRoot: root }) },
    "./changes": { opfsSafeName: encodeURIComponent },
    "./tests": { isTestFile: (n) => /\.(tq|p|tptp)$/i.test(n) },
  };
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    require: (name) => imports[name],
    localStorage: {
      getItem: (k) => stored.get(k) ?? null,
      setItem: (k, v) => stored.set(k, v),
    },
  });
  const store = exports.useLibraryStore();
  const library = () => root.dirs.get("library");
  return { store, root, library, stored };
}

// Store values live in the vm realm; copy into host arrays for deepEqual.
const host = (a) => Array.from(a);
const names = (entries) =>
  host(entries)
    .map((e) => `${e.kind}:${e.name}`)
    .sort();

test("adoptLegacy moves loose root uploads into the library and lists them", async () => {
  const f = fixture();
  f.root.files.set("Legacy.kif", "(instance A B)");
  f.root.files.set("dev/x.kif", "(instance C D)");
  f.root.files.set("legacy.kif.tq", "(query (instance A B))");
  f.root.files.set("notes.txt", "ignored");
  await f.root.getDirectoryHandle("sumo-cache", { create: true });

  const adopted = await f.store.adoptLegacy();

  assert.deepEqual(host(adopted).sort(), [
    "Legacy.kif",
    "dev/x.kif",
    "legacy.kif.tq",
  ]);
  assert.deepEqual([...f.root.files.keys()], ["notes.txt"]);
  assert.ok(f.root.dirs.has("sumo-cache"));
  assert.equal(f.library().files.get("Legacy.kif"), "(instance A B)");
  assert.equal(f.library().files.get("dev%2Fx.kif"), "(instance C D)");
  assert.deepEqual(names(f.store.entries), [
    "file:Legacy.kif",
    "file:dev/x.kif",
    "file:legacy.kif.tq",
  ]);
  assert.equal(await f.store.readLocal("dev/x.kif"), "(instance C D)");
  assert.ok(JSON.parse(f.stored.get("library")).entries.length === 3);
});

test("adoptLegacy never overwrites a library copy and is idempotent", async () => {
  const f = fixture();
  f.root.files.set("A.kif", "old root copy");
  const lib = await f.root.getDirectoryHandle("library", { create: true });
  lib.files.set("A.kif", "newer library copy");

  await f.store.adoptLegacy();
  assert.equal(f.library().files.get("A.kif"), "newer library copy");
  assert.equal(f.root.files.has("A.kif"), false);
  assert.deepEqual(names(f.store.entries), ["file:A.kif"]);
  assert.equal(f.store.entries[0].size, "newer library copy".length);

  assert.deepEqual(host(await f.store.adoptLegacy()), []);
  assert.deepEqual(names(f.store.entries), ["file:A.kif"]);
});

test("adoptLegacy skips a file it cannot move and keeps going", async () => {
  const f = fixture();
  f.root.files.set("bad.kif", "x");
  f.root.files.set("good.kif", "y");
  const removeEntry = f.root.removeEntry.bind(f.root);
  f.root.removeEntry = async (name) => {
    if (name === "bad.kif") throw new Error("NoModificationAllowedError");
    return removeEntry(name);
  };

  assert.deepEqual(host(await f.store.adoptLegacy()), ["good.kif"]);
  assert.equal(f.root.files.has("bad.kif"), true);
  assert.equal(f.root.files.has("good.kif"), false);
});

test("ensureEntry lists loaded url/file constituents once, never sumo", () => {
  const f = fixture({
    persisted: {
      repos: [],
      entries: [{ kind: "file", name: "have.kif", size: 1, added: 1 }],
    },
  });
  f.store.ensureEntry("Merge.kif", { kind: "sumo" }, 10);
  f.store.ensureEntry("have.kif", { kind: "file" }, 99);
  f.store.ensureEntry("https://x.org/a.kif", { kind: "url", url: "" }, 5);
  f.store.ensureEntry("b.kif", { kind: "url", url: "https://x.org/b.kif" }, 6);
  f.store.ensureEntry("b.kif", { kind: "url", url: "https://x.org/b.kif" }, 6);
  f.store.ensureEntry("new.kif", { kind: "file" }, 7);

  assert.deepEqual(names(f.store.entries), [
    "file:have.kif",
    "file:new.kif",
    "url:b.kif",
    "url:https://x.org/a.kif",
  ]);
  assert.equal(f.store.entry("have.kif", "file").size, 1);
  assert.equal(
    f.store.entry("https://x.org/a.kif", "url").url,
    "https://x.org/a.kif",
  );
  assert.equal(JSON.parse(f.stored.get("library")).entries.length, 4);
});

test("deleteEntry removes the library copy; readLocal then throws", async () => {
  const f = fixture();
  await f.store.writeLocal("a.kif", "text");
  assert.equal(await f.store.readLocal("a.kif"), "text");
  await f.store.deleteEntry("a.kif", "file");
  assert.equal(f.store.entries.length, 0);
  await assert.rejects(f.store.readLocal("a.kif"), /not in the library/);
  await f.store.deleteEntry("a.kif", "file"); // already gone: not an error
});
