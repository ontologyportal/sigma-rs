import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";

const exports = {};
vm.runInNewContext(
  ts.transpileModule(
    readFileSync(new URL("./proverOptions.ts", import.meta.url), "utf8"),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    },
  ).outputText,
  { exports },
);
const {
  defaultProfile,
  normalizeProfile,
  toWireConfig,
  profileChanges,
  resetChange,
  strategyProblems,
  strategyDiff,
  profileFromWire,
} = exports;

const PRESETS = {
  "goal-directed": {
    name: "goal-directed",
    goal_dist: true,
    bg_snapshot: false,
  },
};
const wire = (p, backend = "native") =>
  toWireConfig(p, {
    backend,
    vampireArgs: "  --avatar off ",
    presets: PRESETS,
  });
// Wire objects come from another realm (vm); compare as plain JSON.
const plain = (v) => JSON.parse(JSON.stringify(v));

test("a default profile sends no selection or strategy", () => {
  const w = wire(defaultProfile());
  assert.equal(w.selection, undefined);
  assert.equal(w.selectionTolerancePct, undefined);
  assert.equal(w.strategy, undefined);
  assert.equal(w.timeLimitSecs, 30);
  assert.equal(w.vampireArgs, "--avatar off");
});

test("budget modes map onto selection params", () => {
  const p = defaultProfile();
  p.selection.mode = "pct";
  p.selection.pct = 150;
  assert.equal(wire(p).selectionTolerancePct, 99);
  p.selection.mode = "axioms";
  p.selection.axioms = 500;
  assert.deepEqual(plain(wire(p).selection), { auto_budget: 500 });
  p.selection.mode = "tolerance";
  p.selection.tolerance = 2;
  assert.deepEqual(plain(wire(p).selection), {
    auto_budget: null,
    tolerance: 2,
  });
  p.selection.mode = "all";
  p.selection.depth = 3;
  p.selection.autoscale = false;
  assert.deepEqual(plain(wire(p).selection), {
    select_all: true,
    depth_limit: 3,
    autoscale: false,
  });
});

test("strategy = preset, then overrides, then selection repair", () => {
  const p = defaultProfile();
  p.preset = "goal-directed";
  p.strategy = { goal_dist: false, pick_ratio: 3 };
  p.selection.liuRescue = false;
  assert.deepEqual(plain(wire(p).strategy), {
    name: "goal-directed",
    goal_dist: false,
    bg_snapshot: false,
    pick_ratio: 3,
    liu_rescue: false,
  });
});

test("vampire gets no strategy", () => {
  const p = defaultProfile();
  p.strategy = { pick_ratio: 3 };
  assert.equal(wire(p, "vampire").strategy, undefined);
});

test("an unloaded preset throws instead of running the default", () => {
  const p = defaultProfile();
  p.preset = "missing";
  assert.throws(() => wire(p), /unknown strategy preset/);
});

test("per-run overrides win and are coerced", () => {
  const p = defaultProfile();
  const w = toWireConfig(p, {
    backend: "native",
    vampireArgs: "",
    presets: {},
    overrides: { timeLimitSecs: 7.9, maxSteps: -1 },
  });
  assert.equal(w.timeLimitSecs, 7);
  assert.equal(w.maxSteps, 4000);
});

test("changes list only what differs, backend-aware, and reset clears them", () => {
  const d = defaultProfile();
  const p = defaultProfile();
  p.maxSteps = 100;
  p.timeLimitSecs = 5;
  p.strategy = { pick_ratio: 3 };
  p.selection.mode = "all";
  const ids = (b) => plain(profileChanges(p, d, b).map((c) => c.id));
  assert.deepEqual(ids("native"), [
    "time",
    "steps",
    "budget",
    "strategy:pick_ratio",
  ]);
  assert.deepEqual(ids("vampire"), ["time", "budget"]);
  for (const id of ids("native")) resetChange(p, d, id);
  assert.deepEqual(plain(profileChanges(p, d, "native")), []);
});

test("normalizeProfile fills missing and ill-typed fields", () => {
  const base = defaultProfile(10);
  const p = normalizeProfile(
    {
      maxSteps: "x",
      wantProof: false,
      selection: { mode: "bogus", depth: 4 },
      strategy: { a: 1 },
    },
    base,
  );
  assert.equal(p.maxSteps, 4000);
  assert.equal(p.timeLimitSecs, 10);
  assert.equal(p.wantProof, false);
  assert.equal(p.selection.mode, "default");
  assert.equal(p.selection.depth, 4);
  assert.deepEqual(plain(p.strategy), { a: 1 });
  assert.deepEqual(plain(normalizeProfile(null, base)), plain(base));
});

test("strategyProblems flags unknown keys and wrong types", () => {
  const base = {
    pick_ratio: 5,
    demod: false,
    derived_width_cap: null,
    tier_weight: [1, 2, 2],
  };
  assert.deepEqual(
    plain(strategyProblems({ pick_ratio: 3, derived_width_cap: 12 }, base)),
    [],
  );
  assert.deepEqual(
    plain(strategyProblems({ derived_width_cap: null }, base)),
    [],
  );
  assert.equal(strategyProblems({ bogus: 1 }, base).length, 1);
  assert.equal(strategyProblems({ demod: 1 }, base).length, 1);
  assert.equal(strategyProblems({ pick_ratio: -1 }, base).length, 1);
  assert.equal(strategyProblems({ pick_ratio: 1.5 }, base).length, 1);
  assert.equal(strategyProblems([], base).length, 1);
  assert.equal(strategyProblems("x", base).length, 1);
});

test("strategyDiff keeps only changed keys", () => {
  const base = { name: "default", pick_ratio: 5, tier_weight: [1, 2, 2] };
  assert.deepEqual(
    plain(
      strategyDiff(
        { name: "mine", pick_ratio: 5, tier_weight: [1, 1, 4] },
        base,
      ),
    ),
    { tier_weight: [1, 1, 4] },
  );
});

test("E carries its audit subset settings; other backends don't", () => {
  const p = defaultProfile();
  p.auditAxfilter = true;
  p.auditSubsetLimit = 0;
  const e = wire(p, "e");
  assert.equal(e.auditAxfilter, true);
  assert.equal(e.auditSubsetLimit, 1);
  assert.equal(e.selectionTimeLimitSecs, 10);
  assert.equal(e.strategy, undefined);
  assert.equal(wire(p, "native").auditAxfilter, undefined);
  const d = defaultProfile();
  assert.deepEqual(plain(profileChanges(p, d, "e").map((c) => c.id)), [
    "axfilter",
  ]);
  assert.deepEqual(plain(profileChanges(p, d, "native")), []);
});

test("profileFromWire maps a saved config back onto a profile", () => {
  const base = defaultProfile(10);
  const p = profileFromWire(
    {
      backend: "native",
      maxSteps: 500000,
      maxLits: 12,
      timeLimitSecs: 20,
      selectionBudget: 300,
      strategy: { name: "x", pick_ratio: 3 },
    },
    base,
  );
  assert.equal(p.maxSteps, 500000);
  assert.equal(p.maxLits, 12);
  assert.equal(p.timeLimitSecs, 20);
  assert.equal(p.selection.mode, "axioms");
  assert.equal(p.selection.axioms, 300);
  assert.equal(p.selection.autoscale, false);
  assert.deepEqual(plain(p.strategy), { pick_ratio: 3 });
  // Round trip: the profile sends the same budget back, autoscale off.
  assert.deepEqual(plain(wire(p).selection), {
    auto_budget: 300,
    autoscale: false,
  });
  const pct = profileFromWire({ selectionTolerancePct: 25 }, base);
  assert.equal(pct.selection.mode, "pct");
  assert.equal(pct.selection.pct, 25);
  assert.equal(profileFromWire({}, base).selection.mode, "default");
});
