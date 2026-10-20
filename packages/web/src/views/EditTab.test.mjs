import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import { parse, compileScript } from "@vue/compiler-sfc";
import * as vue from "vue";
import ts from "typescript";

const { descriptor } = parse(
  readFileSync(new URL("./EditTab.vue", import.meta.url), "utf8"),
);
const source = ts.transpileModule(
  compileScript(descriptor, { id: "edit-test" }).content,
  {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  },
).outputText;

function fixture({
  chosen = "copy.kif",
  fail = false,
  collision = false,
} = {}) {
  const calls = [];
  const files = new Map();
  const testFiles = new Map();
  const entries = new Set(collision ? [chosen] : []);
  const isTestFile = (name) => /\.(tq|p|tptp)$/i.test(name);
  const status = {
    set: (message) => calls.push(["status", message]),
    clear() {},
    fail: (error) => calls.push(["error", error.message]),
  };
  const kb = {
    find: (name) => files.get(name),
    async updateConstituentText(name, text, origin) {
      calls.push(["save", name, text, origin.kind]);
      if (fail) throw new Error("Write failed");
      files.set(name, { name, text, origin });
      entries.add(name);
      return { notices: [] };
    },
    async remove(name, kind) {
      calls.push(["remove", name, kind]);
      files.delete(name);
    },
  };
  const tests = {
    available: [],
    find: (name) => testFiles.get(name),
    async saveCurrent(text, dialect, target) {
      calls.push(["saveTest", target.name, text, dialect]);
      if (fail) throw new Error("Parse failed");
      testFiles.set(target.name, { ...target, text });
      entries.add(target.name);
      return { saved: true, name: target.name };
    },
    async remove(name, id) {
      calls.push(["removeTest", name, id]);
      testFiles.delete(name);
    },
  };
  const imports = {
    vue: {
      ...vue,
      onActivated() {},
      onDeactivated() {},
      onBeforeUnmount() {},
    },
    "../stores/kb": { useKBStore: () => kb },
    "../stores/tests": {
      useTestsStore: () => tests,
      isTestFile,
      testDialect: (name) => (/\.tq$/i.test(name) ? "kif" : "tptp"),
    },
    "../stores/library": {
      useLibraryStore: () => ({
        entry: (name) => entries.has(name),
        async deleteEntry(name) {
          calls.push(["delete", name]);
          entries.delete(name);
        },
      }),
    },
    "../stores/changes": {
      useChangesStore: () => ({
        rows: [],
        async forget(name) {
          calls.push(["forget", name]);
        },
      }),
    },
    "../stores/auth": { useAuthStore: () => ({}) },
    "../models/Origin": {
      LocalOrigin: class {
        kind = "file";
      },
      serializeOrigin: (origin) => ({ kind: origin.kind }),
      originId: (origin) =>
        origin.kind === "url" ? `url:${origin.url}` : origin.kind,
    },
    "../models/Constituent": {
      engineFile: (name, origin) =>
        origin.kind === "file" ? `uploads/${name}` : name,
    },
    "../composables/useStatus": { useStatus: () => status },
    "../composables/useTabQuery": { useTabQuery: () => ({ onQuery() {} }) },
    "../router": {
      updateParams: (params) => calls.push(["route", params.file]),
    },
  };
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    require: (name) => imports[name] ?? {},
    window: { prompt: () => chosen },
    localStorage: { setItem() {}, removeItem() {} },
    setTimeout: () => 1,
    clearTimeout() {},
  });
  const scope = vue.effectScope();
  const editor = scope.run(() => exports.default.setup({}, { expose() {} }));
  const original = {
    name: "original.kif",
    text: "saved",
    origin: { kind: "file" },
  };
  files.set(original.name, original);
  entries.add(original.name);
  editor.openFile(original);
  editor.text.value = "unsaved edits";
  return { editor, calls, files, testFiles, entries, scope, kb };
}

test("Rename updates the header before the old KB file finishes unloading", async () => {
  const f = fixture();
  let finishRemoval;
  const removing = new Promise((resolve) => {
    const remove = f.kb.remove;
    f.kb.remove = async (...args) => {
      resolve();
      await new Promise((finish) => {
        finishRemoval = finish;
      });
      await remove(...args);
    };
  });
  const renaming = f.editor.onRename();
  try {
    await removing;
    assert.equal(f.editor.fileLabel.value, "copy.kif");
    assert.equal(f.editor.current.value.name, "copy.kif");
    assert.ok(
      f.calls.some((c) => c[0] === "route" && c[1] === "uploads/copy.kif"),
    );
  } finally {
    finishRemoval();
    await renaming;
    f.scope.stop();
  }
});

test("Save As saves the buffer to a local copy and selects its URL", async () => {
  const f = fixture();
  try {
    await f.editor.onSaveAs();
    assert.equal(f.files.get("original.kif").text, "saved");
    assert.equal(f.files.get("copy.kif").text, "unsaved edits");
    assert.equal(f.editor.current.value.name, "copy.kif");
    assert.ok(
      f.calls.some((c) => c[0] === "route" && c[1] === "uploads/copy.kif"),
    );
  } finally {
    f.scope.stop();
  }
});

test("Rename saves first, then removes the old loaded file and library entry", async () => {
  const f = fixture();
  try {
    await f.editor.onRename();
    assert.equal(f.files.has("original.kif"), false);
    assert.equal(f.entries.has("original.kif"), false);
    assert.equal(f.files.get("copy.kif").text, "unsaved edits");
    assert.deepEqual(
      f.calls
        .filter((c) => ["save", "remove", "delete"].includes(c[0]))
        .map((c) => c[0]),
      ["save", "remove", "delete"],
    );
  } finally {
    f.scope.stop();
  }
});

for (const options of [
  { chosen: null },
  { chosen: "  " },
  { chosen: "../bad.kif" },
  { chosen: "original.kif" },
  { collision: true },
  { fail: true },
  { chosen: "wrong.p" },
]) {
  test(`Rename preserves the original on cancellation, invalid names, collisions or failure: ${JSON.stringify(options)}`, async () => {
    const f = fixture(options);
    try {
      await f.editor.onRename();
      assert.equal(f.files.get("original.kif").text, "saved");
      assert.equal(f.editor.current.value.name, "original.kif");
      assert.equal(f.editor.text.value, "unsaved edits");
      assert.equal(
        f.calls.some((c) => c[0] === "remove"),
        false,
      );
    } finally {
      f.scope.stop();
    }
  });
}

test("Rename uses the test store for inference tests", async () => {
  const f = fixture({ chosen: "renamed.kif.tq" });
  try {
    const original = {
      name: "test.kif.tq",
      text: "(query A)",
      origin: { kind: "file" },
    };
    f.testFiles.set(original.name, original);
    f.editor.openFile(original);
    await f.editor.onRename();
    assert.equal(f.testFiles.has(original.name), false);
    assert.equal(f.testFiles.get("renamed.kif.tq").text, original.text);
    assert.ok(
      f.calls.some((c) => c[0] === "route" && c[1] === "renamed.kif.tq"),
    );
    assert.equal(
      f.calls.some((c) => c[0] === "save"),
      false,
    );
  } finally {
    f.scope.stop();
  }
});

test("Save As copies imported files locally without unloading them", async () => {
  const f = fixture();
  try {
    f.editor.openFile({
      name: "Merge.kif",
      text: "imported",
      origin: { kind: "sumo" },
    });
    await f.editor.onSaveAs();
    assert.equal(f.files.get("copy.kif").origin.kind, "file");
    assert.equal(
      f.calls.some((c) => c[0] === "remove"),
      false,
    );
  } finally {
    f.scope.stop();
  }
});

for (const kind of ["sumo", "url"]) {
  test(`Rename replaces a loaded ${kind} constituent with a local file`, async () => {
    const f = fixture();
    try {
      f.editor.openFile({
        name: "Merge.kif",
        text: "imported",
        origin: { kind },
      });
      f.editor.text.value = "edited import";
      await f.editor.onRename();
      assert.equal(f.files.get("copy.kif").text, "edited import");
      assert.equal(f.editor.current.value.origin.kind, "file");
      assert.ok(
        f.calls.some(
          (c) => c[0] === "remove" && c[1] === "Merge.kif" && c[2] === kind,
        ),
      );
      assert.equal(
        f.calls.some((c) => c[0] === "delete"),
        false,
      );
      assert.ok(
        f.calls.some(
          (c) =>
            c[0] === "status" && c[1].includes("original source is unchanged"),
        ),
      );
    } finally {
      f.scope.stop();
    }
  });
}

test("Rename removes an imported test using its full source identity", async () => {
  const f = fixture({ chosen: "renamed.p" });
  try {
    f.editor.openFile({
      name: "test.p",
      text: "test buffer",
      origin: { kind: "url", url: "https://example.com/test.p" },
    });
    await f.editor.onRename();
    assert.ok(
      f.calls.some(
        (c) =>
          c[0] === "removeTest" && c[2] === "url:https://example.com/test.p",
      ),
    );
    assert.equal(f.editor.current.value.name, "renamed.p");
    assert.equal(
      f.calls.some((c) => c[0] === "delete"),
      false,
    );
  } finally {
    f.scope.stop();
  }
});
