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
const origins = {};
const sumo = { owner: "ontologyportal", repo: "sumo", branch: "master" };
vm.runInNewContext(
  transpile(
    readFileSync(new URL("../models/Origin.ts", import.meta.url), "utf8"),
  ),
  {
    exports: origins,
    require: () => ({ SUMO: sumo }),
  },
);
const { descriptor } = parse(
  readFileSync(new URL("./KbTab.vue", import.meta.url), "utf8"),
);
const source = transpile(
  compileScript(descriptor, { id: "branch-test" }).content,
);

function fixture({ confirm = true, fail = false } = {}) {
  const calls = [];
  const original = new origins.GitOrigin("github", "alice", "sumo", "work/old");
  const kb = {
    constituents: [
      { name: original.nameFor("Merge.kif"), origin: original, text: "(old)" },
    ],
    async loadBranch(origin, paths) {
      calls.push({ origin, paths });
      if (fail) throw new Error("Download failed");
      return { failed: [] };
    },
  };
  const log = { set: () => {}, fail: (e) => calls.push(e.message) };
  const imports = {
    vue: { ...vue, onMounted: () => {}, onActivated: () => {} },
    "vue-router": { onBeforeRouteLeave: () => {} },
    "../api/github": {
      fetchRepoBranches: async () => ["master", "work/old", "work/new"],
    },
    "../models/Origin": origins,
    "../models/Constituent": {
      Constituent: { defaults: () => [{ name: "Merge.kif" }] },
    },
    "../stores/kb": { useKBStore: () => kb },
    "../stores/library": {
      useLibraryStore: () => ({
        defaultRepo: sumo,
        repos: [],
        loadCatalogs: () => {},
      }),
    },
    "../composables/useStatus": { useStatus: () => log },
    "../composables/useTabQuery": {
      useTabQuery: () => ({ query: vue.ref({}), str: String }),
    },
  };
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    require: (name) => imports[name] ?? {},
    window: { confirm: () => confirm },
  });
  const scope = vue.effectScope();
  const view = scope.run(() => exports.default.setup({}, { expose: () => {} }));
  return { view, calls, scope };
}

test("branch selection starts from the loaded origin and switches the same repo paths", async () => {
  const f = fixture();
  try {
    assert.equal(f.view.branch.value, "work/old");
    f.view.branch.value = "work/new";
    await f.view.loadPreset("current");
    assert.equal(f.calls[0].origin.owner, "alice");
    assert.equal(f.calls[0].origin.branch, "work/new");
    assert.deepEqual(Array.from(f.calls[0].paths), ["Merge.kif"]);
    assert.equal(f.view.presetsBusy.value, false);
  } finally {
    f.scope.stop();
  }
});

test("cancelling replacement does not load anything", async () => {
  const f = fixture({ confirm: false });
  try {
    await f.view.loadPreset("minimal");
    assert.deepEqual(f.calls, []);
  } finally {
    f.scope.stop();
  }
});

test("standard sets use the selected fork and recover from download errors", async () => {
  const f = fixture({ fail: true });
  try {
    f.view.owner.value = "bob";
    f.view.branch.value = "feature/one";
    await f.view.loadPreset("minimal");
    assert.equal(f.calls[0].origin.owner, "bob");
    assert.equal(f.calls[0].origin.branch, "feature/one");
    assert.equal(f.calls[0].paths.length, 4);
    assert.equal(f.calls[1], "Download failed");
    assert.equal(f.view.presetsBusy.value, false);
  } finally {
    f.scope.stop();
  }
});

test("dropdown lists and filters branches, and selecting a branch loads it", async () => {
  const f = fixture();
  try {
    await f.view.toggleBranches();
    assert.equal(f.view.branchMenuOpen.value, true);
    f.view.branchSearch.value = "new";
    assert.deepEqual(Array.from(f.view.filteredBranches.value), ["work/new"]);
    await f.view.selectBranch("work/new");
    assert.equal(f.calls[0].origin.branch, "work/new");
    assert.equal(f.view.branch.value, "work/new");
    assert.equal(f.view.branchMenuOpen.value, false);
  } finally {
    f.scope.stop();
  }
});

test("cancelled dropdown selection keeps the current branch label", async () => {
  const f = fixture({ confirm: false });
  try {
    await f.view.selectBranch("work/new");
    assert.equal(f.view.branch.value, "work/old");
    assert.deepEqual(f.calls, []);
  } finally {
    f.scope.stop();
  }
});

test("repository entry validates account/repository before requesting branches", async () => {
  const f = fixture();
  try {
    f.view.repository.value = "not a repository";
    await f.view.listBranches();
    assert.match(f.view.branchError.value, /account|username/);
    f.view.repository.value = "bob/sumo";
    await f.view.listBranches();
    await f.view.selectBranch("work/new");
    assert.equal(f.calls[0].origin.owner, "bob");
  } finally {
    f.scope.stop();
  }
});
