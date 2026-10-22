import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";

test("contradiction search paginates and requires the exact reporting prefix", async () => {
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
        : { SUMO: { owner: "ontologyportal", repo: "sumo" } },
    fetch: async (url) => {
      requests.push(url);
      return {
        ok: true,
        status: 200,
        json: async () => ({
          total_count: 75,
          incomplete_results: false,
          items:
            requests.length === 1
              ? [
                  {
                    number: 7,
                    title: "[Contradiction] Geography.kif:1",
                    html_url: "https://github.com/ontologyportal/sumo/issues/7",
                    state: "open",
                  },
                  {
                    number: 8,
                    title: "A contradiction elsewhere",
                    html_url: "https://github.com/ontologyportal/sumo/issues/8",
                    state: "open",
                  },
                  {
                    number: 9,
                    title: "[Contradiction] PR",
                    pull_request: {},
                    html_url: "https://github.com/ontologyportal/sumo/pull/9",
                    state: "open",
                  },
                ]
              : [
                  {
                    number: 10,
                    title: "[Contradiction] Merge.kif:2",
                    html_url:
                      "https://github.com/ontologyportal/sumo/issues/10",
                    state: "closed",
                  },
                ],
        }),
      };
    },
  });
  const first = await exports.searchSumoContradictions();
  assert.deepEqual(
    Array.from(first.issues, (issue) => issue.number),
    [7],
  );
  assert.equal(first.hasMore, true);
  const second = await exports.searchSumoContradictions(2);
  assert.deepEqual(
    Array.from(second.issues, (issue) => issue.number),
    [10],
  );
  assert.equal(second.hasMore, false);
  assert.match(
    decodeURIComponent(requests[0]),
    /repo:ontologyportal\/sumo is:issue in:title contradiction/,
  );
  assert.match(requests[1], /page=2$/);
  await assert.rejects(
    exports.searchSumoContradictions(21),
    /Invalid contradiction issue page/,
  );
});

test("proof issues are read only from SUMO and issue creation uses the signed-in token", async () => {
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
        ? { useAuthStore: () => ({ token: "secret" }) }
        : { SUMO: { owner: "ontologyportal", repo: "sumo" } },
    fetch: async (url, options) => {
      requests.push({ url, options });
      return {
        ok: true,
        status: 200,
        json: async () =>
          options.method === "POST"
            ? {
                number: 42,
                html_url: "https://github.com/ontologyportal/sumo/issues/42",
                title: "[Proof] Test",
                state: "open",
              }
            : { body: "proof body" },
      };
    },
  });
  assert.equal(
    exports.sumoIssueNumber("https://github.com/ontologyportal/sumo/issues/42"),
    42,
  );
  assert.equal(exports.sumoIssueNumber("42"), 42);
  assert.throws(() =>
    exports.sumoIssueNumber("https://github.com/other/sumo/issues/42"),
  );
  assert.equal(await exports.fetchSumoIssue(42), "proof body");
  await exports.createSumoIssue("[Proof] Test", "proof body");
  assert.equal(
    requests[0].url,
    "https://api.github.com/repos/ontologyportal/sumo/issues/42",
  );
  assert.equal(requests[1].options.method, "POST");
  assert.equal(requests[1].options.headers.Authorization, "Bearer secret");
  assert.deepEqual(JSON.parse(requests[1].options.body), {
    title: "[Proof] Test",
    body: "proof body",
  });
});

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
