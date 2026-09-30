import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import { parse, compileScript } from "@vue/compiler-sfc";
import * as vue from "vue";
import ts from "typescript";

function transpile(source) {
  return ts.transpileModule(source, {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  }).outputText;
}
const utils = {};
vm.runInNewContext(
  transpile(
    readFileSync(new URL("../utils/auditReplay.ts", import.meta.url), "utf8"),
  ),
  { exports: utils },
);
const { descriptor } = parse(
  readFileSync(new URL("./AuditTab.vue", import.meta.url), "utf8"),
);
const source = transpile(
  compileScript(descriptor, { id: "audit-test" }).content,
);

function fixture({ unavailable = "", noProof = false } = {}) {
  const calls = [];
  const axiom = { file: "Merge.kif", line: 1, kif: "(p)" };
  const replay = {
    config: {
      backend: "native",
      maxSteps: 500000,
      maxLits: 12,
      timeLimitSecs: 10,
    },
    request: { count: 1, batch: 1, limit: 64 },
    findings: [
      { seed: 2, step: 42, axioms: [axiom] },
      { seed: 2, step: 900, axioms: [axiom] },
    ],
  };
  const imports = {
    vue,
    "vue-router": { onBeforeRouteLeave: () => {} },
    "../services/sigma": {
      isWasmAbort: () => false,
      call: async (cmd, args) => {
        calls.push({ cmd, args });
        return {
          result: {
            total: 1000,
            next_step: args.request.step + 1,
            contradictions: noProof ? [] : [{ steps: [axiom] }],
            batches: [],
            raw_output: "",
          },
        };
      },
    },
    "../utils/format": { errMsg: (e) => e.message },
    "../stores/boot": { useBootStore: () => ({}) },
    "../stores/kb": { useKBStore: () => ({ constituents: [] }) },
    "../stores/prover": {
      useProverStore: () => ({
        reset: () => {
          calls.push("settings");
        },
        cfg: {},
        backend: "native",
        vampireSelected: false,
      }),
    },
    "../composables/useElapsed": {
      useElapsed: () => ({ label: () => "", lastLabel: "" }),
    },
    "../utils/auditReplay": utils,
    "../services/audit-replay": {
      latestAuditReport: async () => ({
        replay,
        unavailable,
        markdown: "# Report",
      }),
      loadAuditReplay: async () => {
        calls.push("replace");
      },
      checkAuditReplay: async () => {
        calls.push("check");
      },
    },
    "../utils/contradictionReport": {
      contradictionKey: () => "proof",
      summarizeBatches: () => ({}),
    },
  };
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    require: (name) => imports[name] || {},
    localStorage: { getItem: () => null, setItem: () => {} },
    performance,
  });
  const scope = vue.effectScope();
  const view = scope.run(() => exports.default.setup({}, { expose: () => {} }));
  return { view, calls, scope, replay };
}

test("opening and dismissing the report never loads or audits", async () => {
  const f = fixture();
  try {
    await f.view.showMasterReport();
    assert.equal(f.view.reportOpen.value, true);
    f.view.reportOpen.value = false;
    assert.deepEqual(f.calls, []);
  } finally {
    f.scope.stop();
  }
});

test("confirmation syncs then audits only the reported positions with workflow settings", async () => {
  const f = fixture();
  try {
    await f.view.showMasterReport();
    await f.view.confirmReplay();
    assert.equal(f.calls[0], "replace");
    const audits = f.calls.filter((c) => c.cmd === "audit");
    assert.deepEqual(
      audits.map((c) => c.args.request.step),
      [42, 900],
    );
    for (const c of audits) {
      assert.equal(c.args.request.seed, 2);
      assert.equal(c.args.request.count, 1);
      assert.equal(c.args.request.batch, 1);
      assert.equal(c.args.request.limit, 64);
      assert.equal(c.args.config.maxSteps, 500000);
    }
    assert.match(f.view.replayMessage.value, /Reproduced all 1/);
    assert.equal(f.view.replayBusy.value, false);
  } finally {
    f.scope.stop();
  }
});

test("stale reports cannot be confirmed", async () => {
  const f = fixture({ unavailable: "master changed" });
  try {
    await f.view.showMasterReport();
    await f.view.confirmReplay();
    assert.deepEqual(f.calls, []);
  } finally {
    f.scope.stop();
  }
});

test("missing contradictions are reported as a mismatch, never success", async () => {
  const f = fixture({ noProof: true });
  try {
    await f.view.showMasterReport();
    await f.view.confirmReplay();
    assert.match(
      f.view.replayMessage.value,
      /1 reported contradiction\(s\) missing/,
    );
  } finally {
    f.scope.stop();
  }
});
