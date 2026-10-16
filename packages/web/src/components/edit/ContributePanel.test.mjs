import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import { parse, compileScript } from "@vue/compiler-sfc";
import * as vue from "vue";
import ts from "typescript";

const { descriptor } = parse(
  readFileSync(new URL("./ContributePanel.vue", import.meta.url), "utf8"),
);
const source = ts.transpileModule(
  compileScript(descriptor, { id: "push-test" }).content,
  {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  },
).outputText;

function fixture({
  dirty = false,
  fail = false,
  mixed = false,
  signedIn = true,
} = {}) {
  const calls = [];
  const origin = {
    kind: "sumo",
    owner: "alice",
    repo: "sumo",
    branch: "work/ontology",
    id: "github:alice/sumo@work/ontology",
    label: "alice/sumo@work/ontology",
    pathOf: (name) => name.replace("alice/sumo/", ""),
  };
  const rows = [
    {
      name: "alice/sumo/Merge.kif",
      origin: "sumo",
      path: "alice/sumo/Merge.kif",
      state: "modified",
      proposed: null,
    },
  ];
  if (mixed) rows.push({ ...rows[0], name: "other.kif" });
  const changes = vue.reactive({
    rows,
    upstreamShas: {},
    trackedText: () => "(saved)",
    trackedOrigin: (row) =>
      row.name === "other.kif"
        ? { ...origin, id: "other", branch: "other" }
        : origin,
    record: () => ({ baseBlobSha: "base" }),
    async markPushed(entries) {
      calls.push(["mark", entries]);
      this.rows = [];
    },
  });
  const imports = {
    vue,
    "../../constants": { SUMO: { owner: "ontologyportal", repo: "sumo" } },
    "../../stores/changes": {
      useChangesStore: () => changes,
      isActionable: () => true,
      hasRepoPath: () => true,
    },
    "../../stores/auth": {
      useAuthStore: () => ({
        token: signedIn ? "test-token" : null,
        openLoginDialog: () => calls.push(["login"]),
      }),
    },
    "../../stores/kb": { useKBStore: () => ({ constituents: [] }) },
    "../../api/github": {
      async pushFiles(args) {
        calls.push(["push", args]);
        if (fail) throw new Error("Remote changed");
        return { url: "https://github.com/commit", blobShas: ["saved"] };
      },
      async createBranchPullRequest(args) {
        calls.push(["pr", args]);
        return { html_url: "https://github.com/pr/7", number: 7 };
      },
    },
    "../../composables/useStatus": {
      useStatus: () => ({
        set: (...args) => calls.push(["status", ...args]),
        clear: () => {},
        fail: (e) => calls.push(["error", e.message]),
      }),
    },
    "../../utils/format": { errMsg: (e) => e.message },
  };
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    require: (name) => imports[name] ?? {},
  });
  const scope = vue.effectScope();
  const panel = scope.run(() =>
    exports.default.setup(
      { open: false, current: { name: rows[0].name, origin }, dirty },
      { expose: () => {}, emit: () => {} },
    ),
  );
  return { panel, calls, scope };
}

test("push uses the loaded fork, branch and repository-relative paths, then offers a PR", async () => {
  const f = fixture();
  try {
    await f.panel.push();
    const args = f.calls.find((c) => c[0] === "push")[1];
    assert.equal(args.owner, "alice");
    assert.equal(args.branch, "work/ontology");
    assert.equal(args.files[0].path, "Merge.kif");
    assert.equal(args.files[0].baseBlobSha, "base");
    assert.equal(args.files[0].content, "(saved)");
    assert.equal(f.panel.result.value.kind, "push");
    await f.panel.proposePushedBranch();
    const pr = f.calls.find((c) => c[0] === "pr")[1];
    assert.equal(pr.headOwner, "alice");
    assert.equal(pr.branch, "work/ontology");
    assert.equal(f.panel.result.value.kind, "pr");
  } finally {
    f.scope.stop();
  }
});

test("unsaved buffers, mixed sources and missing login never push", async () => {
  for (const options of [
    { dirty: true },
    { mixed: true },
    { signedIn: false },
  ]) {
    const f = fixture(options);
    try {
      await f.panel.push();
      assert.ok(!f.calls.some((c) => c[0] === "push" || c[0] === "mark"));
    } finally {
      f.scope.stop();
    }
  }
});

test("failed pushes retain local changes and report the failure", async () => {
  const f = fixture({ fail: true });
  try {
    await f.panel.push();
    assert.equal(f.panel.result.value.text, "Remote changed");
    assert.ok(!f.calls.some((c) => c[0] === "mark"));
    assert.equal(f.panel.submitBusy.value, false);
  } finally {
    f.scope.stop();
  }
});
