// Browser-driven check of the Vampire (WASM) backend end to end: the engine's
// external prover layer drives the Emscripten Vampire through the
// Atomics.wait bridge (src/worker/vampire-bridge.ts) on the Ask/Tell and
// Audit tabs, then the worker still answers ordinary requests.
//
//   BASE_URL=http://localhost:8080/ node packages/web/e2e/vampire.mjs
//
// Needs a deployment that ships public/vampire/ (npm run build --workspace
// @sigma/vampire) and sends the COOP/COEP headers (the Vite dev server does).

import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const shots = path.join(here, "shots");
fs.mkdirSync(shots, { recursive: true });

const BOOT_TIMEOUT = 300_000;
let server = null;
let base = process.env.BASE_URL;
if (!base) {
  const { createServer } = await import("vite");
  server = await createServer({
    root: path.join(here, ".."),
    logLevel: "error",
  });
  await server.listen();
  base = server.resolvedUrls.local[0];
}

const browser = await chromium.launch({
  executablePath: process.env.PLAYWRIGHT_CHROMIUM || undefined,
});
const page = await (
  await browser.newContext({ viewport: { width: 1200, height: 900 } })
).newPage();

const problems = [];
page.on("console", (m) => {
  if (m.type() !== "error") return;
  if (/status of 40[34]/.test(m.text())) return;
  problems.push(`[console.error] ${m.text()}`);
});
page.on("pageerror", (e) => problems.push(`[pageerror] ${e.message}`));
page.on("response", (r) => {
  // Absent in a dev build: the version stamp and the OAuth session probe.
  if (r.status() >= 400 && !/version\.json|\/api\/me/.test(r.url()))
    problems.push(`[http ${r.status()}] ${r.url()}`);
});

let failures = 0;
const step = async (name, fn) => {
  try {
    await fn();
    await page.screenshot({ path: path.join(shots, `vampire-${name}.png`) });
    console.log(`ok   ${name}`);
  } catch (e) {
    failures += 1;
    await page
      .screenshot({ path: path.join(shots, `vampire-${name}-FAIL.png`) })
      .catch(() => {});
    console.log(
      `FAIL ${name}: ${e.message.split("\n").slice(0, 14).join("\n     ")}`,
    );
  }
};
const tab = (label) =>
  page.locator("nav.tabs button", { hasText: label }).first();

await step("00-load", async () => {
  await page.goto(base, { waitUntil: "domcontentloaded" });
  await page.waitForSelector("nav.tabs", { timeout: BOOT_TIMEOUT });
  const isolated = await page.evaluate(() => crossOriginIsolated);
  if (!isolated) throw new Error("page is not cross-origin isolated");
});

await step("01-select-vampire", async () => {
  await tab("Ask/Tell").click();
  await page.waitForSelector(".monaco-editor", { timeout: 60_000 });
  await page.locator("button.cog").first().click();
  await page.locator("select#proverBackend").selectOption("vampire");
  await page.locator("button.cog").first().click();
});

await step("02-prove-default-query", async () => {
  await page.locator("button.btn", { hasText: /^Prove/ }).click();
  await page.waitForSelector(".status", { timeout: 180_000 });
  const status = (await page.locator(".status").first().innerText()).trim();
  const badge = await page.locator("text=via Vampire").count();
  if (!badge) throw new Error("result is not labelled 'via Vampire'");
  if (!/Proved/.test(status)) throw new Error("status: " + status);
  const dl = page.locator("button.btn", { hasText: "Download TPTP input" });
  if (!(await dl.isVisible())) throw new Error("no TPTP download offered");
  console.log(`     status=${status}`);
});

await step("03-worker-still-answers", async () => {
  await tab("Browse").click();
  await page.fill('input[type="search"]', "Human");
  await page.waitForSelector("ul.results li", { timeout: 60_000 });
});

await step("04-audit", async () => {
  await tab("Audit").click();
  await page.getByRole("button", { name: /Run audit/ }).click();
  await page.waitForSelector(".audit-status", { timeout: 300_000 });
  const status = (
    await page.locator(".audit-status").first().innerText()
  ).trim();
  const badge = await page.locator("text=via Vampire").count();
  if (!badge) throw new Error("audit is not labelled 'via Vampire'");
  if (!/Consistent|Inconsistent|Timeout/.test(status))
    throw new Error("status: " + status);
  console.log(`     audit status=${status}`);
});

await step("05-native-still-proves", async () => {
  await tab("Ask/Tell").click();
  await page.locator("button.cog").first().click();
  await page.locator("select#proverBackend").selectOption("native");
  await page.locator("button.cog").first().click();
  await page.locator("button.btn", { hasText: /^Prove/ }).click();
  await page.waitForSelector("text=via SUPr", { timeout: 180_000 });
  const status = (await page.locator(".status").first().innerText()).trim();
  if (!/Proved/.test(status)) throw new Error("status: " + status);
});

await browser.close();
await server?.close();
if (problems.length)
  console.log(`\nPROBLEMS:\n` + problems.map((p) => "  " + p).join("\n"));
if (failures || problems.length) process.exit(1);
console.log("\nvampire e2e: all steps passed");
