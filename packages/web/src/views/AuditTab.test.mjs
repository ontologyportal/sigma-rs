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
const composables = Object.fromEntries(
  ["useAuditSweep", "useAuditRun"].map((name) => [
    name,
    transpile(
      readFileSync(
        new URL(`../composables/${name}.ts`, import.meta.url),
        "utf8",
      ),
    ),
  ]),
);

function fixture({ noProof = false, importMissing = false } = {}) {
  const calls = [];
  const axiom = { file: "Merge.kif", line: 1, kif: "(p)" };
  const finding = {
    key: "000000000000000a+000000000000000b",
    culprits: [{ sid: "000000000000000a", ...axiom }],
    complete: true,
    session: null,
    origin: "url:nightly",
    found_at_ms: 1,
    contradiction: {
      steps: [axiom],
      graphviz: "",
      prose: "",
      prose_missing: [],
      proof_tptp_prologue: "",
    },
  };
  const recorded = {
    findings: [finding],
    focusKey: null,
    refresh: async () => {
      calls.push("refresh");
    },
    importJson: async (json, origin) => {
      calls.push({ cmd: "import", origin, json });
      return importMissing
        ? {
            imported: [],
            rejected: [
              {
                axioms: [axiom],
                reason: "1 cited axiom(s) are not in the knowledge base",
                missing: [axiom],
              },
            ],
          }
        : { imported: [finding], rejected: [] };
    },
  };
  const imports = {
    // Setup runs outside a component: no instance to provide into.
    vue: { ...vue, onActivated: () => {}, provide: () => {} },
    "vue-router": { onBeforeRouteLeave: () => {} },
    "../services/sigma": {
      isWasmAbort: () => false,
      call: async (cmd, args) => {
        calls.push({ cmd, args });
        return {
          result: {
            total: 1000,
            next_step: (args.request.step ?? 0) + 1,
            contradictions: noProof ? [] : [{ steps: [axiom] }],
            batches: [],
            raw_output: "",
          },
        };
      },
    },
    "../utils/format": {
      errMsg: (e) => e.message,
      fmtNum: String,
      fmtTime: (d) => d.toISOString(),
    },
    "../stores/boot": { useBootStore: () => ({}) },
    "../stores/kb": { useKBStore: () => ({ constituents: [] }) },
    "../stores/shell": {
      useShellStore: () => ({ isCompact: true, auditReportRequest: null }),
    },
    "../stores/contradictions": { useContradictionsStore: () => recorded },
    "../stores/prover": {
      useProverStore: () => ({
        adoptConfig: () => {
          calls.push("settings");
        },
        loadDefaults: async () => {},
        reset: (name) => {
          calls.push(`reset:${name}`);
        },
        config: (_name, overrides) => overrides,
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
      fmtSecs: (secs) => `${secs}s`,
    },
    "../utils/auditPlan": plan,
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
  // The view's composables run for real, against the same mocks; they
  // import from their own directory, so alias those specifiers too.
  imports["../services/proof-report"] = {
    runWithReport: imports["../services/sigma"].call,
  };
  imports["./useElapsed"] = imports["../composables/useElapsed"];
  const context = {
    require: (name) => imports[name] || {},
    localStorage: { getItem: () => null, setItem: () => {} },
    performance,
  };
  for (const name of ["useAuditSweep", "useAuditRun"]) {
    const exports = {};
    vm.runInNewContext(composables[name], { ...context, exports });
    imports[`../composables/${name}`] = exports;
  }
  const exports = {};
  vm.runInNewContext(source, { ...context, exports });
  const scope = vue.effectScope();
  const view = scope.run(() => exports.default.setup({}, { expose: () => {} }));
  return { view, calls, scope, finding, recorded };
}

test("replaying a recorded contradiction audits exactly its cited axioms", async () => {
  const f = fixture();
  try {
    await f.view.replay(f.finding);
    const audits = f.calls.filter((c) => c.cmd === "audit");
    assert.equal(audits.length, 1);
    assert.deepEqual([...audits[0].args.request.culprits], [f.finding.key]);
    assert.equal(audits[0].args.request.count, 1);
    assert.equal(audits[0].args.request.batch, 1);
    assert.equal(f.view.run.replayOf.key, f.finding.key);
    assert.equal(f.view.contradictions.value.length, 1);
    assert.match(
      f.view.positionText.value,
      /Replayed a recorded contradiction/,
    );
    assert.equal(f.view.auditing.value, false);
    // The engine recorded the result; the findings list is re-read.
    assert.ok(f.calls.includes("refresh"));
  } finally {
    f.scope.stop();
  }
});

test("a replay that finds nothing keeps the finding and says so", async () => {
  const f = fixture({ noProof: true });
  try {
    await f.view.replay(f.finding);
    assert.equal(f.view.contradictions.value.length, 0);
    assert.match(f.view.caveat.value, /stays recorded/);
  } finally {
    f.scope.stop();
  }
});

test("a shared audit report is recorded as a finding and replayed", async () => {
  const f = fixture();
  try {
    await f.view.importShared({
      problem: { command: "audit", request: { seed: 2, step: 42 } },
      config: { timeLimitSecs: 5 },
    });
    const imported = f.calls.find((c) => c.cmd === "import");
    assert.equal(imported.origin, "report");
    assert.match(imported.json, /"command":"audit"/);
    const audits = f.calls.filter((c) => c.cmd === "audit");
    assert.deepEqual([...audits[0].args.request.culprits], [f.finding.key]);
    assert.equal(f.view.error.value, "");
  } finally {
    f.scope.stop();
  }
});

test("a shared report whose axioms are missing is refused with the missing axioms", async () => {
  const f = fixture({ importMissing: true });
  try {
    await f.view.importShared({
      problem: { command: "audit", request: {} },
      config: {},
    });
    assert.equal(f.calls.filter((c) => c.cmd === "audit").length, 0);
    assert.match(f.view.error.value, /not in this knowledge base/);
    assert.match(f.view.error.value, /Merge\.kif:1/);
  } finally {
    f.scope.stop();
  }
});

test("a shared proof (not audit) report is ignored here", async () => {
  const f = fixture();
  try {
    await f.view.importShared({ problem: { command: "prove" }, config: {} });
    assert.deepEqual(f.calls, []);
  } finally {
    f.scope.stop();
  }
});

test("contradiction proofs fold individually and all at once", async () => {
  const f = fixture();
  try {
    const v = f.view;
    v.form.totalField.value = 0;
    v.form.roundsField.value = 1;
    await v.start(false);
    const [c] = v.contradictions.value;
    assert.ok(c);
    assert.equal(v.isCollapsed(c), false);
    v.setCollapsed(c, true);
    v.setCollapsed(c, true); // idempotent, as a programmatic toggle event is
    assert.equal(v.isCollapsed(c), true);
    assert.equal(v.allCollapsed.value, true);
    v.setAllCollapsed(false);
    assert.equal(v.isCollapsed(c), false);
    v.setAllCollapsed(true);
    assert.equal(v.allCollapsed.value, true);
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
    f.view.form.loadSweep(); // the fixture's storage has nothing for this KB
    assert.equal(f.view.sweep.step, 0);
    assert.equal(f.view.sweep.total, 0);
    assert.equal(f.view.sweep.seed, 0);
    assert.equal(f.view.sweep.scope, "");
  } finally {
    f.scope.stop();
  }
});

test("rounds drive the total; editing the total clears them", () => {
  const f = fixture();
  try {
    const v = f.view;
    v.form.perCheckField.value = 10;
    v.form.roundsField.value = 12;
    assert.equal(v.sweep.totalSecs, 120);
    v.form.batchField.value = 5; // more axioms per round, same rounds
    assert.equal(v.sweep.totalSecs, 120);
    v.form.perCheckField.value = 4;
    assert.equal(v.sweep.totalSecs, 48);
    v.form.totalField.value = 600;
    assert.equal(v.sweep.rounds, null);
    assert.equal(v.sweep.totalSecs, 600);
    v.form.roundsField.value = 3;
    v.presetTotal.value = "60";
    assert.equal(v.sweep.rounds, null);
  } finally {
    f.scope.stop();
  }
});

test("a sweep runs the set number of rounds of `batch` axioms each", async () => {
  const f = fixture();
  try {
    const v = f.view;
    v.form.perCheckField.value = 0;
    v.form.totalField.value = 0;
    v.form.roundsField.value = 3;
    v.form.batchField.value = 2;
    await v.start(false);
    const audits = f.calls.filter((c) => c.cmd === "audit");
    assert.equal(audits.length, 3);
    for (const c of audits) assert.equal(c.args.request.count, 2);
    assert.ok(f.calls.includes("refresh"));
  } finally {
    f.scope.stop();
  }
});

test("each contradiction is stamped with the time it was found", async () => {
  const f = fixture();
  try {
    const v = f.view;
    v.form.totalField.value = 0;
    v.form.roundsField.value = 1;
    const before = Date.now();
    await v.start(false);
    const [c] = v.contradictions.value;
    assert.ok(c.foundAt >= before && c.foundAt <= Date.now());
  } finally {
    f.scope.stop();
  }
});

test("switching to Simple drops the Advanced overrides", async () => {
  const f = fixture();
  try {
    const v = f.view;
    v.sweep.mode = "advanced";
    await vue.nextTick();
    Object.assign(v.sweep, { seed: 7, step: 40 });
    v.form.perCheckField.value = 3;
    v.form.roundsField.value = 4;
    v.form.batchField.value = 2;
    v.form.limitField.value = 9;
    assert.equal(v.sweep.linked, false);
    v.sweep.mode = "simple";
    await vue.nextTick();
    const fresh = plan.planAudit(v.sweep.totalSecs);
    assert.equal(v.sweep.linked, true);
    assert.equal(v.sweep.rounds, null);
    assert.equal(v.sweep.batch, fresh.batch);
    assert.equal(v.sweep.limit, fresh.limit);
    assert.equal(v.sweep.perCheckSecs, fresh.perCheckSecs);
    assert.ok(f.calls.includes("reset:audit"));
    assert.equal(v.sweep.seed, 7); // the sweep position is kept
    assert.equal(v.sweep.step, 40);
  } finally {
    f.scope.stop();
  }
});
