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
    "\nexports.controls = { token, busy, error, selectProvider, useToken, moveTab, providerTabs, gitlabBusy, chooseRepository };",
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
  const auth = { loginWithToken, loginProvider: "github" };
  const imports = {
    vue: {
      ref: (value) => ({ value }),
      nextTick: async () => {},
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

test("provider tabs clear credentials and cannot switch during sign-in", () => {
  const f = fixture();
  f.token.value = "secret";
  f.error.value = "old error";
  f.selectProvider("gitlab");
  assert.equal(f.auth.loginProvider, "gitlab");
  assert.equal(f.token.value, "");
  assert.equal(f.error.value, "");
  f.gitlabBusy.value = true;
  f.selectProvider("github");
  assert.equal(f.auth.loginProvider, "gitlab");
});
test("keyboard navigation selects and focuses the next provider tab", async () => {
  const f = fixture();
  let focused = false;
  f.providerTabs.value = {
    querySelector: () => ({
      focus: () => {
        focused = true;
      },
    }),
  };
  await f.moveTab({ key: "ArrowRight", preventDefault() {} });
  assert.equal(f.auth.loginProvider, "gitlab");
  assert.equal(focused, true);
  await f.moveTab({ key: "Home", preventDefault() {} });
  assert.equal(f.auth.loginProvider, "github");
});

test("choose repository leaves sign-in and opens the connected provider's selector", () => {
  const f = fixture();
  f.auth.provider = "gitlab";
  f.auth.gitlabUser = {
    login: "company-user",
    server: "https://gitlab.nps.edu",
  };
  f.auth.loginDialogOpen = true;
  f.chooseRepository();
  assert.equal(f.auth.loginDialogOpen, false);
  assert.equal(f.navigation[0][0], "kb");
  assert.equal(f.navigation[0][1].chooseRepo, "gitlab");
  assert.equal(f.navigation[0][1].server, "https://gitlab.nps.edu");
});
