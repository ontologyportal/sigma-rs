import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";

const source = ts.transpileModule(
  readFileSync(new URL("./gitlab.ts", import.meta.url), "utf8"),
  {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  },
).outputText;

function fixture(response) {
  const requests = [];
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    require: () => ({}),
    fetch: async (...args) => {
      requests.push(args);
      return response;
    },
  });
  return { ...exports, requests };
}

test("empty 404 gives an actionable error without local development instructions", async () => {
  const f = fixture(new Response(null, { status: 404 }));
  await assert.rejects(f.gitlabRequest("login", { token: "example" }), (e) => {
    assert.match(e.message, /HTTP 404/);
    assert.match(e.message, /Please try again/);
    assert.doesNotMatch(e.message, /localhost|8788|npm run/);
    assert.doesNotMatch(e.message, /Unexpected end|example/);
    return true;
  });
});

test("non-JSON proxy errors are explained without displaying raw response content", async () => {
  for (const status of [200, 502]) {
    const f = fixture(
      new Response("<html>private upstream diagnostic</html>", { status }),
    );
    await assert.rejects(f.gitlabRequest("login"), (e) => {
      assert.match(e.message, new RegExp(`HTTP ${status}`));
      assert.doesNotMatch(e.message, /private upstream/);
      return true;
    });
  }
});

test("JSON authentication errors retain their useful message", async () => {
  const f = fixture(
    Response.json({ error: "GitLab rejected this token." }, { status: 401 }),
  );
  await assert.rejects(f.gitlabRequest("login"), /GitLab rejected this token/);
  const invalid = fixture(Response.json(null, { status: 503 }));
  await assert.rejects(
    invalid.gitlabRequest("login"),
    /GitLab request failed \(503\)/,
  );
});

test("successful sign-in uses the same-origin endpoint and returns the profile", async () => {
  const f = fixture(Response.json({ login: "alice" }));
  assert.equal(
    (await f.gitlabRequest("login", { token: "example" })).login,
    "alice",
  );
  assert.equal(f.requests[0][0], "/api/gitlab");
  assert.equal(f.requests[0][1].credentials, "same-origin");
});

test("all repository requests include their custom server", async () => {
  const g = {
    server: "https://gitlab.nps.edu",
    owner: "fsg",
    repo: "kb",
    branch: "main",
    pathOf: () => "a.kif",
  };
  const tree = fixture(Response.json([]));
  await tree.gitlabTree(g);
  assert.equal(JSON.parse(tree.requests[0][1].body).server, g.server);
  const head = fixture(Response.json({ sha: "abc", date: "2026-11-05" }));
  await head.gitlabHead(g);
  assert.equal(JSON.parse(head.requests[0][1].body).server, g.server);
  const file = fixture(Response.json({ text: "private" }));
  await file.gitlabText(g, "a.kif");
  assert.equal(JSON.parse(file.requests[0][1].body).server, g.server);
});
