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
const RELEASE_LIST_RE =
  /^https:\/\/api\.github\.com\/repos\/ontologyportal\/sigma-rs\/releases\?.*$/;
const releasePage = (tag) =>
  `https://github.com/ontologyportal/sigma-rs/releases/tag/${tag}`;

// A release body shaped like a real one: three top-level `###` sections, the
// Web one carrying two `####` subsections (one with a Markdown link, to
// check the "open in a new tab" sanitizer hook) and a raw <img>, which is
// rendered as a link instead.
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
];

let failures = 0;

/** Runs one scenario in its own browser context, so each gets fresh
 *  localStorage and its own set of mocked routes. */
async function runCase(
  name,
  { seenVersion, version, release, releases = [] },
  check,
) {
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
      body: JSON.stringify({
        tag_name: `sigmakee-v${version}`,
        html_url: releasePage(`sigmakee-v${version}`),
        body: release,
      }),
    });
  });
  await page.route(RELEASE_LIST_RE, (route) => {
    const q = new URL(route.request().url()).searchParams;
    const perPage = Number(q.get("per_page") ?? 30);
    const start = (Number(q.get("page") ?? 1) - 1) * perPage;
    return route.fulfill({
      contentType: "application/json",
      body: JSON.stringify(releases.slice(start, start + perPage)),
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

    if (await notes.locator("img").count())
      throw new Error("<img> should have been rewritten to a link");
    const imageLink = notes.locator("a", {
      hasText: "View image: layout themes demo",
    });
    if (!(await imageLink.getAttribute("href")).includes("layout.png"))
      throw new Error("image link points elsewhere");
    if ((await imageLink.getAttribute("target")) !== "_blank")
      throw new Error("image link does not open in a new tab");

    const link = notes.locator("a", { hasText: "layout docs" });
    if ((await link.getAttribute("target")) !== "_blank")
      throw new Error("release-note link does not open in a new tab");
    if (!(await link.getAttribute("rel")).includes("noopener"))
      throw new Error("release-note link is missing rel=noopener");

    const pageLink = dialog.locator("a", {
      hasText: "View this release on GitHub",
    });
    if (
      (await pageLink.getAttribute("href")) !== releasePage("sigmakee-v9.9.9")
    )
      throw new Error("release page link missing or wrong");
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

// Newest first, as GitHub lists them: a current release with a Web section,
// a draft and another component's release (both hidden), six plain releases,
// and an old release with no Web section (shown whole) -- eight visible in
// all, so two "Load more" pages, the first of which spans two API pages.
const release = (tag, body, extra = {}) => ({
  tag_name: tag,
  html_url: releasePage(tag),
  published_at: "2026-04-25T12:00:00Z",
  draft: false,
  body,
  ...extra,
});
const RELEASES = [
  release("sigmakee-v9.9.9", RELEASE_BODY),
  release("sigmakee-v9.9.8", "Unpublished draft.", { draft: true }),
  release("sumo-lsp-v0.1.0", "Language server release."),
  ...[6, 5, 4, 3, 2, 1].map((n) =>
    release(`sigmakee-v1.0.${n}`, `Patch ${n}.`),
  ),
  release("sigmakee-v1.0.0", "Initial release. See documentation for details"),
];

await runCase(
  "release-history",
  { seenVersion: "9.9.9", version: "9.9.9", release: null, releases: RELEASES },
  async (page) => {
    await page.locator("button.settings-btn").click();
    await page.locator("dialog[open] button.version").click();
    const dialog = page.locator('dialog[open]:has-text("Release notes")');
    await dialog.waitFor({ timeout: 10_000 });

    const entries = dialog.locator("details");
    const summaries = () => dialog.locator("details > summary").allInnerTexts();
    await entries.first().waitFor({ timeout: 10_000 });
    const first = await summaries();
    if (
      first.length !== 5 ||
      !/^v9\.9\.9 - .*\(current\)$/.test(first[0]) ||
      !/^v1\.0\.6 - /.test(first[1]) ||
      !/^v1\.0\.3 - /.test(first[4])
    )
      throw new Error("unexpected first page: " + JSON.stringify(first));

    const current = entries.nth(0);
    if (!(await current.evaluate((d) => d.open)))
      throw new Error("the running version's entry should start open");
    const currentText = await current.innerText();
    if (
      !/Layout Themes/.test(currentText) ||
      /validate command/.test(currentText)
    )
      throw new Error("current entry should show only the Web section");
    const link = current.locator("a", {
      hasText: "View this release on GitHub",
    });
    if ((await link.getAttribute("href")) !== releasePage("sigmakee-v9.9.9"))
      throw new Error("current entry's release link is wrong");
    if (await entries.nth(1).evaluate((d) => d.open))
      throw new Error("older entries should start closed");

    const loadMore = dialog.locator("button", { hasText: "Load more" });
    await loadMore.click();
    await entries.nth(7).waitFor({ timeout: 10_000 });
    const all8 = await summaries();
    if (all8.length !== 8 || !/^v1\.0\.0 - /.test(all8[7]))
      throw new Error("unexpected full list: " + JSON.stringify(all8));
    if (await loadMore.count())
      throw new Error("Load more should disappear once every release is shown");

    const old = entries.nth(7);
    await old.locator("summary").click();
    if (!/Initial release/.test(await old.innerText()))
      throw new Error("old release without a Web section should show its body");

    const all = dialog.locator("a", { hasText: "All releases on GitHub" });
    if (
      (await all.getAttribute("href")) !==
      "https://github.com/ontologyportal/sigma-rs/releases"
    )
      throw new Error("all-releases link is wrong");
  },
);

await browser.close();
await server?.close();

if (failures) process.exit(1);
console.log("\nversion-notes e2e: all steps passed");
