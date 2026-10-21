import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";

test("public repository trees encode branch names and need no token", async () => {
  const source = ts.transpileModule(
    readFileSync(new URL("./github.ts", import.meta.url), "utf8"),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    },
  ).outputText;
  const requests = [];
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    require: (name) =>
      name === "../stores/auth"
        ? { useAuthStore: () => ({ token: null }) }
        : { SUMO: { owner: "ontologyportal", repo: "sumo", branch: "master" } },
    fetch: async (url, options) => {
      requests.push({ url, options });
      return {
        ok: true,
        status: 200,
        json: async () => ({ tree: [{ path: "Merge.kif", type: "blob" }] }),
      };
    },
  });
  const tree = await exports.fetchRepoTree(
    "alice",
    "sumo",
    "work/ontology#one",
  );
  assert.equal(tree[0].path, "Merge.kif");
  assert.equal(
    requests[0].url,
    "https://api.github.com/repos/alice/sumo/git/trees/work%2Fontology%23one?recursive=1",
  );
  assert.equal(requests[0].options.headers.Authorization, undefined);
});

test("branch listing reads every page and preserves slash names", async () => {
  const source = ts.transpileModule(
    readFileSync(new URL("./github.ts", import.meta.url), "utf8"),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    },
  ).outputText;
  const requests = [];
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    require: () => ({ useAuthStore: () => ({ token: null }) }),
    fetch: async (url) => {
      requests.push(url);
      return {
        ok: true,
        status: 200,
        json: async () =>
          requests.length === 1
            ? Array.from({ length: 100 }, (_, i) => ({ name: `branch-${i}` }))
            : [{ name: "work/ontology" }],
      };
    },
  });
  const names = await exports.fetchRepoBranches("alice", "sumo");
  assert.equal(names.length, 101);
  assert.equal(names[100], "work/ontology");
  assert.match(requests[1], /branches\?per_page=100&page=2$/);
});

test("proof sharing reads branch refs afresh and encodes slash-containing branch names", async () => {
  const source = ts.transpileModule(
    readFileSync(new URL("./github.ts", import.meta.url), "utf8"),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    },
  ).outputText;
  const requests = [];
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    require: () => ({ useAuthStore: () => ({ token: null }) }),
    fetch: async (url, options) => {
      requests.push({ url, options });
      return {
        ok: true,
        status: 200,
        json: async () => ({
          object: { sha: String(requests.length).repeat(40) },
        }),
      };
    },
  });
  assert.equal(
    await exports.fetchBranchHead("alice", "sumo", "work/ontology"),
    "1".repeat(40),
  );
  assert.equal(
    await exports.fetchBranchHead("alice", "sumo", "work/ontology"),
    "2".repeat(40),
  );
  assert.equal(
    requests[0].url,
    "https://api.github.com/repos/alice/sumo/git/ref/heads/work%2Fontology",
  );
  assert.ok(requests.every((r) => r.options.cache === "no-store"));
});
