// Browser-driven check of the KB tab's per-source update preferences
// (src/stores/kb.ts, src/components/kb/SourcesCard.vue,
// src/components/kb/UpdatePreviewDialog.vue): cycling the auto-update /
// auto-check / no-check button, a manual "Update now" finding and reviewing
// a real diff, and the background auto-check path surfacing a dismissible
// "Review" alert. Mocks the GitHub commits API and the default SUMO repo's
// raw Merge.kif so upstream "changes" are deterministic instead of waiting
// on real repo history.
//
//   npm run test:e2e:kb-updates --workspace @sigma/web
//
// Requires Playwright's Chromium (`npx playwright install chromium`), or set
// PLAYWRIGHT_CHROMIUM to an existing Chromium executable. BASE_URL skips the
// built-in server and targets a running deployment instead. HEADED=1 runs
// with a visible browser window instead of headless (SLOWMO=<ms> adds a
// per-action delay on top, so the steps are actually watchable).

import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const shots = path.join(here, "shots");
fs.mkdirSync(shots, { recursive: true });

// Boot fetches Mid-level-ontology.kif and WordNet from the real GitHub repo
// (only Merge.kif's raw fetch is mocked below); allow for a cold network.
const BOOT_TIMEOUT = 300_000;

const COMMITS_URL =
  "https://api.github.com/repos/ontologyportal/sumo/commits?per_page=1";
const MERGE_RAW_URL =
  "https://raw.githubusercontent.com/ontologyportal/sumo/master/Merge.kif";
const MARKER = "E2EUpdateMarker";
const MERGE_V1 = "(instance E2EMergeStub Entity)\n";
const MERGE_V2 = `(instance E2EMergeStub Entity)\n(instance ${MARKER} Entity)\n`;
const SUMO_ORIGIN_ID = "github:ontologyportal/sumo@master";
const UPDATE_PREFS_KEY = "sumoBrowserUpdatePrefs";
const UPDATE_BASELINES_KEY = "sumoBrowserUpdateBaselines";

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

const HEADED = !!process.env.HEADED;
// Headed with no explicit pacing would still run at full speed -- give it a
// watchable default (SLOWMO overrides either way, including to 0).
const SLOWMO = process.env.SLOWMO
  ? Number(process.env.SLOWMO)
  : HEADED
    ? 1500
    : undefined;

const browser = await chromium.launch({
  executablePath: process.env.PLAYWRIGHT_CHROMIUM || undefined,
  headless: !HEADED,
  slowMo: SLOWMO,
});

const IGNORED_CONSOLE = [
  /wheel sensitivity/,
  /status of 403/, // GitHub's anonymous quota, hit by unrelated background reads
  // Monaco's semantic tokenizer racing an LSP re-highlight against the
  // editor swapping in the applied update's text -- harmless, and specific
  // to how fast this test's mocked "Apply" lands compared to a real edit.
  /Invalid Semantic Tokens Data From Extension/,
];

let failures = 0;

/** Runs one scenario in its own browser context/page, so each gets fresh
 *  localStorage and its own mocked routes. `setup(page)` registers routes
 *  and any `addInitScript` seeding before boot starts. */
async function runCase(name, setup, check) {
  const context = await browser.newContext({
    viewport: { width: 1200, height: 900 },
  });
  const problems = [];
  const page = await context.newPage();
  page.on("console", (m) => {
    if (m.type() !== "error" && m.type() !== "warning") return;
    if (IGNORED_CONSOLE.some((re) => re.test(m.text()))) return;
    problems.push(`[console.${m.type()}] ${m.text()}`);
  });
  page.on("pageerror", (e) => problems.push(`[pageerror] ${e.message}`));

  try {
    await setup(page);
    await page.goto(base, { waitUntil: "domcontentloaded" });
    await page.waitForSelector("nav.tabs", { timeout: BOOT_TIMEOUT });
    await page
      .locator("nav.tabs button", { hasText: "Knowledge base" })
      .first()
      .click();
    await page.waitForSelector(".src-report", { timeout: 60_000 });
    await check(page);
    if (problems.length)
      throw new Error(`page problems: ${problems.join("; ")}`);
    await page.screenshot({ path: path.join(shots, `kb-update-${name}.png`) });
    // Otherwise the context below closes the instant the last assertion
    // passes, before there's been any chance to actually look at the page.
    if (HEADED) await page.waitForTimeout(1500);
    console.log(`ok   ${name}`);
  } catch (e) {
    failures += 1;
    await page
      .screenshot({ path: path.join(shots, `kb-update-${name}-FAIL.png`) })
      .catch(() => {});
    if (HEADED) await page.waitForTimeout(3000).catch(() => {});
    console.log(`FAIL ${name}: ${e.message.split("\n")[0]}`);
  } finally {
    await context.close();
  }
}

/** The Sources card row for the default SUMO repo (label "GitHub"). */
const sumoRow = (page) =>
  page.locator(".src-report", { hasText: "GitHub" }).first();

async function waitDialogGone(page) {
  await page.waitForFunction(
    () => !document.querySelector("dialog[open]"),
    null,
    { timeout: 15_000 },
  );
}

// -- Scenario 1: cycle the preference button, then a manual "Update now"
//    finds and reviews a real upstream change. -------------------------------
await runCase(
  "cycle-and-update-now",
  async (page) => {
    let commitSha = "1111111111111111111111111111111111111a";
    await page.route(COMMITS_URL, (route) =>
      route.fulfill({
        contentType: "application/json",
        body: JSON.stringify([
          {
            sha: commitSha,
            commit: { author: { date: new Date().toISOString() } },
          },
        ]),
      }),
    );
    let mergeVersion = 1;
    await page.route(MERGE_RAW_URL, (route) =>
      route.fulfill({
        contentType: "text/plain",
        body: mergeVersion === 1 ? MERGE_V1 : MERGE_V2,
      }),
    );
    // Exposed so `check` can advance the mocks mid-scenario without a reload
    // (a reload would re-fetch Merge.kif fresh too, collapsing the diff).
    await page.exposeFunction("__advanceUpstream", () => {
      commitSha = "2222222222222222222222222222222222222b";
      mergeVersion = 2;
    });
  },
  async (page) => {
    const row = sumoRow(page);
    const pref = row.locator("button.src-pref:not(.src-update-now)");

    // First-ever check just records a baseline -- no diff to show yet.
    await pref.waitFor({ timeout: 10_000 });
    if (!/Auto-update/.test(await pref.innerText()))
      throw new Error("default pref for a git source should be auto-update");

    await pref.click();
    if (!/Auto-check/.test(await pref.innerText()))
      throw new Error("pref did not cycle to auto-check");
    await pref.click();
    if (!/No check/.test(await pref.innerText()))
      throw new Error("pref did not cycle to no-check");
    await pref.click();
    if (!/Auto-update/.test(await pref.innerText()))
      throw new Error("pref did not cycle back to auto-update");

    // A reload should keep the cycled-to preference (it's persisted).
    await pref.click(); // -> auto-check
    await page.reload({ waitUntil: "domcontentloaded" });
    await page.waitForSelector("nav.tabs", { timeout: BOOT_TIMEOUT });
    await page
      .locator("nav.tabs button", { hasText: "Knowledge base" })
      .first()
      .click();
    const row2 = sumoRow(page);
    const pref2 = row2.locator("button.src-pref:not(.src-update-now)");
    await pref2.waitFor({ timeout: 10_000 });
    if (!/Auto-check/.test(await pref2.innerText()))
      throw new Error("cycled preference did not survive a reload");

    // Simulate upstream moving on, then force a check regardless of pref.
    await page.evaluate(() => window.__advanceUpstream());
    await row2.locator("button.src-update-now").click();

    const dialog = page.locator('dialog[open]:has-text("Review update")');
    await dialog.waitFor({ timeout: 15_000 });
    await page.waitForFunction(
      () =>
        document.querySelector(".monaco-diff-editor")?.textContent?.length > 0,
      null,
      { timeout: 15_000 },
    );
    const diffText = await page.locator(".monaco-diff-editor").innerText();
    if (!diffText.includes(MARKER))
      throw new Error(
        "diff dialog did not show the incoming change: " + diffText,
      );

    await dialog.locator("button.btn", { hasText: "Apply" }).click();
    await waitDialogGone(page);

    // Confirm the update actually landed in the running session, not just
    // that the dialog closed -- switching tabs and reopening the file in
    // the SAME page (no reload, which would refetch Merge.kif fresh and
    // pass even if "Apply" had silently done nothing).
    await page.locator("nav.tabs button", { hasText: "Edit" }).first().click();
    await page
      .getByRole("button", {
        name: "Open a file, or create a new one",
        exact: true,
      })
      .click();
    await page
      .locator(".open-list .open-file", { hasText: "Merge.kif" })
      .click();
    await page.waitForFunction(
      (marker) =>
        [...document.querySelectorAll(".edit-tab .view-lines")].some((el) =>
          el.textContent?.includes(marker),
        ),
      MARKER,
      { timeout: 15_000 },
    );
    const editorText = (
      await page.locator(".edit-tab .view-lines").allInnerTexts()
    ).join("\n");
    if (!editorText.includes(MARKER))
      throw new Error("applied text was not actually loaded into the KB");
  },
);

// -- Scenario 2: a pre-existing baseline (seeded, simulating an earlier
//    session) lets the background auto-check find a real change on boot and
//    surface it as a dismissible alert with a Review action. --------------
await runCase(
  "auto-check-alert",
  async (page) => {
    await page.route(COMMITS_URL, (route) =>
      route.fulfill({
        contentType: "application/json",
        body: JSON.stringify([
          {
            sha: "3333333333333333333333333333333333333c",
            commit: { author: { date: new Date().toISOString() } },
          },
        ]),
      }),
    );
    // First call is the boot ingest, second is the background check's own
    // re-fetch moments later -- give them different content so a real diff
    // exists without needing a `force` re-fetch or a reload.
    let calls = 0;
    await page.route(MERGE_RAW_URL, (route) => {
      calls += 1;
      return route.fulfill({
        contentType: "text/plain",
        body: calls === 1 ? MERGE_V1 : MERGE_V2,
      });
    });
    await page.addInitScript(
      ([prefsKey, baselinesKey, originId]) => {
        localStorage.setItem(
          prefsKey,
          JSON.stringify({ [originId]: "auto-check" }),
        );
        localStorage.setItem(
          baselinesKey,
          JSON.stringify({
            [originId]: "0000000000000000000000000000000000000d",
          }),
        );
      },
      [UPDATE_PREFS_KEY, UPDATE_BASELINES_KEY, SUMO_ORIGIN_ID],
    );
  },
  async (page) => {
    const alert = page.locator(".src-alert", { hasText: "GitHub" });
    await alert.waitFor({ timeout: 30_000 });

    const review = alert.locator("button.src-alert-review");
    await review.click();

    const dialog = page.locator('dialog[open]:has-text("Review update")');
    await dialog.waitFor({ timeout: 15_000 });
    await page.waitForFunction(
      () =>
        document.querySelector(".monaco-diff-editor")?.textContent?.length > 0,
      null,
      { timeout: 15_000 },
    );

    await dialog.locator("button.btn", { hasText: "Apply" }).click();
    await waitDialogGone(page);

    // The alert that spawned the review is dismissed once acknowledged.
    if (await page.locator(".src-alert", { hasText: "GitHub" }).count())
      throw new Error("reviewed alert should have been dismissed");
  },
);

await browser.close();
await server?.close();

if (failures) process.exit(1);
console.log("\nkb-update-prefs e2e: all steps passed");
