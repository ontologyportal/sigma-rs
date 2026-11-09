import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { createPinia, setActivePinia, defineStore } from "pinia";

const source = ts.transpileModule(
  readFileSync(new URL("./auth.ts", import.meta.url), "utf8"),
  {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  },
).outputText;

function fixture(
  whoami,
  fetch = async () => ({ ok: false }),
  gitlab = async () => ({ user: null }),
) {
  setActivePinia(createPinia());
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    require: (name) => {
      if (name === "pinia") return { defineStore };
      if (name === "../api/gitlab") return { gitlabRequest: gitlab };
      if (name === "../api/github") return { whoami };
      throw new Error(name);
    },
    fetch,
  });
  return exports.useAuthStore();
}

const profile = { login: "contributor", name: null, avatar_url: "avatar" };

test("valid token is trimmed and enables the existing contribution session", async () => {
  const store = fixture(async (token) => {
    assert.equal(token, "test-token");
    return profile;
  });
  store.openLoginDialog();
  await store.loginWithToken("  test-token  ");
  assert.equal(store.token, "test-token");
  assert.equal(store.user.login, "contributor");
  assert.equal(store.user.name, "contributor");
  assert.equal(store.user.avatarUrl, "avatar");
  assert.equal(store.githubSignedIn, true);
  assert.equal(store.loginDialogOpen, false);
  assert.equal(fixture().token, null);
});

test("blank token makes no request", async () => {
  const store = fixture(() => assert.fail("unexpected GitHub request"));
  await assert.rejects(
    store.loginWithToken("  "),
    /Enter a GitHub access token/,
  );
  assert.equal(store.githubSignedIn, false);
});

for (const message of ["Token rejected by GitHub", "Network unavailable"]) {
  test(message + " leaves authentication unchanged", async () => {
    const store = fixture(async () => {
      throw new Error(message);
    });
    store.openLoginDialog();
    await assert.rejects(store.loginWithToken("test-token"), { message });
    assert.equal(store.token, null);
    assert.equal(store.user, null);
    assert.equal(store.loginDialogOpen, true);
  });
}

test("closing the dialog during validation prevents a late sign-in", async () => {
  let resolve;
  const store = fixture(
    () =>
      new Promise((r) => {
        resolve = r;
      }),
  );
  const request = new AbortController();
  const pending = store.loginWithToken("test-token", request.signal);
  request.abort();
  resolve(profile);
  await assert.rejects(pending, { name: "AbortError" });
  assert.equal(store.githubSignedIn, false);
});

test("late OAuth initialization does not overwrite a personal token", async () => {
  let resolve;
  const store = fixture(
    async () => profile,
    () =>
      new Promise((r) => {
        resolve = r;
      }),
  );
  const pending = store.init();
  await store.loginWithToken("test-token");
  resolve({ ok: false });
  await pending;
  assert.equal(store.token, "test-token");
  assert.equal(store.githubSignedIn, true);
});

test("logout clears a personal token even without the local OAuth backend", async () => {
  const store = fixture(
    async () => profile,
    async () => {
      throw new Error("offline");
    },
  );
  await store.loginWithToken("test-token");
  await store.logout();
  assert.equal(store.token, null);
  assert.equal(store.user, null);
  assert.equal(store.githubSignedIn, false);
});

test("GitLab session restores after a reload without granting GitHub authentication", async () => {
  const store = fixture(undefined, undefined, async (operation) => {
    assert.equal(operation, "session");
    return { user: { login: "shaun.rose", server: "https://gitlab.nps.edu" } };
  });
  await store.init();
  assert.equal(store.gitlabUser.login, "shaun.rose");
  assert.equal(store.gitlabUser.server, "https://gitlab.nps.edu");
  assert.equal(store.user, null);
  assert.equal(store.token, null);
  assert.equal(store.githubSignedIn, false);
  assert.equal(store.accountName, "");
});

test("late session restoration cannot erase a fresh GitLab sign-in", async () => {
  let resolve;
  const store = fixture(
    undefined,
    undefined,
    () =>
      new Promise((r) => {
        resolve = r;
      }),
  );
  const pending = store.initGitlab();
  store.gitlabUser = { login: "new-login", server: "https://gitlab.nps.edu" };
  resolve({ user: null });
  await pending;
  assert.equal(store.gitlabUser.login, "new-login");
});

test("expired GitLab sessions clear the displayed profile", async () => {
  const store = fixture();
  store.gitlabUser = { login: "alice", server: "https://gitlab.nps.edu" };
  await store.initGitlab();
  assert.equal(store.gitlabUser, null);
});

test("GitHub token sign-in and sign-out preserve the GitLab connection", async () => {
  const calls = [];
  const store = fixture(
    async () => profile,
    undefined,
    async (op) => {
      calls.push(op);
      return {};
    },
  );
  store.gitlabUser = { login: "company", server: "https://gitlab.example.org" };
  await store.loginWithToken("github-token");
  assert.equal(store.accountName, "contributor");
  await store.logout();
  assert.deepEqual(calls, []);
  assert.equal(store.accountName, "");
  assert.equal(store.gitlabUser.login, "company");
});

test("failed GitHub validation preserves the current GitLab account", async () => {
  const store = fixture(async () => {
    throw new Error("bad token");
  });
  store.gitlabUser = { login: "company", server: "https://gitlab.example.org" };
  await assert.rejects(store.loginWithToken("bad"), /bad token/);
  assert.equal(store.gitlabUser.login, "company");
  assert.equal(store.githubSignedIn, false);
});

test("GitLab sign-in and sign-out preserve the GitHub token", async () => {
  let reject = true;
  const store = fixture(
    async () => profile,
    undefined,
    async (operation) => {
      if (operation === "logout") return {};
      if (reject) throw new Error("bad token");
      return { login: "company", server: "https://gitlab.example.org" };
    },
  );
  await store.loginWithToken("github-token");
  await assert.rejects(
    store.loginWithGitlabToken("https://gitlab.example.org", "bad"),
    /bad token/,
  );
  assert.equal(store.gitlabUser, null);
  reject = false;
  await store.loginWithGitlabToken("https://gitlab.example.org", "valid");
  assert.equal(store.gitlabUser.login, "company");
  await store.logoutGitlab();
  assert.equal(store.gitlabUser, null);
  assert.equal(store.token, "github-token");
  assert.equal(store.user.login, "contributor");
});

test("both sessions restore on startup with the GitHub account in the header", async () => {
  const calls = [];
  const store = fixture(
    undefined,
    async () => ({
      ok: true,
      json: async () => ({
        name: "GitHub user",
        login: "github",
        token: "github-token",
      }),
    }),
    async (op) => {
      calls.push(op);
      return {
        user: { login: "company", server: "https://gitlab.example.org" },
      };
    },
  );
  await store.init();
  assert.equal(store.accountName, "GitHub user");
  assert.equal(store.gitlabUser.login, "company");
  assert.deepEqual(calls, ["session"]);
});
