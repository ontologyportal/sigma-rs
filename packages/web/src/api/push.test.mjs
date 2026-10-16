import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";

const source = ts.transpileModule(
  readFileSync(new URL("./github.ts", import.meta.url), "utf8"),
  {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  },
).outputText;

function fixture({
  remoteSha = "base",
  canPush = true,
  truncated = false,
  advance = false,
  existingPr = false,
} = {}) {
  const requests = [];
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    TextEncoder,
    TextDecoder,
    btoa,
    require: () => ({ SUMO: { owner: "upstream", repo: "sumo" } }),
    fetch: async (url, options) => {
      const path = new URL(url).pathname;
      const method = options.method ?? "GET";
      const body = options.body ? JSON.parse(options.body) : null;
      requests.push({ url, path, method, body });
      let data;
      let status = 200;
      if (path === "/user") data = { login: "alice" };
      else if (/\/repos\/[^/]+\/sumo$/.test(path))
        data = { default_branch: "master", permissions: { push: canPush } };
      else if (method === "GET" && path.includes("/git/ref/heads/"))
        data = { object: { sha: "parent" } };
      else if (method === "GET" && path.includes("/git/trees/"))
        data = {
          truncated,
          tree: [
            { path: "Merge.kif", type: "blob", mode: "100644", sha: remoteSha },
          ],
        };
      else if (method === "GET" && path.endsWith("/git/commits/parent"))
        data = { tree: { sha: "parent-tree" } };
      else if (method === "POST" && path.endsWith("/git/blobs"))
        data = { sha: "saved" };
      else if (method === "POST" && path.endsWith("/git/trees"))
        data = { sha: "new-tree" };
      else if (method === "POST" && path.endsWith("/git/commits"))
        data = { sha: "new-commit" };
      else if (method === "POST" && path.endsWith("/git/refs")) data = {};
      else if (method === "PATCH" && path.includes("/git/refs/heads/")) {
        status = advance ? 422 : 200;
        data = advance ? { message: "Update is not a fast forward" } : {};
      } else if (path.endsWith("/pulls"))
        data =
          method === "GET"
            ? existingPr
              ? [{ html_url: "https://github.com/pr/7", number: 7 }]
              : []
            : { html_url: "https://github.com/pr/7", number: 7 };
      else assert.fail(`Unexpected request: ${method} ${url}`);
      return {
        ok: status < 400,
        status,
        json: async () => data,
        headers: { get: () => null },
      };
    },
  });
  return { api: exports, requests };
}

const opts = {
  token: "test-token",
  owner: "alice",
  repo: "sumo",
  branch: "work/ontology",
  title: "Update ontology",
  files: [{ path: "Merge.kif", content: "(new)", baseBlobSha: "base" }],
};

test("push commits on the selected branch without creating a branch or pull request", async () => {
  const { api, requests } = fixture();
  const result = await api.pushFiles(opts);
  assert.equal(result.sha, "new-commit");
  assert.equal(result.blobShas[0], "saved");
  assert.ok(
    requests.every(
      (r) =>
        !r.path.endsWith("/pulls") &&
        !r.path.endsWith("/forks") &&
        !r.path.endsWith("/git/refs"),
    ),
  );
  const tree = requests.find(
    (r) => r.method === "POST" && r.path.endsWith("/git/trees"),
  );
  assert.equal(tree.body.base_tree, "parent-tree");
  assert.equal(tree.body.tree[0].path, "Merge.kif");
  assert.deepEqual(
    requests.find((r) => r.path.endsWith("/git/commits") && r.method === "POST")
      .body.parents,
    ["parent"],
  );
  const update = requests.find((r) => r.method === "PATCH");
  assert.equal(update.path, "/repos/alice/sumo/git/refs/heads/work/ontology");
  assert.deepEqual(update.body, { sha: "new-commit", force: false });
});

test("changed remote files, missing permissions, and incomplete trees prevent all writes", async () => {
  for (const settings of [
    { remoteSha: "changed" },
    { canPush: false },
    { truncated: true },
  ]) {
    const { api, requests } = fixture(settings);
    await assert.rejects(api.pushFiles(opts));
    assert.ok(requests.every((r) => r.method === "GET"));
  }
});

test("concurrent remote commits reject the ref update without forcing it", async () => {
  const { api, requests } = fixture({ advance: true });
  await assert.rejects(api.pushFiles(opts), /fast forward/);
  assert.equal(requests.filter((r) => r.method === "PATCH").length, 1);
  assert.equal(requests.at(-1).body.force, false);
});

test("new files cannot overwrite existing remote paths and invalid paths never reach GitHub", async () => {
  const f = fixture();
  await assert.rejects(
    f.api.pushFiles({
      ...opts,
      files: [{ ...opts.files[0], baseBlobSha: null }],
    }),
    /changed/,
  );
  assert.ok(f.requests.every((r) => r.method === "GET"));
  for (const path of ["", "../Merge.kif", "/Merge.kif", "a//b", "a\\b"]) {
    const { api, requests } = fixture();
    await assert.rejects(
      api.pushFiles({ ...opts, files: [{ ...opts.files[0], path }] }),
      /Invalid/,
    );
    assert.equal(requests.length, 0);
  }
});

test("pull requests can be opened after pushing, and existing ones are reused", async () => {
  for (const existingPr of [false, true]) {
    const { api, requests } = fixture({ existingPr });
    const pr = await api.createBranchPullRequest({
      token: "test-token",
      headOwner: "alice",
      branch: "work/ontology",
      title: "Update",
      body: "Description",
    });
    assert.equal(pr.number, 7);
    const writes = requests.filter((r) => r.method === "POST");
    assert.equal(writes.length, existingPr ? 0 : 1);
    if (!existingPr)
      assert.deepEqual(writes[0].body, {
        title: "Update",
        body: "Description",
        head: "alice:work/ontology",
        base: "master",
      });
  }
});

test("existing pull-request contribution flow still creates one commit and a pull request", async () => {
  const { api, requests } = fixture();
  const pr = await api.contributeFiles({ ...opts, body: "Description" });
  assert.equal(pr.number, 7);
  assert.match(pr.branch, /^sumo-browser\//);
  assert.equal(
    requests.filter(
      (r) => r.method === "POST" && r.path.endsWith("/git/commits"),
    ).length,
    1,
  );
  assert.equal(
    requests.filter((r) => r.method === "POST" && r.path.endsWith("/pulls"))
      .length,
    1,
  );
});
