import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";

const format = {};
vm.runInNewContext(
  ts.transpileModule(
    readFileSync(new URL("./format.ts", import.meta.url), "utf8"),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    },
  ).outputText,
  { exports: format, Intl },
);

test("fmtCount keeps counts under 1,000 exact", () => {
  for (const n of [0, 7, 42, 999]) assert.equal(format.fmtCount(n), String(n));
});

test("fmtCount never exceeds four characters", () => {
  for (const n of [1000, 1234, 9999, 12068, 99999, 123456, 1234567, 9.9e8])
    assert.ok(format.fmtCount(n).length <= 4, `${n} -> ${format.fmtCount(n)}`);
});

test("fmtCount abbreviates large counts", () => {
  assert.notEqual(format.fmtCount(12068), "12068");
  assert.notEqual(format.fmtCount(12068), "12,068");
});
