import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { webcrypto } from "node:crypto";

function fixture({
  authenticated = true,
  changed = false,
  reject = false,
  expires = false,
  invalidToken = false,
  redirected = false,
  missingProject = false,
} = {}) {
  const calls = [];
  const kv = new Map();
  const id = "12345678-1234-1234-1234-123456789abc";
  if (authenticated)
    kv.set(
      `gitlab:${id}`,
      JSON.stringify({
        token: "secret",
        expires: Date.now() + (expires ? -1000 : 100000),
      }),
    );
  const env = {
    SESSIONS: {
      get: async (k) => kv.get(k),
      put: async (k, v) => kv.set(k, v),
      delete: async (k) => kv.delete(k),
    },
    GITLAB_OAUTH_CLIENT_ID: "client",
    GITLAB_OAUTH_CLIENT_SECRET: "client-secret",
    GITLAB_OAUTH_REDIRECT_URI: "https://app.test/api/gitlab-auth-callback",
  };
  const modules = new Map();
  const upstream = async (url, init = {}) => {
    // Match the Workers runtime, which rejects this Fetch redirect mode.
    if (init.redirect === "error")
      throw new TypeError("Unsupported redirect mode: error");
    calls.push({ url, ...init });
    if (redirected)
      return new Response(null, {
        status: 302,
        headers: { Location: "https://untrusted.test/" },
      });
    if (url.endsWith("/user"))
      return Response.json(
        invalidToken ? { message: "Unauthorized" } : { username: "alice" },
        { status: invalidToken ? 401 : 200 },
      );
    if (url === "https://github.com/login/oauth/access_token")
      return Response.json({ access_token: "github-oauth-token" });
    if (url.endsWith("/oauth/token"))
      return Response.json({ access_token: "secret", expires_in: 7200 });
    if (url.endsWith("/oauth/revoke")) return Response.json({});
    if (init.method === "POST")
      return Response.json(
        reject ? { error: "concurrent change" } : { id: "commit" },
        { status: reject ? 400 : 201 },
      );
    if (missingProject && url.includes("/repository/branches"))
      return Response.json(
        { message: "404 Project Not Found" },
        { status: 404 },
      );
    if (url.includes("/raw?")) return new Response("(private text)");
    if (url.includes("repository/tree"))
      return Response.json([
        { path: "a.kif", type: "blob", id: "a".repeat(40) },
      ]);
    if (url.includes("new.kif")) return new Response(null, { status: 404 });
    return Response.json({
      blob_id: (changed ? "b" : "a").repeat(40),
      last_commit_id: "last-file-commit",
    });
  };
  function load(url) {
    if (modules.has(url.href)) return modules.get(url.href);
    const exports = {};
    modules.set(url.href, exports);
    const source = ts.transpileModule(readFileSync(url, "utf8"), {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    }).outputText;
    vm.runInNewContext(source, {
      exports,
      require: (path) => load(new URL(`${path}.ts`, url)),
      fetch: upstream,
      crypto: webcrypto,
      TextEncoder,
      TextDecoder,
      Uint8Array,
      URL,
      URLSearchParams,
      Headers,
      Request,
      Response,
      btoa,
      Date,
    });
    return exports;
  }
  const handler = load(new URL("./gitlab.ts", import.meta.url));
  const request = async (body, origin = "https://app.test", cookieId = id) => {
    const response = await handler.onRequestPost({
      env,
      request: new Request("https://app.test/api/gitlab", {
        method: "POST",
        headers: {
          Origin: origin,
          "Content-Type": "application/json",
          Cookie: `gitlab_session=${cookieId}`,
        },
        body: JSON.stringify({
          owner: "group/subgroup",
          repo: "kb",
          ref: "work/ontology",
          ...body,
        }),
      }),
    });
    return { response, body: await response.json() };
  };
  return { calls, env, kv, id, request, load };
}

const files = [
  { path: "a.kif", content: "(edited)", baseBlobSha: "a".repeat(40) },
];

test("authenticated push keeps tokens server-side and checks the original file", async () => {
  const f = fixture();
  const { response, body } = await f.request({
    operation: "push",
    files,
    title: "Update",
  });
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("Cache-Control"), "no-store");
  assert.ok(!JSON.stringify(body).includes("secret"));
  assert.equal(f.calls.length, 2);
  assert.ok(f.calls[0].url.includes("group%2Fsubgroup%2Fkb"));
  assert.ok(f.calls[0].url.includes("ref=work%2Fontology"));
  const commit = JSON.parse(f.calls[1].body);
  assert.equal(commit.branch, "work/ontology");
  assert.equal(commit.actions[0].last_commit_id, "last-file-commit");
  assert.equal(commit.actions[0].action, "update");
  assert.ok(!("force" in commit));
  for (const call of f.calls) {
    assert.equal(call.redirect, "manual");
    assert.equal(call.headers.Authorization, "Bearer secret");
    assert.equal(new URL(call.url).hostname, "gitlab.com");
  }
});

test("missing or expired login and cross-origin writes fail without contacting GitLab", async () => {
  for (const settings of [{ authenticated: false }, { expires: true }]) {
    const f = fixture(settings);
    assert.equal(
      (await f.request({ operation: "push", files, title: "Update" })).response
        .status,
      401,
    );
    assert.equal(f.calls.length, 0);
  }
  const f = fixture();
  assert.equal(
    (
      await f.request(
        { operation: "push", files, title: "Update" },
        "https://evil.test",
      )
    ).response.status,
    403,
  );
  assert.equal(f.calls.length, 0);
});

test("changed files, accidental overwrite, duplicate paths and traversal never create a commit", async () => {
  const f = fixture({ changed: true });
  assert.equal(
    (await f.request({ operation: "push", files, title: "Update" })).response
      .status,
    409,
  );
  assert.equal(f.calls.filter((c) => c.method === "POST").length, 0);
  for (const selection of [
    [{ ...files[0], baseBlobSha: null }],
    [files[0], files[0]],
    [{ ...files[0], path: "../a.kif" }],
  ]) {
    const f = fixture();
    assert.ok(
      (
        await f.request({
          operation: "push",
          files: selection,
          title: "Update",
        })
      ).response.status >= 400,
    );
    assert.equal(f.calls.filter((c) => c.method === "POST").length, 0);
  }
});

test("creation uses create actions and concurrent commit rejection is returned to the user", async () => {
  const f = fixture();
  assert.equal(
    (
      await f.request({
        operation: "push",
        files: [{ ...files[0], path: "new.kif", baseBlobSha: null }],
        title: "Add file",
      })
    ).response.status,
    200,
  );
  assert.equal(JSON.parse(f.calls[1].body).actions[0].action, "create");
  const rejected = fixture({ reject: true });
  assert.equal(
    (await rejected.request({ operation: "push", files, title: "Update" }))
      .response.status,
    409,
  );
});

test("private file reads are uncached and logout deletes the session and revokes access", async () => {
  const f = fixture();
  const result = await f.request({ operation: "file", path: "a.kif" });
  assert.equal(result.body.text, "(private text)");
  assert.equal(result.response.headers.get("Cache-Control"), "no-store");
  await f.request({ operation: "logout" });
  assert.equal(f.kv.has(`gitlab:${f.id}`), false);
  assert.ok(f.calls.some((c) => c.url.endsWith("/oauth/revoke")));
});

test("OAuth uses PKCE, fixed callback, secure cookies and validates state before exchange", async () => {
  const f = fixture();
  const auth = f.load(new URL("./gitlab-auth.ts", import.meta.url));
  const result = await auth.onRequestGet({ env: f.env });
  const location = new URL(result.headers.get("Location"));
  assert.equal(location.searchParams.get("code_challenge_method"), "S256");
  assert.equal(
    location.searchParams.get("redirect_uri"),
    f.env.GITLAB_OAUTH_REDIRECT_URI,
  );
  assert.equal(location.searchParams.get("scope"), "api");
  const cookies = result.headers.getSetCookie();
  assert.ok(
    cookies.every(
      (c) =>
        c.includes("HttpOnly") &&
        c.includes("Secure") &&
        c.includes("SameSite=Lax"),
    ),
  );
  const callback = f.load(
    new URL("./gitlab-auth-callback.ts", import.meta.url),
  );
  const invalid = await callback.onRequestGet({
    env: f.env,
    request: new Request(
      `${f.env.GITLAB_OAUTH_REDIRECT_URI}?code=code&state=wrong`,
    ),
  });
  assert.equal(invalid.status, 400);
  assert.equal(f.calls.length, 0);
  const valid = await callback.onRequestGet({
    env: f.env,
    request: new Request(
      `${f.env.GITLAB_OAUTH_REDIRECT_URI}?code=code&state=${location.searchParams.get("state")}`,
      { headers: { Cookie: cookies.map((c) => c.split(";")[0]).join("; ") } },
    ),
  });
  assert.equal(valid.status, 302);
  assert.ok(
    valid.headers.getSetCookie().some((c) => c.startsWith("gitlab_session=")),
  );
  assert.ok(f.calls[0].body.get("code_verifier"));
});

test("personal tokens connect without OAuth configuration and are kept out of responses", async () => {
  const f = fixture();
  delete f.env.GITLAB_OAUTH_CLIENT_ID;
  delete f.env.GITLAB_OAUTH_CLIENT_SECRET;
  const { response, body } = await f.request({
    operation: "login",
    token: "glpat-example",
  });
  assert.equal(response.status, 200);
  assert.equal(body.login, "alice");
  assert.ok(!JSON.stringify(body).includes("glpat-example"));
  const cookie = response.headers
    .getSetCookie()
    .find((c) => c.startsWith("gitlab_session="));
  assert.ok(cookie.includes("HttpOnly") && cookie.includes("Secure"));
  const sessionId = cookie.split(";")[0].split("=")[1];
  const saved = JSON.parse(f.kv.get(`gitlab:${sessionId}`));
  assert.equal(saved.token, "glpat-example");
  assert.equal(saved.kind, "personal");
  assert.ok(saved.expires <= Date.now() + 7200000);
  assert.equal(f.kv.has(`gitlab:${f.id}`), false);
  assert.equal(f.calls[0].headers.Authorization, "Bearer glpat-example");
  await f.request(
    { operation: "push", files, title: "Update" },
    "https://app.test",
    sessionId,
  );
  assert.equal(f.calls.at(-1).headers.Authorization, "Bearer glpat-example");
  await f.request({ operation: "logout" }, "https://app.test", sessionId);
  assert.equal(f.kv.has(`gitlab:${sessionId}`), false);
  assert.ok(!f.calls.some((c) => c.url.includes("oauth")));
});

test("invalid personal tokens preserve the old session and cross-origin login is rejected", async () => {
  const f = fixture({ invalidToken: true });
  const result = await f.request({ operation: "login", token: "bad-token" });
  assert.equal(result.response.status, 401);
  assert.equal(result.response.headers.getSetCookie().length, 0);
  assert.ok(f.kv.has(`gitlab:${f.id}`));
  assert.equal(f.kv.size, 1);
  const external = fixture();
  assert.equal(
    (
      await external.request(
        { operation: "login", token: "glpat-test" },
        "https://evil.test",
      )
    ).response.status,
    403,
  );
  assert.equal(external.calls.length, 0);
  for (const token of ["", "has whitespace", 123, "x".repeat(4097)]) {
    assert.equal(
      (await external.request({ operation: "login", token })).response.status,
      400,
    );
  }
  assert.equal(external.calls.length, 0);
});

test("OAuth availability is reported without requiring a session or contacting GitLab", async () => {
  const f = fixture({ authenticated: false });
  assert.equal(
    (await f.request({ operation: "capabilities" })).body.oauthConfigured,
    true,
  );
  delete f.env.GITLAB_OAUTH_CLIENT_ID;
  assert.equal(
    (await f.request({ operation: "capabilities" })).body.oauthConfigured,
    false,
  );
  assert.equal(f.calls.length, 0);
});

test("unconfigured OAuth returns to the app so personal token sign-in remains usable", async () => {
  const f = fixture();
  delete f.env.GITLAB_OAUTH_CLIENT_ID;
  const auth = f.load(new URL("./gitlab-auth.ts", import.meta.url));
  const result = await auth.onRequestGet({ env: f.env });
  assert.equal(result.status, 302);
  assert.equal(result.headers.get("Location"), "/");
  assert.equal(result.headers.get("Cache-Control"), "no-store");
  assert.equal(f.calls.length, 0);
});

test("Workers-compatible fetch rejects redirects without forwarding credentials", async () => {
  const f = fixture({ redirected: true });
  const result = await f.request({
    operation: "login",
    token: "example-personal-token",
  });
  assert.equal(result.response.status, 502);
  assert.match(result.body.error, /Credentials were not forwarded/);
  assert.ok(!JSON.stringify(result.body).includes("example-personal-token"));
  assert.equal(f.calls.length, 1);
  assert.equal(f.calls[0].redirect, "manual");
  assert.equal(new URL(f.calls[0].url).hostname, "gitlab.com");
});

test("personal token login uses the typed GitLab server and binds the session to it", async () => {
  const f = fixture();
  const login = await f.request({
    operation: "login",
    server: "https://gitlab.nps.edu/fsg/",
    token: "company-token",
  });
  assert.equal(login.response.status, 200);
  assert.equal(login.body.server, "https://gitlab.nps.edu");
  assert.equal(f.calls[0].url, "https://gitlab.nps.edu/api/v4/user");
  // An existing GitLab.com session must not be reused at the company host.
  assert.equal(f.calls[0].headers.Authorization, "Bearer company-token");
  const cookie = login.response.headers
    .getSetCookie()
    .find((c) => c.startsWith("gitlab_session="));
  const sessionId = cookie.split(";")[0].split("=")[1];
  assert.equal(
    JSON.parse(f.kv.get(`gitlab:${sessionId}`)).server,
    "https://gitlab.nps.edu",
  );
  const pushed = await f.request(
    {
      operation: "push",
      server: "https://gitlab.nps.edu",
      files,
      title: "Update",
    },
    "https://app.test",
    sessionId,
  );
  assert.equal(pushed.response.status, 200);
  assert.match(pushed.body.url, /^https:\/\/gitlab.nps.edu\//);
  for (const call of f.calls)
    assert.equal(new URL(call.url).hostname, "gitlab.nps.edu");
  const before = f.calls.length;
  const wrongHost = await f.request(
    { operation: "push", server: "https://gitlab.com", files, title: "Update" },
    "https://app.test",
    sessionId,
  );
  assert.equal(wrongHost.response.status, 401);
  assert.equal(f.calls.length, before);
  await f.request(
    { operation: "tree", server: "https://gitlab.com" },
    "https://app.test",
    sessionId,
  );
  assert.equal(f.calls.at(-1).headers.Authorization, undefined);
  await f.request(
    { operation: "logout", server: "https://gitlab.com" },
    "https://app.test",
    sessionId,
  );
  assert.ok(f.kv.has(`gitlab:${sessionId}`));
  await f.request(
    { operation: "logout", server: "https://gitlab.nps.edu" },
    "https://app.test",
    sessionId,
  );
  assert.equal(f.kv.has(`gitlab:${sessionId}`), false);
});

test("invalid server URLs are rejected before sending any credentials", async () => {
  for (const server of [
    "http://gitlab.nps.edu",
    "https://user:password@gitlab.nps.edu",
    "https://127.0.0.1",
    "https://[::1]",
    "https://169.254.169.254",
    "https://localhost",
    "https://gitlab.local",
    "https://gitlab.nps.edu?token=secret",
    "https://gitlab.nps.edu#token",
    "https://gitlab.nps.edu:8443",
    "file:///etc/passwd",
  ]) {
    const f = fixture();
    const result = await f.request({
      operation: "login",
      server,
      token: "company-token",
    });
    assert.equal(result.response.status, 400, server);
    assert.equal(f.calls.length, 0);
  }
});

test("redirects from a custom server never receive a second credential-bearing request", async () => {
  const f = fixture({ redirected: true });
  const result = await f.request({
    operation: "login",
    server: "https://gitlab.nps.edu",
    token: "company-token",
  });
  assert.equal(result.response.status, 502);
  assert.equal(f.calls.length, 1);
  assert.equal(new URL(f.calls[0].url).hostname, "gitlab.nps.edu");
  assert.equal(f.calls[0].redirect, "manual");
});

test("session metadata restores the company account without returning credentials", async () => {
  const f = fixture();
  f.kv.set(
    `gitlab:${f.id}`,
    JSON.stringify({
      token: "private-token",
      login: "shaun.rose",
      server: "https://gitlab.nps.edu",
      kind: "personal",
      expires: Date.now() + 100000,
    }),
  );
  const result = await f.request({ operation: "session" });
  assert.equal(result.body.user.login, "shaun.rose");
  assert.equal(result.body.user.server, "https://gitlab.nps.edu");
  assert.ok(!JSON.stringify(result.body).includes("private-token"));
  assert.equal(f.calls.length, 0);
  assert.equal(result.response.headers.get("Cache-Control"), "no-store");
});

test("older company sessions resolve their profile on their own host", async () => {
  const f = fixture();
  f.kv.set(
    `gitlab:${f.id}`,
    JSON.stringify({
      token: "company-token",
      server: "https://gitlab.nps.edu",
      expires: Date.now() + 100000,
    }),
  );
  const result = await f.request({ operation: "session" });
  assert.equal(result.body.user.login, "alice");
  assert.equal(f.calls[0].url, "https://gitlab.nps.edu/api/v4/user");
});

test("missing sessions report signed out", async () => {
  const f = fixture({ authenticated: false });
  assert.equal((await f.request({ operation: "session" })).body.user, null);
});

test("branch 404 identifies the selected server and project", async () => {
  const f = fixture({ missingProject: true });
  const result = await f.request({
    operation: "branches",
    server: "https://gitlab.nps.edu",
    owner: "fsg",
    repo: "sumo",
  });
  assert.equal(result.response.status, 404);
  assert.match(result.body.error, /fsg\/sumo/);
  assert.match(result.body.error, /gitlab.nps.edu/);
  assert.match(result.body.error, /token cannot access/);
});

test("successful GitLab token sign-in preserves the browser's GitHub session", async () => {
  const f = fixture();
  f.kv.set(
    "session:github-session",
    JSON.stringify({ access_token: "github-token", created_at: Date.now() }),
  );
  const handler = f.load(new URL("./gitlab.ts", import.meta.url));
  const result = await handler.onRequestPost({
    env: f.env,
    request: new Request("https://app.test/api/gitlab", {
      method: "POST",
      headers: {
        Origin: "https://app.test",
        "Content-Type": "application/json",
        Cookie: "session_id=github-session",
      },
      body: JSON.stringify({ operation: "login", token: "gitlab-token" }),
    }),
  });
  assert.equal(result.status, 200);
  assert.equal(f.kv.has("session:github-session"), true);
  assert.ok(
    result.headers.getSetCookie().every((c) => !c.startsWith("session_id=")),
  );
});

test("GitHub OAuth callback preserves the GitLab session", async () => {
  const f = fixture();
  f.env.GITHUB_OAUTH_CLIENT_ID = "client";
  f.env.GITHUB_OAUTH_CLIENT_SECRET = "secret";
  const callback = f.load(
    new URL("./github-auth-callback.ts", import.meta.url),
  );
  const result = await callback.onRequestGet({
    env: f.env,
    request: new Request(
      "https://app.test/api/github-auth-callback?state=abc&code=code",
      { headers: { Cookie: `oauth_state=abc; gitlab_session=${f.id}` } },
    ),
  });
  assert.equal(result.status, 302);
  assert.equal(f.kv.has(`gitlab:${f.id}`), true);
  assert.ok(
    result.headers
      .getSetCookie()
      .every((c) => !c.startsWith("gitlab_session=")),
  );
});

test("GitLab OAuth callback preserves the GitHub session", async () => {
  const f = fixture();
  f.kv.set(
    "session:github-id",
    JSON.stringify({ access_token: "github-token" }),
  );
  const callback = f.load(
    new URL("./gitlab-auth-callback.ts", import.meta.url),
  );
  const result = await callback.onRequestGet({
    env: f.env,
    request: new Request(
      "https://app.test/api/gitlab-auth-callback?state=state&code=code",
      {
        headers: {
          Cookie:
            "gitlab_state=state; gitlab_verifier=verifier; session_id=github-id",
        },
      },
    ),
  });
  assert.equal(result.status, 302);
  assert.equal(f.kv.has("session:github-id"), true);
  assert.ok(
    result.headers.getSetCookie().every((c) => !c.startsWith("session_id=")),
  );
});

test("failed GitLab token sign-in keeps the existing GitHub session", async () => {
  const f = fixture({ invalidToken: true });
  f.kv.set(
    "session:github-id",
    JSON.stringify({ access_token: "github-token" }),
  );
  const handler = f.load(new URL("./gitlab.ts", import.meta.url));
  const result = await handler.onRequestPost({
    env: f.env,
    request: new Request("https://app.test/api/gitlab", {
      method: "POST",
      headers: {
        Origin: "https://app.test",
        "Content-Type": "application/json",
        Cookie: "session_id=github-id",
      },
      body: JSON.stringify({ operation: "login", token: "invalid" }),
    }),
  });
  assert.equal(result.status, 401);
  assert.ok(f.kv.has("session:github-id"));
  assert.equal(result.headers.getSetCookie().length, 0);
});
