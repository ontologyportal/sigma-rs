// Browser-driven smoke test of the SUMO browser demo: boots the app in a
// headless Chromium against a Vite dev server started here, walks every tab
// through its main flow, and fails on any step error, page error, or
// unexpected console error/warning.
//
//   npm run test:e2e --workspace @sigma/web
//
// Requires Playwright's Chromium (`npx playwright install chromium`), or set
// PLAYWRIGHT_CHROMIUM to an existing Chromium executable. BASE_URL skips the
// built-in server and targets a running deployment instead. Screenshots of
// every step land in e2e/shots/ (gitignored).

import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const shots = path.join(here, "shots");
fs.mkdirSync(shots, { recursive: true });

// Boot fetches the constituents and WordNet from GitHub; allow for a cold
// network on the first load.
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

// Known-noisy warnings that are not regressions: Cytoscape's advisory about
// custom wheel sensitivity and its label-mapping notices on edges without
// labels.
const IGNORED_CONSOLE = [
  /wheel sensitivity/,
  /Do not assign mappings/,
  /style value of `label`/,
  // GitHub's anonymous API quota (60/hour per IP) is environmental, not a
  // regression; the app answers it with the login dialog, dismissed below.
  /status of 403/,
];

const problems = [];
page.on("console", (m) => {
  if (m.type() !== "error" && m.type() !== "warning") return;
  if (IGNORED_CONSOLE.some((re) => re.test(m.text()))) return;
  problems.push(`[console.${m.type()}] ${m.text()}`);
});
page.on("pageerror", (e) => problems.push(`[pageerror] ${e.message}`));
page.on("requestfailed", (r) => {
  const u = r.url();
  // Absent in a dev build: the version stamp, the OAuth session probe, and
  // the optional Vampire wasm.
  if (/version\.json|\/api\/me|vampire/.test(u)) return;
  problems.push(`[requestfailed] ${u} ${r.failure()?.errorText}`);
});

/** Close the "Log in required" dialog the app raises when the anonymous
 *  GitHub quota is exhausted, which would otherwise block every click. */
async function dismissLoginDialog() {
  const cancel = page.locator(
    'dialog[open]:has-text("Log in required") button',
    {
      hasText: "Cancel",
    },
  );
  if (await cancel.count()) await cancel.click();
}

let failures = 0;
const step = async (name, fn) => {
  const before = problems.length;
  try {
    await dismissLoginDialog();
    await fn();
    await page.waitForTimeout(400);
    await page.screenshot({
      path: path.join(shots, `${name}.png`),
      fullPage: true,
    });
    const extra = problems.length - before;
    console.log(`ok   ${name}${extra ? `  (+${extra} problems)` : ""}`);
  } catch (e) {
    failures += 1;
    await page
      .screenshot({
        path: path.join(shots, `${name}-FAIL.png`),
        fullPage: true,
      })
      .catch(() => {});
    const msg = e.message.split("\n")[0];
    console.log(`FAIL ${name}: ${msg}`);
    problems.push(`[step ${name}] ${msg}`);
  }
};

const tab = (label) =>
  page.locator("nav.tabs button", { hasText: label }).first();

await step("00-load", async () => {
  await page.goto(base, { waitUntil: "domcontentloaded" });
  await page.waitForSelector("nav.tabs", { timeout: BOOT_TIMEOUT });
});

await step("01-browse-search", async () => {
  await page.fill('input[type="search"]', "Human");
  await page.waitForSelector("ul.results li", { timeout: 60_000 });
  if (!page.url().includes("q=Human"))
    throw new Error("URL missing ?q=Human: " + page.url());
});

await step("01b-browse-arrow-keys", async () => {
  await page.locator('input[type="search"]').focus();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowDown");
  await page.waitForTimeout(200);
  const selected = await page.locator("ul.results li.selected").count();
  if (selected !== 1) throw new Error("no highlighted result after ArrowDown");
  await page.keyboard.press("Enter");
  await page.waitForSelector(".man h2", { timeout: 60_000 });
  if (!page.url().includes("sym="))
    throw new Error("Enter did not open the highlighted result: " + page.url());
  await page.goBack();
  await page.waitForSelector("ul.results li", { timeout: 60_000 });
});

await step("02-browse-manpage", async () => {
  await page.locator("ul.results li a.sym").first().click();
  await page.waitForSelector(".man h2", { timeout: 60_000 });
  if (!page.url().includes("sym="))
    throw new Error("URL missing ?sym=: " + page.url());
  await page.waitForTimeout(3000); // the taxonomy graph streams in
});

await step("03-browse-back", async () => {
  await page.goBack();
  await page.waitForSelector("ul.results li", { timeout: 60_000 });
});

await step("04-browse-tab-keeps-state", async () => {
  await tab("History").click();
  await page.waitForTimeout(500);
  await tab("Browse").click();
  await page.waitForSelector("ul.results li", { timeout: 60_000 });
  if (!page.url().includes("q=Human"))
    throw new Error("tab bar lost the browse query: " + page.url());
});

await step("05-browse-home", async () => {
  await page.locator("a.brand").click();
  await page.waitForSelector(".stats", { timeout: 60_000 });
  if (/[?]/.test(page.url()))
    throw new Error("brand link kept a query: " + page.url());
});

await step("10-kb", async () => {
  await tab("Knowledge base").click();
  await page.waitForSelector("table tbody tr", { timeout: 60_000 });
  // The upstream catalog is a GitHub API read; wait for it (or its error).
  await page.waitForFunction(
    () => document.querySelectorAll("table tbody tr").length > 2,
    null,
    { timeout: 60_000 },
  );
});

await step("11-kb-add-remove", async () => {
  const search = page.locator('input[type="search"]').last();
  await search.fill("Weather");
  await page.waitForTimeout(300);
  const row = page
    .locator("table tbody tr", { hasText: "Weather.kif" })
    .first();
  await row.locator('input[type="checkbox"]').check();
  await row.locator("text=will load").waitFor({ timeout: 3000 });
  await page.locator("text=Unsaved changes").first().waitFor({ timeout: 3000 });
  await page.locator("button.btn", { hasText: "Save changes" }).click();
  await page.waitForSelector("text=/Added 1/", { timeout: 180_000 });
  await page.waitForFunction(
    () => !document.querySelector(".toast")?.checkVisibility?.(),
    null,
    { timeout: 180_000 },
  );
  // Now loaded: selecting it again schedules a removal.
  await row.locator('input[type="checkbox"]').check();
  await page.locator("button.btn", { hasText: "Save changes" }).click();
  await page.waitForSelector("text=/removed 1/", { timeout: 180_000 });
  await search.fill("");
});

await step("12-kb-import-dialog", async () => {
  await page.locator("button.btn", { hasText: "Import" }).first().click();
  const dialog = page.locator(
    'dialog[open]:has-text("Import into the library")',
  );
  await dialog.waitFor({ timeout: 5000 });
  await dialog.locator("button", { hasText: "GitHub" }).click();
  await dialog
    .locator("text=ontologyportal/sumo@master")
    .waitFor({ timeout: 5000 });
  await page.keyboard.press("Escape");
  await page.waitForFunction(
    () => !document.querySelector("dialog[open]"),
    null,
    { timeout: 5000 },
  );
});

await step("13-kb-local-library", async () => {
  const os = await import("node:os");
  const tmp = path.join(os.tmpdir(), "SmokeLocal.kif");
  fs.writeFileSync(tmp, "(instance SmokeLocalThing Entity)\n");
  page.once("dialog", (d) => d.accept()); // the delete confirmation, later
  await page.locator("button.btn", { hasText: "Import" }).first().click();
  const dialog = page.locator(
    'dialog[open]:has-text("Import into the library")',
  );
  await dialog.waitFor({ timeout: 5000 });
  await dialog.locator("button", { hasText: "Local" }).click();
  await dialog.locator("#importFiles").setInputFiles(tmp);
  await dialog
    .locator("button.btn", { hasText: /^Import/ })
    .last()
    .click();
  await dialog.locator("text=/Imported 1 file/").waitFor({ timeout: 15_000 });
  await dialog.locator("button", { hasText: "Close" }).click();
  await page.waitForFunction(
    () => !document.querySelector("dialog[open]"),
    null,
    { timeout: 15_000 },
  );
  const search = page.locator('input[type="search"]').last();
  await search.fill("SmokeLocal");
  const row = page
    .locator("table tbody tr", { hasText: "SmokeLocal.kif" })
    .first();
  await row.waitFor({ timeout: 5000 });
  if (!/Local/.test(await row.innerText()))
    throw new Error("imported file is not marked Local");
  await row.locator('input[type="checkbox"]').check();
  await page.locator("button.btn", { hasText: "Save changes" }).click();
  await page.waitForSelector("text=/Added 1/", { timeout: 180_000 });
  await page.waitForFunction(
    () => !document.querySelector(".toast")?.checkVisibility?.(),
    null,
    { timeout: 180_000 },
  );
  await row.locator('input[type="checkbox"]').check();
  await page.locator("button.btn", { hasText: "Save changes" }).click();
  await page.waitForSelector("text=/removed 1/", { timeout: 180_000 });
  if (!(await row.isVisible()))
    throw new Error("unloading removed the file from the library");
  await row.locator("text=delete").click();
  await page.waitForSelector("text=/Deleted SmokeLocal.kif/", {
    timeout: 5000,
  });
  if (await row.count()) throw new Error("deleted entry still listed");
  await search.fill("");
  fs.unlinkSync(tmp);
});

await step("15-problems", async () => {
  await tab("Problems").click();
  await page.waitForSelector("table tbody tr", { timeout: 60_000 });
  // The upstream catalog is a GitHub API read; wait for it (or its error).
  await page.waitForFunction(
    () => document.querySelectorAll("table tbody tr").length > 1,
    null,
    { timeout: 60_000 },
  );
  const search = page.locator('input[type="search"]').last();
  await search.fill(".kif.tq");
  await page.waitForTimeout(300);
  const row = page.locator("table tbody tr", { hasText: ".kif.tq" }).first();
  await row.waitFor({ timeout: 5000 });
  const name = (await row.locator(".name").innerText()).trim();
  if (!name.endsWith(".kif.tq"))
    throw new Error("first matching row is not a .kif.tq test: " + name);
  await row.locator('input[type="checkbox"]').check();
  await row.locator("text=will import").waitFor({ timeout: 3000 });
  await page.locator("button.btn", { hasText: "Save changes" }).click();
  await page.waitForSelector("text=/Imported 1/", { timeout: 60_000 });
  // Now imported: tick it again and run it.
  await row.locator('input[type="checkbox"]').check();
  await page.locator("button.btn", { hasText: "Run selected" }).click();
  await row.locator("td.col-extra .result").waitFor({ timeout: 180_000 });
  await page.waitForSelector("text=/passed\\./", { timeout: 180_000 });
  // Its name opens it in Ask/Tell.
  await row.locator("a.name").click();
  await page.waitForURL(/\/prover\?test=/, { timeout: 10_000 });
  await page.waitForSelector(".monaco-editor", { timeout: 60_000 });
  await page.waitForFunction(
    () => document.querySelector(".monaco-editor .view-lines")?.textContent,
    null,
    { timeout: 30_000 },
  );
  // Back to Problems (the tab returns to its last query) and remove it.
  await tab("Problems").click();
  await row.waitFor({ timeout: 5000 });
  await row.locator('input[type="checkbox"]').check();
  await row.locator("text=will remove").waitFor({ timeout: 3000 });
  await page.locator("button.btn", { hasText: "Save changes" }).click();
  await page.waitForSelector("text=/removed 1/", { timeout: 60_000 });
  await search.fill("");
});

await step("20-history", async () => {
  await tab("History").click();
  await page.waitForTimeout(4000);
});

await step("30-edit", async () => {
  await tab("Edit").click();
  await page.waitForSelector(".monaco-editor", { timeout: 60_000 });
  await page.waitForTimeout(3000);
});

await step("31-edit-deeplink", async () => {
  await page.goto(base + "edit?file=Merge.kif&l=100", {
    waitUntil: "domcontentloaded",
  });
  await page.waitForSelector("nav.tabs", { timeout: BOOT_TIMEOUT });
  await page.waitForSelector(".monaco-editor", { timeout: 60_000 });
  await page.waitForTimeout(4000);
});

await step("40-diagnostics", async () => {
  await tab("Diagnostics").click();
  await page.waitForTimeout(2000);
});

await step("41-diagnostics-filter", async () => {
  await page.locator("button", { hasText: "Filter" }).first().click();
  await page.waitForTimeout(300);
  const sel = page.locator("select#diag-filter-severity");
  const opts = await sel.locator("option").allTextContents();
  const pick = opts.find((o) => /^Warning/.test(o)) || opts[1];
  if (pick) {
    await sel.selectOption({ label: pick });
    await page.waitForTimeout(800);
    if (!page.url().includes("sev="))
      throw new Error("URL missing ?sev=: " + page.url());
  }
});

await step("42-diagnostics-sort", async () => {
  const sel = page.locator("select#diag-sort");
  await sel.selectOption("location");
  await page.waitForTimeout(800);
  if (!page.url().includes("sort=location"))
    throw new Error("URL missing ?sort=: " + page.url());
});

await step("50-asktell", async () => {
  await tab("Ask/Tell").click();
  await page.waitForSelector(".monaco-editor", { timeout: 60_000 });
  await page.waitForTimeout(1500);
});

await step("51-asktell-prove", async () => {
  await page.locator("button.btn", { hasText: /^Prove/ }).click();
  await page.waitForSelector(".status", { timeout: 120_000 });
  await page.waitForTimeout(1000);
});

await step("52-asktell-settings", async () => {
  await page.locator("button.cog").first().click();
  await page.waitForTimeout(500);
});

await step("60-audit", async () => {
  await tab("Audit").click();
  await page.waitForTimeout(1000);
  const timeLimits = await page.locator('label:has-text("time limit")').count();
  if (timeLimits !== 1)
    throw new Error(
      `expected one time-limit field on Audit, found ${timeLimits}`,
    );
});

await step("70-settings-dialog", async () => {
  await page.locator("button.settings-btn").click();
  await page.waitForSelector("dialog[open]", { timeout: 5000 });
  await page.waitForTimeout(500);
  await page.keyboard.press("Escape");
});

await step("80-slash-shortcut", async () => {
  await tab("History").click();
  await page.waitForTimeout(500);
  await page.keyboard.press("/");
  await page.waitForTimeout(500);
  const focused = await page.evaluate(() =>
    document.activeElement?.getAttribute("type"),
  );
  if (focused !== "search")
    throw new Error("search box not focused after /: " + focused);
});

await step("90-mobile-nav", async () => {
  await page.setViewportSize({ width: 420, height: 800 });
  await page.waitForTimeout(300);
  if (await page.locator("nav.tabs").isVisible())
    throw new Error("tab strip still visible on a narrow viewport");
  const overflow = await page.evaluate(
    () =>
      document.documentElement.scrollWidth >
      document.documentElement.clientWidth,
  );
  if (overflow)
    throw new Error("page scrolls horizontally on a narrow viewport");
  await page.locator(".tab-select select").selectOption("kb");
  await page.waitForSelector("table tbody tr", { timeout: 60_000 });
  if (!page.url().includes("/kb"))
    throw new Error("select did not navigate: " + page.url());
  await page.setViewportSize({ width: 1200, height: 900 });
});

await browser.close();
await server?.close();

if (problems.length) {
  console.log(
    `\nPROBLEMS (${problems.length}):\n` +
      problems.map((p) => "  " + p).join("\n"),
  );
}
if (failures || problems.length) process.exit(1);
console.log("\nall steps passed, no problems");
