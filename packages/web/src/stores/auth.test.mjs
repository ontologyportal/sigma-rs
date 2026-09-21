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

function fixture(whoami, fetch = async () => ({ ok: false })) {
  setActivePinia(createPinia());
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    require: (name) => {
      if (name === "pinia") return { defineStore };
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
  assert.equal(store.signedIn, true);
  assert.equal(store.loginDialogOpen, false);
  assert.equal(fixture().token, null);
});

test("blank token makes no request", async () => {
  const store = fixture(() => assert.fail("unexpected GitHub request"));
  await assert.rejects(
    store.loginWithToken("  "),
    /Enter a GitHub access token/,
  );
  assert.equal(store.signedIn, false);
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
  assert.equal(store.signedIn, false);
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
  assert.equal(store.signedIn, true);
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
  assert.equal(store.signedIn, false);
});
