import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";

const exports = {};
vm.runInNewContext(
  ts.transpileModule(
    readFileSync(new URL("./auditPlan.ts", import.meta.url), "utf8"),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    },
  ).outputText,
  { exports },
);
const { planAudit, minChecks, nextCheckSecs, focusRoundSecs } = exports;

test("planAudit gives a third of the total per check, 2 to 10 s", () => {
  assert.equal(planAudit(60).perCheckSecs, 10);
  assert.equal(planAudit(3600).perCheckSecs, 10);
  assert.equal(planAudit(12).perCheckSecs, 4);
  assert.equal(planAudit(3).perCheckSecs, 2);
});

test("planAudit runs to the deadline, one axiom per check", () => {
  const p = planAudit(300);
  assert.equal(p.totalSecs, 300);
  assert.equal(p.count, null);
  assert.equal(p.batch, 1);
  assert.equal(p.limit, 5);
});

test("planAudit treats junk and zero as no deadline", () => {
  assert.equal(planAudit(0).totalSecs, 0);
  assert.equal(planAudit(-5).totalSecs, 0);
  assert.equal(planAudit("abc").totalSecs, 0);
  assert.equal(planAudit(0).perCheckSecs, 10);
});

test("minChecks is the worst-case check count", () => {
  assert.equal(minChecks(planAudit(60)), 6);
  assert.equal(minChecks(planAudit(5)), 2);
  assert.equal(minChecks({ ...planAudit(600), count: 3 }), 3);
  assert.equal(minChecks(planAudit(0)), null);
});

test("nextCheckSecs trims the last check to the deadline", () => {
  assert.equal(nextCheckSecs(10, null), 10);
  assert.equal(nextCheckSecs(10, 60_000), 10);
  assert.equal(nextCheckSecs(10, 4_500), 4);
  assert.equal(nextCheckSecs(10, 900), 0);
  assert.equal(nextCheckSecs(0, 30_000), 30);
});

test("focusRoundSecs splits the remainder over the rounds left", () => {
  assert.equal(focusRoundSecs(60_000, 3), 20);
  assert.equal(focusRoundSecs(2_000, 3), 1);
  assert.equal(focusRoundSecs(500, 3), 0);
  assert.equal(focusRoundSecs(60_000, 0), 0);
});
