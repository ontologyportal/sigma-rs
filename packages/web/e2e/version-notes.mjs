// Browser-driven check of the version-update dialog (src/stores/shell.ts,
// src/components/VersionDialog.vue): a real upgrade is hard to simulate (it
// needs a bumped `version.json` and a matching GitHub release to exist at
// once), so this test seeds localStorage's "last seen version" and mocks
// both `./version.json` and the GitHub release API response instead of
// waiting for an actual release.
//
//   npm run test:e2e:version --workspace @sigma/web
//
// Requires Playwright's Chromium (`npx playwright install chromium`), or set
// PLAYWRIGHT_CHROMIUM to an existing Chromium executable. BASE_URL skips the
// built-in server and targets a running deployment instead.

import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const shots = path.join(here, "shots");
fs.mkdirSync(shots, { recursive: true });

const BOOT_TIMEOUT = 300_000;
const SEEN_VERSION_KEY = "sumoBrowserSeenVersion";
const RELEASE_URL_RE =
  /^https:\/\/api\.github\.com\/repos\/ontologyportal\/sigma-rs\/releases\/tags\/.*$/;

// A release body shaped like a real one: three top-level `###` sections, the
// Web one carrying two `####` subsections (one with a Markdown link, to
// check the "open in a new tab" sanitizer hook) and a raw <img>.
const RELEASE_BODY = `## SigmaKEE v9.9.9

### Web Updates
#### Layout Themes

With layout themes you can change how the application appears. See the
[layout docs](https://example.com/layout) for details.

<img width="720" height="387" alt="layout themes demo" src="https://example.com/layout.png" />

#### WordNet Diagnostics

WordNet diagnostics now appear on the Diagnostics page.

### CLI Updates

#### WordNet diagnostics

Use the \`--wordnet\` option in the validate command.

### VSCode Updates

This update deploys the first official SigmaKEE VSCode extension.
`;

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

const IGNORED_CONSOLE = [
  /wheel sensitivity/,
  /status of 403/, // GitHub's anonymous quota, hit by unrelated background reads
  /status of 404/, // the "no release yet" case deliberately mocks a 404
  /NotSameOriginAfterDefaultedToSameOriginByCoep/, // the fixture's placeholder <img> src is cross-origin
];

let failures = 0;

/** Runs one scenario in its own browser context, so each gets fresh
 *  localStorage and its own set of mocked routes. */
async function runCase(name, { seenVersion, version, release }, check) {
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

  if (seenVersion) {
    await page.addInitScript(
      ([key, v]) => localStorage.setItem(key, v),
      [SEEN_VERSION_KEY, seenVersion],
    );
  }

  await page.route("**/version.json", (route) =>
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({ version, build: "999", commit: "abc1234" }),
    }),
  );

  let releaseRequests = 0;
  await page.route(RELEASE_URL_RE, (route) => {
    releaseRequests += 1;
    if (release === null) return route.fulfill({ status: 404, body: "{}" });
    return route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({ tag_name: `sigmakee-v${version}`, body: release }),
    });
  });

  try {
    await page.goto(base, { waitUntil: "domcontentloaded" });
    await page.waitForSelector("nav.tabs", { timeout: BOOT_TIMEOUT });
    await check(page, () => releaseRequests);
    if (problems.length)
      throw new Error(`page problems: ${problems.join("; ")}`);
    await page.screenshot({ path: path.join(shots, `version-${name}.png`) });
    console.log(`ok   ${name}`);
  } catch (e) {
    failures += 1;
    await page
      .screenshot({ path: path.join(shots, `version-${name}-FAIL.png`) })
      .catch(() => {});
    console.log(`FAIL ${name}: ${e.message.split("\n")[0]}`);
  } finally {
    await context.close();
  }
}

await runCase(
  "first-visit",
  { seenVersion: null, version: "9.9.9", release: RELEASE_BODY },
  async (page, releaseRequests) => {
    const dialog = page.locator('dialog[open]:has-text("Welcome to SigmaKEE")');
    await dialog.waitFor({ timeout: 10_000 });
    await page.waitForTimeout(1000); // give a wrongly-fired notes fetch time to land
    if (releaseRequests() !== 0)
      throw new Error("release notes were fetched on a first-ever visit");
    if (await dialog.locator(".notes").count())
      throw new Error("first-visit dialog should not render release notes");
  },
);

await runCase(
  "upgrade-with-notes",
  { seenVersion: "1.0.0", version: "9.9.9", release: RELEASE_BODY },
  async (page) => {
    const dialog = page.locator(
      'dialog[open]:has-text("New version available")',
    );
    await dialog.waitFor({ timeout: 10_000 });
    const notes = dialog.locator(".notes");
    await notes.waitFor({ timeout: 10_000 });

    const text = await notes.innerText();
    if (!/Layout Themes/.test(text))
      throw new Error("Web section heading missing: " + text);
    if (!/WordNet Diagnostics/.test(text))
      throw new Error("Web section body missing: " + text);
    if (/VSCode extension/.test(text))
      throw new Error("VSCode section leaked into the Web notes: " + text);
    if (/validate command/.test(text))
      throw new Error("CLI section leaked into the Web notes: " + text);

    const img = notes.locator("img");
    if (!(await img.getAttribute("src")).includes("layout.png"))
      throw new Error("embedded <img> was not rendered");

    const link = notes.locator("a", { hasText: "layout docs" });
    if ((await link.getAttribute("target")) !== "_blank")
      throw new Error("release-note link does not open in a new tab");
    if (!(await link.getAttribute("rel")).includes("noopener"))
      throw new Error("release-note link is missing rel=noopener");
  },
);

await runCase(
  "upgrade-without-release",
  { seenVersion: "1.0.0", version: "9.9.9", release: null },
  async (page) => {
    const dialog = page.locator(
      'dialog[open]:has-text("New version available")',
    );
    await dialog.waitFor({ timeout: 10_000 });
    await page.waitForTimeout(1000); // the failed notes fetch settles
    if (await dialog.locator(".notes").count())
      throw new Error("notes rendered despite a 404 release lookup");
    const body = await dialog.locator(".hint").innerText();
    if (!/using a new version \(v9\.9\.9\)/.test(body))
      throw new Error("generic upgrade notice missing: " + body);
  },
);

await browser.close();
await server?.close();

if (failures) process.exit(1);
console.log("\nversion-notes e2e: all steps passed");
