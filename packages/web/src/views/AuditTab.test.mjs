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
const plan = {};
vm.runInNewContext(
  transpile(
    readFileSync(new URL("../utils/auditPlan.ts", import.meta.url), "utf8"),
  ),
  { exports: plan },
);
const { descriptor } = parse(
  readFileSync(new URL("./AuditTab.vue", import.meta.url), "utf8"),
);
const source = transpile(
  compileScript(descriptor, { id: "audit-test" }).content,
);

function fixture({
  unavailable = "",
  noProof = false,
  invalid = "",
  recheckOutcome = "Consistent",
} = {}) {
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
    vue: { ...vue, onActivated: () => {} },
    "vue-router": { onBeforeRouteLeave: () => {} },
    "../services/sigma": {
      isWasmAbort: () => false,
      call: async (cmd, args) => {
        calls.push({ cmd, args });
        if (cmd === "prepareAuditRecheck" || cmd === "auditRecheckStatus")
          return { available: !invalid, reason: invalid, revision: 7 };
        if (cmd === "recheckAudit")
          return {
            result: {
              total: 1,
              next_step: 1,
              contradictions: [],
              batches: [
                {
                  status: recheckOutcome,
                  stop_reason:
                    recheckOutcome === "Timeout" ? "TimeLimit" : null,
                  focus: [],
                },
              ],
              raw_output: "",
            },
          };
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
        adoptConfig: () => {
          calls.push("settings");
        },
        backend: "native",
        backendLabel: "SUPr",
        vampireSelected: false,
      }),
    },
    "../composables/useTabQuery": {
      useTabQuery: () => ({ onQuery: () => {}, str: String }),
    },
    "../router": { updateParams: () => {} },
    "../composables/useElapsed": {
      useElapsed: () => ({ label: () => "", lastLabel: "" }),
    },
    "../utils/auditReplay": utils,
    "../utils/auditPlan": plan,
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
      summarizeBatches: (batches) => ({
        total: batches.length,
        clean: batches.filter((b) => b.status === "Consistent").length,
        contradictory: 0,
        timeLimit: batches.filter((b) => b.status === "Timeout").length,
        stepLimit: 0,
        crashed: 0,
        other: 0,
      }),
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

test("report dialog explains the nightly audit and stale-report restriction", () => {
  const template = descriptor.template.content.replace(/\s+/g, " ");
  assert.match(template, /SUMO runs a two-hour contradiction audit each night/);
  assert.match(template, /replay only those steps/);
  assert.match(template, /Reports cannot be loaded if master has changed/);
});

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

test("rechecking uses tracked targets without loading inputs or resetting settings again", async () => {
  const f = fixture();
  try {
    await f.view.showMasterReport();
    await f.view.confirmReplay();
    f.calls.length = 0;
    f.view.sweep.batch = 20;
    f.view.sweep.scope = "Other.kif";
    await f.view.recheckContradictions();
    assert.deepEqual(
      f.calls.map((c) => c.cmd),
      [
        "auditRecheckStatus",
        "recheckAudit",
        "recheckAudit",
        "auditRecheckStatus",
      ],
    );
    assert.deepEqual(
      f.calls.filter((c) => c.cmd === "recheckAudit").map((c) => c.args.index),
      [0, 1],
    );
    assert.match(
      f.view.replayMessage.value,
      /2 no longer reproduced a contradiction/,
    );
    assert.match(f.view.replayMessage.value, /does not prove/);
  } finally {
    f.scope.stop();
  }
});

test("invalid formula tracking prevents every recheck call", async () => {
  const f = fixture({ invalid: "Formulas were added" });
  try {
    await f.view.showMasterReport();
    await f.view.confirmReplay();
    f.calls.length = 0;
    await f.view.recheckContradictions();
    assert.deepEqual(
      f.calls.map((c) => c.cmd),
      ["auditRecheckStatus", "auditRecheckStatus"],
    );
    assert.match(f.view.recheckReason.value, /Formulas were added/);
  } finally {
    f.scope.stop();
  }
});

test("recheck timeouts are inconclusive, not repaired contradictions", async () => {
  const f = fixture({ recheckOutcome: "Timeout" });
  try {
    await f.view.showMasterReport();
    await f.view.confirmReplay();
    await f.view.recheckContradictions();
    assert.match(
      f.view.replayMessage.value,
      /0 no longer reproduced a contradiction, 2 inconclusive/,
    );
  } finally {
    f.scope.stop();
  }
});

test("contradiction proofs fold individually and all at once", async () => {
  const f = fixture();
  try {
    await f.view.showMasterReport();
    await f.view.confirmReplay();
    const [c] = f.view.contradictions.value;
    assert.ok(c);
    assert.equal(f.view.isCollapsed(c), false);
    f.view.setCollapsed(c, true);
    f.view.setCollapsed(c, true); // idempotent, as a programmatic toggle event is
    assert.equal(f.view.isCollapsed(c), true);
    assert.match(
      f.view.proofSummary(c),
      /^proof \(1 step\) · cites Merge\.kif:1/,
    );
    assert.equal(f.view.allCollapsed.value, true);
    assert.match(f.view.citedSummary(c), /Merge\.kif:1/);
    f.view.setAllCollapsed(false);
    assert.equal(f.view.isCollapsed(c), false);
    f.view.setAllCollapsed(true);
    assert.equal(f.view.allCollapsed.value, true);
  } finally {
    f.scope.stop();
  }
});

test("a finished sweep starts a new one instead of resuming at its end", async () => {
  const f = fixture();
  try {
    Object.assign(f.view.sweep, { seed: 5, step: 1000, total: 1000 });
    assert.equal(f.view.sweepDone.value, true);
    assert.equal(f.view.canContinue.value, false);
    f.view.start(false);
    assert.equal(f.view.sweep.step, 0);
    assert.notEqual(f.view.sweep.seed, 5);
    f.view.stopRequested.value = true;
  } finally {
    f.scope.stop();
  }
});

test("a KB with no saved sweep starts its own instead of inheriting one", () => {
  const f = fixture();
  try {
    Object.assign(f.view.sweep, {
      seed: 9,
      step: 400,
      total: 900,
      scope: "Old.kif",
    });
    f.view.loadSweep(); // the fixture's storage has nothing for this KB
    assert.equal(f.view.sweep.step, 0);
    assert.equal(f.view.sweep.total, 0);
    assert.equal(f.view.sweep.seed, 0);
    assert.equal(f.view.sweep.scope, "");
  } finally {
    f.scope.stop();
  }
});
