/** Real workers and WASM, with a small isolated KB and the shared prover
 *  options panel (Audit profile). */
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
const backend = (page, label) =>
  page.getByRole("radio", { name: label, exact: true }).click();
/** A fixed selection target with autoscaling off -- the panel's form of the
 *  engine's `selectionBudget`. */
async function axiomTarget(page, n) {
  await page.locator("select[id$='-budget']").selectOption("axioms");
  await page.locator('input[aria-label="Maximum axioms"]').fill(String(n));
  const details = page.locator("summary", {
    hasText: "Axiom selection details",
  });
  const autoscale = page
    .locator("label.check", { hasText: "autoscale" })
    .locator("input");
  if (!(await autoscale.isVisible())) await details.click();
  await autoscale.uncheck();
}
try {
  const page = await browser.newPage();
  page.setDefaultTimeout(60000);
  await page.goto(
    new URL("e2e/eprover.html", server.resolvedUrls.local[0]).href,
  );
  await page.evaluate(() => window.ready);
  assert.equal(await page.evaluate(() => crossOriginIsolated), true);
  await page.locator(".prover-options").waitFor();
  await backend(page, "E");
  await axiomTarget(page, 1);
  assert.equal(await page.locator("#audit-steps").count(), 0);
  const proof = await page.evaluate(async () => {
    const { result } = await window.rpc.call("prove", {
      assertions: "(likes Alice Bob)",
      query: "(likes Alice Bob)",
      config: window.prover.config("audit"),
    });
    return result;
  });
  assert.equal(proof.status, "Proved", proof.raw_output);
  assert.ok(proof.input_tptp.includes("Alice"));
  assert.ok(proof.proof.length > 0);
  console.log(
    "ok E Ask/Tell retains mandatory assertions under a small selection target",
  );
  await axiomTarget(page, 10);

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
          config: window.prover.config("audit"),
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

  await page.locator("summary", { hasText: "E audit subsets" }).click();
  await page
    .locator("label.check", { hasText: "e_axfilter subsets" })
    .locator("input")
    .check();
  await axiomTarget(page, 10);
  await page.locator("#audit-subsets").fill("10");
  const subsets = await page.evaluate(
    async () =>
      (
        await window.rpc.call("audit", {
          config: window.prover.config("audit"),
          request: { count: 2, limit: 5 },
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
      (
        await window.rpc.call("audit", {
          config: window.prover.config("audit"),
        })
      ).result,
  );
  assert.equal(limited.status, "Unknown", limited.raw_output);
  assert.match(limited.raw_output, /does not establish whole-KB consistency/);
  console.log("ok subset satisfiability does not claim whole-KB consistency");

  await backend(page, "SUPr");
  const native = await page.evaluate(
    async () =>
      (
        await window.rpc.call("prove", {
          query: "(likes Alice Bob)",
          config: window.prover.config("audit"),
        })
      ).result,
  );
  assert.equal(native.status, "Proved", native.raw_output);
  console.log("ok switching back to SUPr still proves");

  await backend(page, "Vampire");
  const vampire = await page.evaluate(
    async () =>
      (
        await window.rpc.call("prove", {
          query: "(likes Alice Bob)",
          config: window.prover.config("audit"),
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
          config: {
            ...window.prover.config("audit"),
            backend: "e",
            timeLimitSecs: 1,
          },
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
            ...window.prover.config("audit"),
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
