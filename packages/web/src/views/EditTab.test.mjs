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
  const timers = new Map();
  let timerId = 0;
  let clock = 0;
  const hooks = { activated: [], deactivated: [], unmount: [] };
  async function tick(ms) {
    const end = clock + ms;
    while (true) {
      await vue.nextTick();
      const next = [...timers]
        .filter(([, t]) => t.at <= end)
        .sort((a, b) => a[1].at - b[1].at)[0];
      if (!next) break;
      timers.delete(next[0]);
      clock = next[1].at;
      await next[1].fn();
    }
    clock = end;
    await new Promise((resolve) => setImmediate(resolve));
  }
  const files = vue.reactive(new Map());
  const testFiles = vue.reactive(new Map());
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
      onActivated: (fn) => hooks.activated.push(fn),
      onDeactivated: (fn) => hooks.deactivated.push(fn),
      onBeforeUnmount: (fn) => hooks.unmount.push(fn),
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
    "../services/lsp": { lspSyncDocument: async () => [] },
    "../services/sigma": { call: async () => ({ diagnostics: [] }) },
    "../composables/useTabQuery": { useTabQuery: () => ({ onQuery() {} }) },
    "../router": {
      updateParams: (params) => calls.push(["route", params.file]),
    },
  };
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    require: (name) => imports[name] ?? {},
    window: {
      prompt: () => chosen,
      confirm: () => false,
      addEventListener() {},
      removeEventListener() {},
    },
    document: { addEventListener() {}, removeEventListener() {} },
    console,

    localStorage: { setItem() {}, removeItem() {} },
    setTimeout: (fn, delay) => {
      const id = ++timerId;
      timers.set(id, { fn, at: clock + delay });
      return id;
    },
    clearTimeout: (id) => timers.delete(id),
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
  return {
    editor,
    calls,
    files,
    testFiles,
    entries,
    scope,
    kb,
    tests,
    tick,
    hooks,
    timers,
  };
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
    await f.editor.openFile(original);
    f.calls.length = 0;
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
    await f.editor.openFile({
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
      await f.editor.openFile({
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
    await f.editor.openFile({
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

test("autosave waits five idle seconds, resets on typing, and skips unchanged text", async () => {
  const f = fixture();
  try {
    await f.tick(4999);
    assert.equal(
      f.calls.some((c) => c[0] === "save"),
      false,
    );
    f.editor.text.value = "latest edits";
    await f.tick(4999);
    assert.equal(
      f.calls.some((c) => c[0] === "save"),
      false,
    );
    await f.tick(1);
    assert.equal(f.files.get("original.kif").text, "latest edits");
    assert.equal(f.editor.dirty.value, false);
    await f.tick(10000);
    assert.equal(f.calls.filter((c) => c[0] === "save").length, 1);
    assert.equal(
      f.calls.some((c) => c[0] === "route"),
      false,
    );
  } finally {
    f.scope.stop();
  }
});

test("switching files immediately saves the old buffer and cancels its timer", async () => {
  const f = fixture();
  try {
    const next = {
      name: "next.kif",
      text: "next file",
      origin: { kind: "file" },
    };
    f.files.set(next.name, next);
    assert.equal(await f.editor.openFile(next), true);
    assert.equal(f.files.get("original.kif").text, "unsaved edits");
    assert.equal(f.editor.current.value.name, next.name);
    await f.tick(10000);
    assert.equal(f.calls.filter((c) => c[0] === "save").length, 1);
    assert.equal(f.files.get(next.name).text, "next file");
  } finally {
    f.scope.stop();
  }
});

test("failed autosaves preserve the buffer and prevent switching without retry loops", async () => {
  const f = fixture({ fail: true });
  try {
    await f.tick(5000);
    assert.equal(f.editor.text.value, "unsaved edits");
    assert.equal(f.editor.dirty.value, true);
    await f.tick(10000);
    assert.equal(f.calls.filter((c) => c[0] === "save").length, 1);
    assert.equal(await f.editor.openFile(null), false);
    assert.equal(f.editor.current.value.name, "original.kif");
    assert.equal(f.editor.text.value, "unsaved edits");
    assert.equal(
      f.calls.some((c) => c[0] === "error"),
      true,
    );
  } finally {
    f.scope.stop();
  }
});

test("switching waits for an in-flight save and preserves edits typed during it", async () => {
  const f = fixture();
  let finish;
  const update = f.kb.updateConstituentText;
  f.kb.updateConstituentText = async (...args) => {
    await new Promise((resolve) => {
      finish = resolve;
    });
    f.kb.updateConstituentText = update;
    return update(...args);
  };
  try {
    await f.tick(5000);
    assert.equal(f.editor.saving.value, true);
    f.editor.text.value = "typed during save";
    const switching = f.editor.openFile(null);
    assert.equal(f.editor.current.value.name, "original.kif");
    finish();
    assert.equal(await switching, true);
    assert.equal(f.files.get("original.kif").text, "typed during save");
    assert.equal(f.editor.current.value, null);
    assert.deepEqual(
      f.calls.filter((c) => c[0] === "save").map((c) => c[2]),
      ["unsaved edits", "typed during save"],
    );
  } finally {
    f.scope.stop();
  }
});

test("edits during an autosave wait for their own idle delay", async () => {
  const f = fixture();
  let finish;
  const update = f.kb.updateConstituentText;
  f.kb.updateConstituentText = async (...args) => {
    await new Promise((resolve) => {
      finish = resolve;
    });
    f.kb.updateConstituentText = update;
    return update(...args);
  };
  try {
    await f.tick(5000);
    f.editor.text.value = "newer edits";
    await f.tick(1000);
    finish();
    await f.tick(0);
    assert.equal(f.editor.dirty.value, true);
    assert.equal(f.files.get("original.kif").text, "unsaved edits");
    await f.tick(3999);
    assert.equal(f.files.get("original.kif").text, "unsaved edits");
    await f.tick(1);
    assert.equal(f.files.get("original.kif").text, "newer edits");
  } finally {
    f.scope.stop();
  }
});

test("unnamed buffers use Save As for their first save and cannot be silently discarded", async () => {
  const f = fixture();
  try {
    await f.editor.openFile(null);
    f.calls.length = 0;
    f.editor.text.value = "new unnamed work";
    await f.tick(10000);
    assert.equal(
      f.calls.some((c) => c[0] === "save"),
      false,
    );
    const unnamed = f.editor.tabs.value.find(
      (t) => t.id === f.editor.activeTab.value,
    );
    assert.equal(await f.editor.openFile(f.files.get("original.kif")), true);
    assert.equal(await f.editor.selectTab(unnamed), true);
    assert.equal(f.editor.text.value, "new unnamed work");
    await f.editor.closeTab(unnamed.id);
    assert.ok(f.editor.tabs.value.some((t) => t.id === unnamed.id));
    await f.editor.onSaveAs();
    assert.equal(f.files.get("copy.kif").text, "new unnamed work");
  } finally {
    f.scope.stop();
  }
});

test("autosave uses the existing test save path with an explicit target", async () => {
  const f = fixture();
  try {
    const test = {
      name: "test.kif.tq",
      text: "(query A)",
      origin: { kind: "file" },
    };
    f.testFiles.set(test.name, test);
    await f.editor.openFile(test);
    f.calls.length = 0;
    f.editor.text.value = "(query B)";
    await f.tick(5000);
    assert.equal(f.testFiles.get(test.name).text, "(query B)");
    assert.equal(f.calls.filter((c) => c[0] === "saveTest").length, 1);
    assert.equal(
      f.calls.some((c) => c[0] === "save"),
      false,
    );
  } finally {
    f.scope.stop();
  }
});

test("leaving the Edit tab flushes pending saves and page closing warns until saved", async () => {
  const f = fixture();
  try {
    let prevented = false;
    f.editor.onBeforeUnload({
      preventDefault() {
        prevented = true;
      },
    });
    assert.equal(prevented, true);
    for (const hook of f.hooks.deactivated) hook();
    await f.tick(0);
    assert.equal(f.files.get("original.kif").text, "unsaved edits");
    prevented = false;
    f.editor.onBeforeUnload({
      preventDefault() {
        prevented = true;
      },
    });
    assert.equal(prevented, false);
  } finally {
    f.scope.stop();
  }
});

test("rapid file selections open only the latest requested file after saving", async () => {
  const f = fixture();
  let finish;
  const update = f.kb.updateConstituentText;
  f.kb.updateConstituentText = async (...args) => {
    await new Promise((resolve) => {
      finish = resolve;
    });
    return update(...args);
  };
  try {
    const b = { name: "b.kif", text: "B", origin: { kind: "file" } };
    const c = { name: "c.kif", text: "C", origin: { kind: "file" } };
    f.files.set(b.name, b);
    f.files.set(c.name, c);
    const first = f.editor.openFile(b);
    const last = f.editor.openFile(c);
    finish();
    assert.equal(await first, false);
    assert.equal(await last, true);
    assert.equal(f.editor.current.value.name, c.name);
    assert.equal(f.files.get("original.kif").text, "unsaved edits");
    assert.equal(f.calls.filter((c) => c[0] === "save").length, 1);
  } finally {
    f.scope.stop();
  }
});

test("edits during an unnamed file's first save are subsequently autosaved", async () => {
  const f = fixture();
  let finish;
  try {
    await f.editor.openFile(null);
    f.editor.text.value = "first version";
    const update = f.kb.updateConstituentText;
    f.kb.updateConstituentText = async (...args) => {
      await new Promise((resolve) => {
        finish = resolve;
      });
      f.kb.updateConstituentText = update;
      return update(...args);
    };
    const saving = f.editor.onSaveAs();
    f.editor.text.value = "newer unnamed edits";
    finish();
    await saving;
    assert.equal(f.files.get("copy.kif").text, "first version");
    await f.tick(5000);
    assert.equal(f.files.get("copy.kif").text, "newer unnamed edits");
  } finally {
    f.scope.stop();
  }
});

test("reverting to saved text cancels an unnecessary autosave", async () => {
  const f = fixture();
  try {
    f.editor.text.value = "saved";
    await f.tick(10000);
    assert.equal(
      f.calls.some((c) => c[0] === "save"),
      false,
    );
  } finally {
    f.scope.stop();
  }
});

test("a failed save can be retried after correcting the buffer", async () => {
  const f = fixture({ fail: true });
  try {
    await f.tick(5000);
    f.kb.updateConstituentText = async (name, text, origin) => {
      f.files.set(name, { name, text, origin });
      return { notices: [] };
    };
    f.editor.text.value = "corrected edits";
    await f.tick(5000);
    assert.equal(f.files.get("original.kif").text, "corrected edits");
    assert.equal(f.editor.dirty.value, false);
  } finally {
    f.scope.stop();
  }
});

test("clearing a constituent autosaves its empty buffer", async () => {
  const f = fixture();
  try {
    f.editor.text.value = "";
    await f.tick(5000);
    assert.equal(f.files.get("original.kif").text, "");
    assert.equal(f.editor.dirty.value, false);
  } finally {
    f.scope.stop();
  }
});

test("a test parse failure preserves the original test and blocks file switching", async () => {
  const f = fixture();
  try {
    const test = {
      name: "test.kif.tq",
      text: "(query A)",
      origin: { kind: "file" },
    };
    f.testFiles.set(test.name, test);
    await f.editor.openFile(test);
    f.tests.saveCurrent = async () => {
      throw new Error("Parse failed");
    };
    f.editor.text.value = "malformed test";
    await f.tick(5000);
    assert.equal(await f.editor.openFile(null), false);
    assert.equal(f.editor.text.value, "malformed test");
    assert.equal(f.testFiles.get(test.name).text, "(query A)");
  } finally {
    f.scope.stop();
  }
});

test("an external test update refreshes a clean editor without overwriting the new text", async () => {
  const f = fixture();
  try {
    const test = {
      name: "test.kif.tq",
      text: "(query A)",
      origin: { kind: "file" },
    };
    f.testFiles.set(test.name, test);
    await f.editor.openFile(test);
    f.calls.length = 0;
    f.testFiles.set(test.name, { ...test, text: "(query NewQuery)" });
    for (const hook of f.hooks.activated) hook();
    await f.tick(0);
    assert.equal(f.editor.text.value, "(query NewQuery)");
    assert.equal(
      f.calls.some((c) => c[0] === "saveTest"),
      false,
    );
  } finally {
    f.scope.stop();
  }
});

test("reverting to the previous saved text during a save is autosaved afterward", async () => {
  const f = fixture();
  let finish;
  const update = f.kb.updateConstituentText;
  f.kb.updateConstituentText = async (...args) => {
    await new Promise((resolve) => {
      finish = resolve;
    });
    f.kb.updateConstituentText = update;
    return update(...args);
  };
  try {
    await f.tick(5000);
    f.editor.text.value = "saved";
    finish();
    await f.tick(0);
    assert.equal(f.files.get("original.kif").text, "unsaved edits");
    await f.tick(5000);
    assert.equal(f.files.get("original.kif").text, "saved");
  } finally {
    f.scope.stop();
  }
});

test("edits remain saveable when persistence succeeds but postprocessing fails", async () => {
  const f = fixture();
  let finish;
  const update = f.kb.updateConstituentText;
  f.kb.updateConstituentText = async (...args) => {
    await new Promise((resolve) => {
      finish = resolve;
    });
    await update(...args);
    f.kb.updateConstituentText = update;
    throw new Error("Postprocessing failed");
  };
  try {
    await f.tick(5000);
    f.editor.text.value = "saved";
    finish();
    await f.tick(0);
    assert.equal(f.files.get("original.kif").text, "unsaved edits");
    await f.tick(5000);
    assert.equal(f.files.get("original.kif").text, "saved");
  } finally {
    f.scope.stop();
  }
});

test("opening files adds tabs and reopening selects the existing tab", async () => {
  const f = fixture();
  try {
    const originalId = f.editor.activeTab.value;
    const next = { name: "next.kif", text: "next", origin: { kind: "file" } };
    f.files.set(next.name, next);
    await f.editor.openFile(next);
    assert.equal(f.editor.tabs.value.length, 2);
    await f.editor.openFile(f.files.get("original.kif"));
    assert.equal(f.editor.tabs.value.length, 2);
    assert.equal(f.editor.activeTab.value, originalId);
    assert.equal(f.editor.text.value, "unsaved edits");
  } finally {
    f.scope.stop();
  }
});

test("closing inactive and active tabs preserves files and selects a neighbor", async () => {
  const f = fixture();
  try {
    const originalId = f.editor.activeTab.value;
    await f.editor.openFile(null);
    const blankId = f.editor.activeTab.value;
    await f.editor.openFile(null);
    const lastId = f.editor.activeTab.value;
    await f.editor.closeTab(blankId);
    assert.equal(f.editor.activeTab.value, lastId);
    await f.editor.closeTab(lastId);
    assert.equal(f.editor.activeTab.value, originalId);
    await f.editor.closeTab(originalId);
    assert.equal(f.editor.tabs.value.length, 0);
    assert.equal(f.editor.activeTab.value, null);
    assert.equal(f.editor.current.value, null);
    assert.equal(f.files.get("original.kif").text, "unsaved edits");
  } finally {
    f.scope.stop();
  }
});

test("failed saves prevent closing the active tab", async () => {
  const f = fixture({ fail: true });
  try {
    const id = f.editor.activeTab.value;
    await f.editor.closeTab(id);
    assert.equal(f.editor.activeTab.value, id);
    assert.equal(f.editor.tabs.value.length, 1);
    assert.equal(f.editor.text.value, "unsaved edits");
  } finally {
    f.scope.stop();
  }
});

test("dragging tabs reorders them without changing the active file", async () => {
  const f = fixture();
  try {
    await f.editor.openFile(null);
    await f.editor.openFile(null);
    const ids = f.editor.tabs.value.map((t) => t.id);
    const active = f.editor.activeTab.value;
    f.editor.draggedTab.value = ids[0];
    f.editor.dropTab(ids[2]);
    assert.deepEqual(
      Array.from(f.editor.tabs.value, (t) => t.id),
      [ids[1], ids[2], ids[0]],
    );
    assert.equal(f.editor.activeTab.value, active);
    f.editor.dropTab(ids[0]);
    assert.equal(f.editor.tabs.value.length, 3);
  } finally {
    f.scope.stop();
  }
});

test("inactive unnamed work still warns before leaving the page", async () => {
  const f = fixture();
  try {
    await f.editor.openFile(null);
    f.editor.text.value = "keep this draft";
    await f.editor.openFile(f.files.get("original.kif"));
    let warned = false;
    f.editor.onBeforeUnload({
      preventDefault() {
        warned = true;
      },
    });
    assert.equal(warned, true);
  } finally {
    f.scope.stop();
  }
});

for (const name of ["test.kif.tq", "test.p"])
  test(`a saved ${name} update arriving after activation refreshes the raw buffer`, async () => {
    const f = fixture();
    try {
      const original = { name, text: "original", origin: { kind: "file" } };
      f.testFiles.set(name, original);
      await f.editor.openFile(original);
      for (const hook of f.hooks.activated) hook();
      await f.tick(0);
      f.calls.length = 0;
      f.testFiles.set(name, { ...original, text: "saved from AskTell" });
      await f.tick(0);
      assert.equal(f.editor.text.value, "saved from AskTell");
      assert.equal(f.editor.dirty.value, false);
      await f.tick(10000);
      assert.equal(
        f.calls.some((c) => c[0] === "saveTest"),
        false,
      );
      await f.editor.openFile(null);
      await f.editor.openFile(f.testFiles.get(name));
      assert.equal(f.editor.text.value, "saved from AskTell");
    } finally {
      f.scope.stop();
    }
  });

test("external saves preserve unsaved raw edits and leave other files alone", async () => {
  const f = fixture();
  try {
    const original = {
      name: "test.kif.tq",
      text: "original",
      origin: { kind: "file" },
    };
    f.testFiles.set(original.name, original);
    await f.editor.openFile(original);
    f.editor.text.value = "raw draft";
    f.testFiles.set(original.name, { ...original, text: "external save" });
    await f.tick(0);
    assert.equal(f.editor.text.value, "raw draft");
    assert.equal(f.editor.dirty.value, true);
    await f.editor.openFile(f.files.get("original.kif"));
    f.testFiles.set(original.name, { ...original, text: "another save" });
    await f.tick(0);
    assert.equal(f.editor.text.value, "unsaved edits");
    await f.editor.openFile(f.testFiles.get(original.name));
    assert.equal(f.editor.text.value, "another save");
  } finally {
    f.scope.stop();
  }
});
