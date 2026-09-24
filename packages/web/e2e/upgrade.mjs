// Browser-driven check that UPGRADING THE APP never breaks a returning user.
//
//   npm run test:e2e:upgrade --workspace @sigma/web
//
// Unlike the other e2e files, the "before" state here is not hand-seeded: an
// OLD build of this package (`OLD_REF`, default HEAD) is extracted with
// `git archive` and served, the user's work is created through its own UI --
// so every byte of localStorage, OPFS and the KB snapshot cache is exactly
// what that version writes -- and then the CURRENT working tree is served on
// the same origin, in the same browser context, and has to boot into it.
//
// The claims checked after the upgrade:
//   - boot succeeds with no page errors,
//   - every saved constituent is loaded, with the user's saved edits,
//   - the engine names each file the way the new code expects (a citation
//     opens its file) -- the old snapshot cache must not be restored over it,
//   - saving again after the upgrade does not leave a second copy behind,
//   - the new build's own snapshot restores on the boot after,
//   - an upload may now share a repo file's name.
//
// Requires Playwright's Chromium (`npx playwright install chromium`), or set
// PLAYWRIGHT_CHROMIUM to an existing Chromium executable. HEADED=1 runs with
// a visible browser window (SLOWMO=<ms> adds a per-action delay). UPGRADE_PORT
// picks the port both builds share (default 5188).

import { chromium } from "playwright";
import { execSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";

const here = path.dirname(fileURLToPath(import.meta.url));
const webRoot = path.join(here, "..");
const repoRoot = path.join(webRoot, "..", "..");
const shots = path.join(here, "shots");
fs.mkdirSync(shots, { recursive: true });

const OLD_REF = process.env.OLD_REF || "HEAD";
// Inside the workspace so the old copy resolves `sigmakee` and every other
// dependency through the same node_modules the current build uses.
const oldRoot = path.join(here, ".old-app");
const PORT = Number(process.env.UPGRADE_PORT || 5188);

const BOOT_TIMEOUT = 180_000;

const COMMITS_GLOB = "https://api.github.com/repos/*/*/commits?*";
const MERGE_RAW_URL =
  "https://raw.githubusercontent.com/ontologyportal/sumo/master/Merge.kif";
const URL_SRC = "https://e2e.invalid/Extra.kif";

const SUMO_ORIGIN = {
  kind: "sumo",
  owner: "ontologyportal",
  repo: "sumo",
  branch: "master",
};
const URL_ORIGIN = { kind: "url", url: URL_SRC };

const SUMO_FILE_SETTING = "sumoFiles";
const LIBRARY_KEY = "sumoLibrary";
const WORDNET_ENABLED_KEY = "sumoBrowserWordNetEnabled";

const SHA = "1111111111111111111111111111111111111aaa";

const MERGE_TEXT = "(instance E2EMergeStub Entity)\n";
const MINE = "Mine.kif";
const MINE_TERM = "E2EUpgradeMine";
const MINE_TEXT = `(instance ${MINE_TERM} Entity)\n`;
const EXTRA_TERM = "E2EUpgradeExtra";
const EXTRA_TEXT = `(instance ${EXTRA_TERM} Entity)\n`;
const TWIN_TERM = "E2EUpgradeTwin";
const TWIN_TEXT = `(instance ${TWIN_TERM} Entity)\n`;
const LOCAL_MARKER = "E2ELocalEdit";

const opfsSafeName = (name) => encodeURIComponent(name);

// -- The two builds ------------------------------------------------------------

fs.rmSync(oldRoot, { recursive: true, force: true });
fs.mkdirSync(oldRoot, { recursive: true });
execSync(
  `git archive --format=tar ${JSON.stringify(OLD_REF)} packages/web | tar -x -C ${JSON.stringify(oldRoot)} --strip-components=2`,
  { cwd: repoRoot, stdio: ["ignore", "ignore", "inherit"] },
);
const oldSha = execSync(`git rev-parse --short ${JSON.stringify(OLD_REF)}`, {
  cwd: repoRoot,
})
  .toString()
  .trim();

let server = null;

/** Serve `root` on the shared port, replacing whatever was serving there. The
 *  origin never changes, so OPFS and localStorage carry across the swap. */
async function serve(root) {
  await server?.close();
  server = await createServer({
    root,
    logLevel: "error",
    server: { port: PORT, strictPort: true },
  });
  await server.listen();
  return server.resolvedUrls.local[0];
}

const HEADED = !!process.env.HEADED;
const SLOWMO = process.env.SLOWMO
  ? Number(process.env.SLOWMO)
  : HEADED
    ? 1000
    : undefined;

const browser = await chromium.launch({
  executablePath: process.env.PLAYWRIGHT_CHROMIUM || undefined,
  headless: !HEADED,
  slowMo: SLOWMO,
});

// Cytoscape's advisories (see smoke.mjs) come from the man page's taxonomy
// graph, which every citation check renders.
const IGNORED_CONSOLE = [
  /wheel sensitivity/,
  /Do not assign mappings/,
  /style value of `label`/,
  /status of 403/,
  /Invalid Semantic Tokens Data From Extension/,
];

let failures = 0;
const results = [];

// -- Page helpers ----------------------------------------------------------------

async function seedOpfs(page, entries) {
  await page.evaluate(async (entries) => {
    const root = await navigator.storage.getDirectory();
    for (const { dir, name, text } of entries) {
      const d = await root.getDirectoryHandle(dir, { create: true });
      const h = await d.getFileHandle(name, { create: true });
      const w = await h.createWritable();
      await w.write(text);
      await w.close();
    }
  }, entries);
}

async function readOpfs(page, dir, name) {
  return page.evaluate(
    async ([dir, name]) => {
      try {
        const root = await navigator.storage.getDirectory();
        const d = await root.getDirectoryHandle(dir);
        const h = await d.getFileHandle(name);
        return await (await h.getFile()).text();
      } catch {
        return null;
      }
    },
    [dir, name],
  );
}

/** `page.goto` that tolerates the one reload a freshly started Vite server
 *  can trigger while it optimizes dependencies -- the reloaded page is still
 *  what the caller waits for next. */
async function gotoApp(page, url) {
  try {
    await page.goto(url, { waitUntil: "domcontentloaded" });
  } catch (e) {
    if (!/ERR_ABORTED|interrupted by another navigation/.test(e.message))
      throw e;
  }
}

async function boot(page, base, query = "") {
  await gotoApp(page, base + query);
  await page.waitForSelector("nav.tabs", { timeout: BOOT_TIMEOUT });
  await page
    .locator("nav.tabs button", { hasText: "Knowledge base" })
    .first()
    .click();
  await page.waitForSelector(".src-report", { timeout: 60_000 });
}

/** Open `name` in the Edit tab and return the editor's visible text. */
async function editorText(page, name) {
  await page.locator("nav.tabs button", { hasText: "Edit" }).first().click();
  await page
    .getByRole("button", {
      name: "Open a file, or create a new one",
      exact: true,
    })
    .click();
  await page
    .locator(".open-list .open-file", { hasText: name })
    .first()
    .click();
  await page.waitForSelector(".edit-tab .view-lines", { timeout: 30_000 });
  await page.waitForTimeout(500);
  return (await page.locator(".edit-tab .view-lines").allInnerTexts()).join(
    "\n",
  );
}

/** Append a KIF comment to `name` through the UI and save it -- the real write
 *  path, which also flushes the KB snapshot cache. */
async function editAndSave(page, name, marker) {
  await editorText(page, name);
  await page.locator(".edit-tab .view-lines").first().click();
  await page.keyboard.press(
    process.platform === "darwin" ? "Meta+ArrowDown" : "Control+End",
  );
  await page.keyboard.press("Enter");
  await page.keyboard.type(`;; ${marker}`);
  await page
    .getByRole("button", {
      name: "Save to the in-browser knowledge base",
      exact: true,
    })
    .click();
  await page.locator("text=/Saved /").first().waitFor({ timeout: 30_000 });
}

/** The source citations the Browse man page shows for `term`, as `{ label,
 *  openable }` -- `openable` is whether the app resolved the cited file to a
 *  loaded constituent (a link) or not (plain text). */
async function citations(page, base, term) {
  await gotoApp(page, `${base}?sym=${encodeURIComponent(term)}&view=formulas`);
  await page.waitForSelector("nav.tabs", { timeout: BOOT_TIMEOUT });
  const refs = page.locator(".ref-loc");
  await refs.first().waitFor({ timeout: 60_000 });
  // Man-page sections render incrementally; let them settle.
  await page.waitForTimeout(500);
  return refs.evaluateAll((els) =>
    els.map((e) => ({
      label: e.textContent.trim(),
      openable: e.classList.contains("jump-src"),
    })),
  );
}

/** Exactly one citation for `term`, reading `file:line`, and it opens. */
async function expectCitedOnce(page, base, term, file, label) {
  const got = await citations(page, base, term);
  const want = `${file}:1`;
  if (got.length !== 1 || got[0].label !== want || !got[0].openable)
    throw new Error(
      `${label}: expected one openable citation ${want} for ${term}, got ` +
        JSON.stringify(got),
    );
}

function expectHas(text, needle, label) {
  if (!text.includes(needle))
    throw new Error(`${label}: expected ${JSON.stringify(needle)} in the file`);
}

// -- Case runner -----------------------------------------------------------------

/**
 * One browser context for the whole case, so storage survives the server swap.
 * `before` runs against the old build after `seed`; `after` against the new.
 */
async function runCase(name, { seed, before, after }) {
  const context = await browser.newContext({
    viewport: { width: 1200, height: 900 },
  });
  const problems = [];
  let phase = "old";
  const page = await context.newPage();
  page.on("console", (m) => {
    if (m.type() !== "error" && m.type() !== "warning") return;
    if (IGNORED_CONSOLE.some((re) => re.test(m.text()))) return;
    problems.push(`[${phase} console.${m.type()}] ${m.text()}`);
  });
  page.on("pageerror", (e) =>
    problems.push(`[${phase} pageerror] ${e.message}`),
  );
  const fetches = { merge: 0 };

  try {
    await page.route("https://api.github.com/**", (route) =>
      route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({ tree: [], truncated: false }),
      }),
    );
    await page.route(COMMITS_GLOB, (route) =>
      route.fulfill({
        contentType: "application/json",
        body: JSON.stringify([
          { sha: SHA, commit: { author: { date: new Date().toISOString() } } },
        ]),
      }),
    );
    await page.route(MERGE_RAW_URL, (route) => {
      fetches.merge += 1;
      return route.fulfill({ contentType: "text/plain", body: MERGE_TEXT });
    });
    await page.route(URL_SRC, (route) =>
      route.fulfill({ contentType: "text/plain", body: EXTRA_TEXT }),
    );

    let base = await serve(oldRoot);
    const seedUrl = new URL("__e2e_seed", base).href;
    await page.route(seedUrl, (route) =>
      route.fulfill({ contentType: "text/html", body: "<!doctype html>" }),
    );
    await page.goto(seedUrl, { waitUntil: "domcontentloaded" });
    await seed(page);
    await boot(page, base);
    await before(page, base);
    if (problems.length)
      throw new Error(`old build had problems: ${problems.join("; ")}`);

    phase = "new";
    base = await serve(webRoot);
    await after(page, base, fetches);
    if (problems.length)
      throw new Error(`page problems: ${problems.join("; ")}`);
    await page.screenshot({ path: path.join(shots, `upgrade-${name}.png`) });
    console.log(`ok   ${name}`);
    results.push({ name, ok: true });
  } catch (e) {
    failures += 1;
    await page
      .screenshot({ path: path.join(shots, `upgrade-${name}-FAIL.png`) })
      .catch(() => {});
    if (HEADED) await page.waitForTimeout(3000).catch(() => {});
    const msg = e.message.split("\n")[0];
    console.log(`FAIL ${name}: ${msg}`);
    results.push({ name, ok: false, msg });
  } finally {
    await context.close();
  }
}

function seedManifest(page, files, library) {
  return page.evaluate(
    ([keys, files, library]) => {
      localStorage.setItem(keys.wordnet, "false");
      localStorage.setItem(keys.files, JSON.stringify(files));
      if (library) localStorage.setItem(keys.library, JSON.stringify(library));
    },
    [
      {
        wordnet: WORDNET_ENABLED_KEY,
        files: SUMO_FILE_SETTING,
        library: LIBRARY_KEY,
      },
      files,
      library,
    ],
  );
}

const libraryEntry = (name, text) => ({
  kind: "file",
  name,
  size: text.length,
  added: Date.now(),
});

// -- 1. Repo file + upload, both edited, with a snapshot cache ---------------------
// The snapshot is the one piece of state whose meaning depends on how the code
// names files, so this case makes sure the old build left one behind.
await runCase("edited-upload-with-snapshot", {
  seed: async (page) => {
    await seedManifest(
      page,
      [
        { name: "Merge.kif", origin: SUMO_ORIGIN },
        { name: MINE, origin: { kind: "file" } },
      ],
      { repos: [], entries: [libraryEntry(MINE, MINE_TEXT)] },
    );
    await seedOpfs(page, [
      { dir: "library", name: opfsSafeName(MINE), text: MINE_TEXT },
    ]);
  },
  before: async (page) => {
    await editAndSave(page, "Merge.kif", `${LOCAL_MARKER}Merge`);
    await editAndSave(page, MINE, `${LOCAL_MARKER}Mine`);
    const meta = await readOpfs(page, "sumo-cache", "meta.json");
    if (!meta)
      throw new Error(
        "precondition: the old build wrote no snapshot cache, so this case " +
          "cannot tell whether the new build would wrongly restore it",
      );
  },
  after: async (page, base, fetches) => {
    // 1. Boot, and the old snapshot is not taken for a new one.
    fetches.merge = 0;
    await boot(page, base);
    await expectCitedOnce(page, base, MINE_TERM, `uploads/${MINE}`, "upgrade");

    // 2. Both edits survived.
    expectHas(
      await editorText(page, "Merge.kif"),
      `${LOCAL_MARKER}Merge`,
      "upgrade/Merge.kif",
    );
    expectHas(
      await editorText(page, MINE),
      `${LOCAL_MARKER}Mine`,
      "upgrade/Mine.kif",
    );

    // 3. A save after the upgrade replaces the file in place.
    await editAndSave(page, MINE, `${LOCAL_MARKER}Again`);
    await expectCitedOnce(
      page,
      base,
      MINE_TERM,
      `uploads/${MINE}`,
      "save-after-upgrade",
    );

    // 4. The new build's snapshot is what the next boot restores: a cache hit
    // never fetches a `sumo` file.
    const meta = JSON.parse(
      (await readOpfs(page, "sumo-cache", "meta.json")) || "{}",
    );
    if (!String(meta.fingerprint).startsWith("engine-names-2#"))
      throw new Error(
        `new-snapshot: cache fingerprint is ${JSON.stringify(meta.fingerprint)}`,
      );
    fetches.merge = 0;
    await boot(page, base);
    if (fetches.merge !== 0)
      throw new Error(
        `new-snapshot: the boot after the upgrade fetched Merge.kif ` +
          `${fetches.merge}x -- its own snapshot was not restored`,
      );
    await expectCitedOnce(
      page,
      base,
      MINE_TERM,
      `uploads/${MINE}`,
      "new-snapshot",
    );
    expectHas(
      await editorText(page, MINE),
      `${LOCAL_MARKER}Again`,
      "new-snapshot/Mine.kif",
    );

    // 5. An upload may now share a repo file's name.
    await seedOpfs(page, [
      { dir: "library", name: opfsSafeName("Merge.kif"), text: TWIN_TEXT },
    ]);
    await page.evaluate(
      ([keys, entry]) => {
        const files = JSON.parse(localStorage.getItem(keys.files));
        files.push({ name: "Merge.kif", origin: { kind: "file" } });
        localStorage.setItem(keys.files, JSON.stringify(files));
        const lib = JSON.parse(localStorage.getItem(keys.library));
        lib.entries.push(entry);
        localStorage.setItem(keys.library, JSON.stringify(lib));
      },
      [
        { files: SUMO_FILE_SETTING, library: LIBRARY_KEY },
        libraryEntry("Merge.kif", TWIN_TEXT),
      ],
    );
    await boot(page, base);
    await expectCitedOnce(
      page,
      base,
      TWIN_TERM,
      "uploads/Merge.kif",
      "same-name",
    );
    await expectCitedOnce(
      page,
      base,
      "E2EMergeStub",
      "Merge.kif",
      "same-name/repo",
    );
  },
});

// -- 2. A URL source ----------------------------------------------------------------
// No snapshot is ever taken with a URL source loaded, so the rename can only
// surface through what the running app resolves.
await runCase("url-source", {
  seed: async (page) => {
    await seedManifest(page, [
      { name: "Merge.kif", origin: SUMO_ORIGIN },
      { name: "Extra.kif", origin: URL_ORIGIN },
    ]);
  },
  before: async () => {},
  after: async (page, base) => {
    await boot(page, base);
    await expectCitedOnce(
      page,
      base,
      EXTRA_TERM,
      "e2e.invalid/Extra.kif",
      "url-upgrade",
    );
    expectHas(await editorText(page, "Extra.kif"), EXTRA_TERM, "url-upgrade");
  },
});

await browser.close();
await server?.close();
fs.rmSync(oldRoot, { recursive: true, force: true });

console.log(`\n  upgrading from ${OLD_REF} (${oldSha}) to the working tree`);
for (const r of results)
  console.log(
    `  ${r.ok ? "PASS" : "FAIL"}  ${r.name}${r.ok ? "" : ` -- ${r.msg}`}`,
  );

if (failures) {
  console.log(`\nupgrade e2e: ${failures}/${results.length} failed`);
  process.exit(1);
}
console.log("\nupgrade e2e: all cases passed");
