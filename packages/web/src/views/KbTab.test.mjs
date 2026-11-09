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
    URL,
    require: () => ({ SUMO: sumo }),
  },
);
const { descriptor } = parse(
  readFileSync(new URL("./KbTab.vue", import.meta.url), "utf8"),
);
const source = transpile(
  compileScript(descriptor, { id: "branch-test" }).content,
);

function fixture({
  confirm = true,
  fail = false,
  service = "github",
  query = {},
  availableBranches = ["master", "work/old", "work/new"],
} = {}) {
  const calls = [];
  const original = new origins.GitOrigin(service, "alice", "sumo", "work/old");
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
    "../api/gitlab": {
      coordinates: (g) => g,
      gitlabRequest: async (op, g) => {
        calls.push({ operation: op, ...g });
        return ["work/old", "work/new"];
      },
    },
    "../api/github": {
      fetchRepoBranches: async () => availableBranches,
    },
    "../router": { updateParams: (params) => calls.push({ params }) },
    "../models/Origin": origins,
    "../models/Constituent": {
      Constituent: { defaults: () => [{ name: "Merge.kif" }] },
    },
    "../stores/auth": {
      useAuthStore: () =>
        vue.reactive({
          gitlabUser: { login: "test", server: "https://gitlab.nps.edu" },
        }),
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
      useTabQuery: () => ({ query: vue.ref(query), str: String }),
    },
  };
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    URL,
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
    assert.match(f.view.branchError.value, /project|account/);
    f.view.repository.value = "bob/sumo";
    await f.view.listBranches();
    await f.view.selectBranch("work/new");
    assert.equal(f.calls[0].origin.owner, "bob");
  } finally {
    f.scope.stop();
  }
});

test("GitLab branch selection preserves its provider and subgroup namespace", async () => {
  const f = fixture({ service: "gitlab" });
  try {
    assert.equal(f.view.service.value, "gitlab");
    f.view.repository.value = "group/subgroup/kb";
    await f.view.listBranches();
    assert.equal(f.calls[0].operation, "branches");
    assert.equal(f.calls[0].owner, "group/subgroup");
    await f.view.selectBranch("work/new");
    const load = f.calls.find((c) => c.origin);
    assert.equal(load.origin.service, "gitlab");
    assert.equal(load.origin.owner, "group/subgroup");
    assert.equal(load.origin.repo, "kb");
  } finally {
    f.scope.stop();
  }
});

test("custom GitLab server follows branch selection and loading", async () => {
  const f = fixture({ service: "gitlab" });
  try {
    f.view.server.value = "https://gitlab.nps.edu";
    f.view.repository.value = "fsg/kb";
    await f.view.listBranches();
    assert.equal(f.calls[0].server, "https://gitlab.nps.edu");
    await f.view.selectBranch("work/new");
    const load = f.calls.find((c) => c.origin);
    assert.equal(load.origin.server, "https://gitlab.nps.edu");
    assert.equal(load.origin.owner, "fsg");
    assert.equal(load.origin.repo, "kb");
  } finally {
    f.scope.stop();
  }
});

test("pasting a full NPS project URL requests fsg/sumo on the matching server", async () => {
  const f = fixture({ service: "gitlab" });
  try {
    f.view.repository.value = "https://gitlab.nps.edu/fsg/sumo";
    await f.view.listBranches();
    assert.equal(f.calls[0].owner, "fsg");
    assert.equal(f.calls[0].repo, "sumo");
    assert.equal(f.calls[0].server, "https://gitlab.nps.edu");
    assert.equal(f.view.repository.value, "fsg/sumo");
  } finally {
    f.scope.stop();
  }
});

test("switching from GitHub to GitLab clears the unrelated GitHub project", async () => {
  const f = fixture();
  try {
    assert.ok(f.view.repository.value);
    f.view.service.value = "gitlab";
    await vue.nextTick();
    assert.equal(f.view.repository.value, "");
    await f.view.listBranches();
    assert.equal(f.calls.length, 0);
  } finally {
    f.scope.stop();
  }
});

test("group URLs do not result in branch requests", async () => {
  const f = fixture({ service: "gitlab" });
  try {
    f.view.repository.value = "https://gitlab.nps.edu/fsg/";
    await f.view.listBranches();
    assert.equal(f.calls.length, 0);
    assert.match(f.view.branchError.value, /group URL alone/);
  } finally {
    f.scope.stop();
  }
});

test("account repository action opens the project selector on its connected server", async () => {
  const f = fixture({
    query: { chooseRepo: "gitlab", server: "https://gitlab.nps.edu" },
  });
  try {
    await vue.nextTick();
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(f.view.service.value, "gitlab");
    assert.equal(f.view.server.value, "https://gitlab.nps.edu");
    assert.equal(f.view.repository.value, "");
    assert.equal(f.view.repoFormOpen.value, true);
    assert.equal(f.view.branchMenuOpen.value, false);
    assert.ok(!f.calls.some((c) => c.operation));
    assert.equal(f.calls[0].params.chooseRepo, undefined);
  } finally {
    f.scope.stop();
  }
});

test("repository provider tabs follow the GitLab session and use its server", async () => {
  const f = fixture();
  try {
    f.view.auth.gitlabUser = null;
    assert.deepEqual(Array.from(f.view.repoProviders.value), ["github"]);
    f.view.selectRepoProvider("gitlab");
    assert.equal(f.view.service.value, "github");
    f.view.auth.gitlabUser = {
      login: "user",
      server: "https://gitlab.example.org",
    };
    assert.deepEqual(Array.from(f.view.repoProviders.value), [
      "github",
      "gitlab",
    ]);
    await vue.nextTick();
    f.view.switchRepository();
    f.view.selectRepoProvider("gitlab");
    assert.equal(f.view.service.value, "gitlab");
    assert.equal(f.view.server.value, "https://gitlab.example.org");
    f.view.auth.gitlabUser = null;
    await vue.nextTick();
    assert.deepEqual(Array.from(f.view.repoProviders.value), ["github"]);
    assert.equal(f.view.service.value, "github");
  } finally {
    f.scope.stop();
  }
});

test("switching repositories automatically prefers master then main", async () => {
  for (const availableBranches of [
    ["work/new", "main", "master"],
    ["work/new", "main"],
  ]) {
    const f = fixture({ availableBranches });
    try {
      await f.view.loadSelectedRepository();
      const loaded = f.calls.find((call) => call.origin);
      assert.equal(
        loaded.origin.branch,
        availableBranches.includes("master") ? "master" : "main",
      );
    } finally {
      f.scope.stop();
    }
  }
});
test("a repository without master or main is not loaded", async () => {
  const f = fixture({ availableBranches: ["feature"] });
  try {
    await f.view.loadSelectedRepository();
    assert.equal(f.calls.filter((call) => call.origin).length, 0);
    assert.match(f.view.branchError.value, /no master or main/);
  } finally {
    f.scope.stop();
  }
});
