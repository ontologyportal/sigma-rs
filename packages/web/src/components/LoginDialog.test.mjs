import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import { parse } from "@vue/compiler-sfc";
import ts from "typescript";

const { descriptor } = parse(
  readFileSync(new URL("./LoginDialog.vue", import.meta.url), "utf8"),
);
const source = ts.transpileModule(
  descriptor.scriptSetup.content +
    "\nexports.controls = { token, busy, error, useToken, chooseRepository };",
  {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  },
).outputText;

function fixture(loginWithToken = async () => {}) {
  const exports = {};
  let reset;
  const navigation = [];
  const auth = { loginWithToken };
  const imports = {
    vue: {
      ref: (value) => ({ value }),
      watch: (_, callback) => {
        reset = callback;
      },
      onBeforeUnmount() {},
    },
    "../stores/auth": { useAuthStore: () => auth },
    "../router": { navigate: (...args) => navigation.push(args) },
    "../utils/format": { errMsg: (e) => e.message },
  };
  vm.runInNewContext(source, {
    exports,
    require: (name) => imports[name],
    AbortController,
  });
  return { ...exports.controls, auth, navigation, reset: () => reset() };
}

test("closing the dialog clears the token and cancels pending validation", async () => {
  let signal;
  let complete;
  const f = fixture((_, pendingSignal) => {
    signal = pendingSignal;
    return new Promise((resolve) => {
      complete = resolve;
    });
  });
  f.token.value = "test-token";
  const pending = f.useToken();
  assert.equal(f.busy.value, true);
  f.reset();
  assert.equal(signal.aborted, true);
  assert.equal(f.token.value, "");
  assert.equal(f.busy.value, false);
  complete();
  await pending;
});

test("validation errors leave the form open for correction", async () => {
  const f = fixture(async () => {
    throw new Error("Token rejected");
  });
  f.token.value = "test-token";
  await f.useToken();
  assert.equal(f.error.value, "Token rejected");
  assert.equal(f.busy.value, false);
});

test("choose repository leaves sign-in and opens the GitHub selector even when GitLab is connected", () => {
  const f = fixture();
  f.auth.gitlabUser = {
    login: "company-user",
    server: "https://gitlab.nps.edu",
  };
  f.auth.loginDialogOpen = true;
  f.chooseRepository();
  assert.equal(f.auth.loginDialogOpen, false);
  assert.equal(f.navigation[0][0], "kb");
  assert.equal(f.navigation[0][1].chooseRepo, "github");
  assert.equal(f.navigation[0][1].server, undefined);
});
