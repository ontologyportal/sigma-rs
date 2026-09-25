/** Real workers and WASM, with a small isolated KB and the shared settings UI. */
import assert from "node:assert/strict";
import { chromium } from "playwright";
import { createServer } from "vite";
import { fileURLToPath } from "node:url";
const server = await createServer({
  root: fileURLToPath(new URL("../", import.meta.url)),
  logLevel: "error",
});
await server.listen();
const browser = await chromium.launch({
  executablePath: process.env.PLAYWRIGHT_CHROMIUM || undefined,
});
try {
  const page = await browser.newPage();
  page.setDefaultTimeout(60000);
  await page.goto(
    new URL("e2e/eprover.html", server.resolvedUrls.local[0]).href,
  );
  await page.evaluate(() => window.ready);
  assert.equal(await page.evaluate(() => crossOriginIsolated), true);
  await page.locator("#proverBackend").selectOption("e");
  await page.locator("#cfgSelectionBudget").fill("1");
  assert.equal(await page.locator("#cfgMaxSteps").count(), 0);
  const proof = await page.evaluate(async () => {
    const { result } = await window.rpc.call("prove", {
      assertions: "(likes Alice Bob)",
      query: "(likes Alice Bob)",
      config: window.prover.config(),
    });
    return result;
  });
  assert.equal(proof.status, "Proved", proof.raw_output);
  assert.ok(proof.input_tptp.includes("Alice"));
  assert.ok(proof.proof.length > 0);
  console.log(
    "ok E Ask/Tell retains mandatory assertions under a small selection target",
  );

  await page.evaluate(async () => {
    await window.rpc.call("newSession");
    await window.rpc.call("ingest", {
      name: "contradiction.kif",
      text: "(likes Alice Bob)\n(not (likes Alice Bob))",
    });
    await window.rpc.call("promoteAll", { names: ["contradiction.kif"] });
  });
  const audit = await page.evaluate(
    async () =>
      (
        await window.rpc.call("audit", {
          config: window.prover.config(),
        })
      ).result,
  );
  assert.equal(audit.status, "Inconsistent", audit.raw_output);
  assert.equal(audit.contradictions.length, 1);
  assert.ok(
    audit.contradictions[0].steps.some(
      (step) => step.file === "contradiction.kif",
    ),
  );
  console.log("ok E audit exposes proof and source citations");

  await page.locator("#cfgAuditAxfilter").check();
  await page.locator("#cfgSelectionBudget").fill("10");
  await page.locator("#cfgSubsetLimit").fill("10");
  const subsets = await page.evaluate(
    async () =>
      (
        await window.rpc.call("audit", {
          config: window.prover.config(),
          limit: 5,
        })
      ).result,
  );
  assert.equal(subsets.status, "Inconsistent", subsets.raw_output);
  assert.equal(subsets.contradictions.length, 1);
  assert.match(subsets.raw_output, /Subset audit:.*attempted/);
  assert.ok(
    subsets.contradictions[0].steps.some(
      (step) => step.file === "contradiction.kif",
    ),
  );
  console.log(
    "ok filtered audit preserves citations and deduplicates contradictions",
  );

  await page.evaluate(async () => {
    await window.rpc.call("newSession");
    await window.rpc.call("ingest", {
      name: "consistent.kif",
      text: "(likes Alice Bob)",
    });
    await window.rpc.call("promoteAll", { names: ["consistent.kif"] });
  });
  const limited = await page.evaluate(
    async () =>
      (await window.rpc.call("audit", { config: window.prover.config() }))
        .result,
  );
  assert.equal(limited.status, "Unknown", limited.raw_output);
  assert.match(limited.raw_output, /does not establish whole-KB consistency/);
  console.log("ok subset satisfiability does not claim whole-KB consistency");

  await page.locator("#proverBackend").selectOption("native");
  const native = await page.evaluate(
    async () =>
      (
        await window.rpc.call("prove", {
          query: "(likes Alice Bob)",
          config: window.prover.config(),
        })
      ).result,
  );
  assert.equal(native.status, "Proved", native.raw_output);
  console.log("ok switching back to SUPr still proves");

  await page.locator("#proverBackend").selectOption("vampire");
  const vampire = await page.evaluate(
    async () =>
      (
        await window.rpc.call("prove", {
          query: "(likes Alice Bob)",
          config: window.prover.config(),
        })
      ).result,
  );
  assert.equal(vampire.status, "Proved", vampire.raw_output);
  console.log("ok Vampire still works through the shared bridge");

  const recovery = await browser.newPage();
  await recovery.route("**/eprover/eprover.js", async (route) => {
    await new Promise((resolve) => setTimeout(resolve, 7000));
    await route.abort().catch(() => {});
  });
  await recovery.goto(
    new URL("e2e/eprover.html", server.resolvedUrls.local[0]).href,
  );
  await recovery.evaluate(() => window.ready);
  const timeout = await recovery.evaluate(
    async () =>
      (
        await window.rpc.call("prove", {
          assertions: "(likes Alice Bob)",
          query: "(likes Alice Bob)",
          config: { ...window.prover.config(), backend: "e", timeLimitSecs: 1 },
        })
      ).result,
  );
  assert.equal(timeout.status, "Timeout", timeout.raw_output);
  await recovery.unroute("**/eprover/eprover.js");
  const recovered = await recovery.evaluate(
    async () =>
      (
        await window.rpc.call("prove", {
          assertions: "(likes Alice Bob)",
          query: "(likes Alice Bob)",
          config: {
            ...window.prover.config(),
            backend: "e",
            timeLimitSecs: 10,
          },
        })
      ).result,
  );
  assert.equal(recovered.status, "Proved", recovered.raw_output);
  await recovery.close();
  console.log(
    "ok E deadline terminates the worker and the next query recovers",
  );
} finally {
  await browser.close();
  await server.close();
}
