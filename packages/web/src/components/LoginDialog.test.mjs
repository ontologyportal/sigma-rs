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
    "\nexports.controls = { tokenOpen, tokenInput, token, busy, error, toggleToken, useToken };",
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
  const imports = {
    vue: {
      ref: (value) => ({ value }),
      nextTick: async () => {},
      watch: (_, callback) => {
        reset = callback;
      },
      onBeforeUnmount() {},
    },
    "../stores/auth": { useAuthStore: () => ({ loginWithToken }) },
    "../utils/format": { errMsg: (e) => e.message },
  };
  vm.runInNewContext(source, {
    exports,
    require: (name) => imports[name],
    AbortController,
  });
  return { ...exports.controls, reset: () => reset() };
}

test("token form starts collapsed and focuses the input when expanded", async () => {
  const f = fixture();
  let focused = 0;
  f.tokenInput.value = { focus: () => focused++ };
  assert.equal(f.tokenOpen.value, false);
  await f.toggleToken();
  assert.equal(f.tokenOpen.value, true);
  assert.equal(focused, 1);
  await f.toggleToken();
  assert.equal(f.tokenOpen.value, false);
  assert.equal(focused, 1);
});

test("closing the dialog collapses the form and cancels pending validation", async () => {
  let signal;
  let complete;
  const f = fixture((_, pendingSignal) => {
    signal = pendingSignal;
    return new Promise((resolve) => {
      complete = resolve;
    });
  });
  await f.toggleToken();
  f.token.value = "test-token";
  const pending = f.useToken();
  assert.equal(f.busy.value, true);
  f.reset();
  assert.equal(signal.aborted, true);
  assert.equal(f.tokenOpen.value, false);
  assert.equal(f.token.value, "");
  assert.equal(f.busy.value, false);
  complete();
  await pending;
});

test("validation errors leave the form open for correction", async () => {
  const f = fixture(async () => {
    throw new Error("Token rejected");
  });
  await f.toggleToken();
  f.token.value = "test-token";
  await f.useToken();
  assert.equal(f.tokenOpen.value, true);
  assert.equal(f.error.value, "Token rejected");
  assert.equal(f.busy.value, false);
});
