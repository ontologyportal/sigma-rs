import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { mock, test } from "node:test";
import vm from "node:vm";
import ts from "typescript";
import * as vue from "vue";

const exports = {};
vm.runInNewContext(
  ts.transpileModule(
    readFileSync(new URL("./useElapsed.ts", import.meta.url), "utf8"),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    },
  ).outputText,
  {
    exports,
    // No component instance here, so the unmount hook is a no-op.
    require: () => ({ ...vue, onBeforeUnmount: () => {} }),
    performance,
    setInterval,
    clearInterval,
  },
);
const { fmtSecs, useElapsed } = exports;

test("fmtSecs formats seconds and minutes", () => {
  assert.equal(fmtSecs(0), "0s");
  assert.equal(fmtSecs(12.9), "12s");
  assert.equal(fmtSecs(59.99), "59s");
  assert.equal(fmtSecs(60), "1m 00s");
  assert.equal(fmtSecs(185), "3m 05s");
});

test("label shows the limit only when one is set", () => {
  const { label } = useElapsed(vue.ref(false));
  assert.equal(label(30), "0s / 30s");
  assert.equal(label(0), "0s");
});

test("fraction is the share of the limit used, capped at 1", async () => {
  let now = 0;
  const perf = mock.method(performance, "now", () => now);
  try {
    const running = vue.ref(false);
    const { fraction } = useElapsed(running);
    assert.equal(fraction(0), null);
    assert.equal(fraction(30), 0);
    running.value = true;
    await vue.nextTick();
    now = 15000;
    await new Promise((r) => setTimeout(r, 300));
    assert.equal(fraction(30), 0.5);
    now = 45000;
    await new Promise((r) => setTimeout(r, 300));
    assert.equal(fraction(30), 1);
    running.value = false;
    await vue.nextTick();
  } finally {
    perf.mock.restore();
  }
});

test("counts while running and records the finished run", async () => {
  let now = 1000;
  const perf = mock.method(performance, "now", () => now);
  try {
    const running = vue.ref(false);
    const { seconds, lastSecs, lastLabel } = useElapsed(running);
    assert.equal(lastSecs.value, null);
    assert.equal(lastLabel.value, "");

    running.value = true;
    await vue.nextTick();
    now += 2500;
    await new Promise((r) => setTimeout(r, 300));
    assert.equal(seconds.value, 2.5);

    now += 1000;
    running.value = false;
    await vue.nextTick();
    assert.equal(lastSecs.value, 3.5);
    assert.equal(lastLabel.value, "3.5s");

    // A second run restarts the live counter from zero.
    running.value = true;
    await vue.nextTick();
    assert.equal(seconds.value, 0);
    running.value = false;
    await vue.nextTick();
  } finally {
    perf.mock.restore();
  }
});
