import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const source = ts.transpileModule(
  readFileSync(new URL("./external-prover-bridge.ts", import.meta.url), "utf8"),
  {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  },
).outputText;

function bridge(isolated = true) {
  const restarts = [];
  const context = {
    exports: {},
    SharedArrayBuffer: isolated ? SharedArrayBuffer : undefined,
    Int32Array,
    Uint8Array,
    TextDecoder,
    Atomics,
    Date,
    self: { postMessage: (message) => restarts.push(message) },
  };
  vm.runInNewContext(source, context);
  return { context, restarts, install: context.exports.installVampireBridge };
}

function responder(result, messages = [], options = {}) {
  let ctrl;
  const bytes = new TextEncoder().encode(JSON.stringify(result));
  return {
    postMessage(message) {
      messages.push(message);
      if (message.type === "run") {
        ctrl = new Int32Array(message.ctrl);
        if (options.stallRun) return;
        Atomics.store(ctrl, 1, options.length ?? bytes.length);
        Atomics.store(ctrl, 0, 1);
      } else if (!options.stallBuffer) {
        new Uint8Array(message.data).set(bytes);
        Atomics.store(ctrl, 0, 2);
      }
    },
  };
}

test("E and its filter use their own port without affecting Vampire", () => {
  const { context, install, restarts } = bridge();
  const eMessages = [],
    vampireMessages = [];
  install(responder({ stdout: "E proof" }, eMessages), "e");
  install(responder({ stdout: "Vampire proof" }, vampireMessages), "vampire");
  assert.equal(
    context.__sigmaRunEproverSync("problem", "[]", 100).stdout,
    "E proof",
  );
  context.__sigmaRunAxfilterSync("problem", "{}", 100);
  assert.equal(
    context.__sigmaRunVampireSync("problem", "", 100).stdout,
    "Vampire proof",
  );
  assert.deepEqual(
    eMessages.filter((m) => m.type === "run").map((m) => m.program),
    ["eprover", "e_axfilter"],
  );
  assert.equal(vampireMessages[0].program, "vampire");
  assert.equal(restarts.length, 0);
});

for (const phase of ["stallRun", "stallBuffer"]) {
  test(
    "E deadline during " +
      phase +
      " requests only E restart and accepts a replacement",
    () => {
      const { context, install, restarts } = bridge();
      install(responder({}, [], { [phase]: true }), "e");
      install(responder({ stdout: "Vampire proof" }), "vampire");
      const result = context.__sigmaRunEproverSync("problem", "[]", 2);
      assert.match(result.stdout, /SZS status Timeout/);
      assert.equal(restarts.length, 1);
      assert.equal(restarts[0].backend, "e");
      assert.throws(
        () => context.__sigmaRunEproverSync("", "[]", 1),
        /restarting/,
      );
      assert.equal(
        context.__sigmaRunVampireSync("", "", 100).stdout,
        "Vampire proof",
      );
      install(responder({ stdout: "recovered" }), "e");
      assert.equal(
        context.__sigmaRunEproverSync("", "[]", 100).stdout,
        "recovered",
      );
    },
  );
}

test("missing assets and oversized output trigger recovery", () => {
  const { context, install, restarts } = bridge();
  install(responder({ error: "E WASM assets could not be loaded" }), "e");
  assert.throws(() => context.__sigmaRunEproverSync("", "[]", 100), /assets/);
  install(responder({}, [], { length: 256 * 1024 * 1024 + 1 }), "e");
  assert.throws(
    () => context.__sigmaRunEproverSync("", "[]", 100),
    /bridge limit/,
  );
  assert.equal(restarts.length, 2);
});

test("non-isolated pages get a clear prerequisite error", () => {
  const { context, install } = bridge(false);
  install(responder({}), "e");
  assert.throws(
    () => context.__sigmaRunEproverSync("", "[]", 100),
    /COOP\/COEP/,
  );
});
