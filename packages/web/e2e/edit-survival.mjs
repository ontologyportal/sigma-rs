// Browser-driven check that LOCAL WORK IS NEVER LOST to an upstream update.
// Everything else about the update feature (preference cycling, the review
// dialog, alert dismissal) is covered by kb-update-prefs.mjs; this file cares
// about exactly one property: an edit the user made, or a file the user added,
// still holds their content after a refresh or an upstream change.
//
//   npm run test:e2e:edit-survival --workspace @sigma/web
//
// Each case seeds localStorage AND OPFS directly rather than driving the UI to
// create its own fixtures -- the UI path is slow, and a seeding bug there would
// show up as a survival failure here. Assertions are made against the OPFS
// bytes (hashed independently in Node, so a wrong app-side hash cannot mask a
// wrong file) as well as against what the editor actually shows.
//
// Requires Playwright's Chromium (`npx playwright install chromium`), or set
// PLAYWRIGHT_CHROMIUM to an existing Chromium executable. BASE_URL skips the
// built-in server and targets a running deployment instead. HEADED=1 runs with
// a visible browser window (SLOWMO=<ms> adds a per-action delay).

import { chromium } from "playwright";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const shots = path.join(here, "shots");
fs.mkdirSync(shots, { recursive: true });

// Every constituent is mocked and WordNet is seeded off, so boot has no real
// network to wait on -- but a cold wasm compile still dominates the first load.
const BOOT_TIMEOUT = 180_000;

const COMMITS_URL =
  "https://api.github.com/repos/ontologyportal/sumo/commits?per_page=1";
const MERGE_RAW_URL =
  "https://raw.githubusercontent.com/ontologyportal/sumo/master/Merge.kif";
// Routed by Playwright before any DNS lookup, so it never leaves the process.
const URL_SRC = "https://e2e.invalid/Extra.kif";

const SUMO_ORIGIN = {
  kind: "sumo",
  owner: "ontologyportal",
  repo: "sumo",
  branch: "master",
};
const SUMO_ORIGIN_ID = "github:ontologyportal/sumo@master";
const URL_ORIGIN = { kind: "url", url: URL_SRC };
const URL_ORIGIN_ID = `url:${URL_SRC}`;

const SUMO_FILE_SETTING = "sumoFiles";
const LIBRARY_KEY = "sumoLibrary";
const EDITS_KEY = "sumoBrowserEdits";
const UPDATE_PREFS_KEY = "sumoBrowserUpdatePrefs";
const UPDATE_BASELINES_KEY = "sumoBrowserUpdateBaselines";
const WORDNET_ENABLED_KEY = "sumoBrowserWordNetEnabled";

const SHA_V1 = "1111111111111111111111111111111111111aaa";
const SHA_V2 = "2222222222222222222222222222222222222bbb";

// Distinct markers so an assertion can tell WHOSE content it is looking at.
const LOCAL_MARKER = "E2ELocalEdit";
const UPSTREAM_MARKER = "E2EUpstreamChange";

const MERGE_V1 = "(instance E2EMergeStub Entity)\n";
const MERGE_V2 = `(instance E2EMergeStub Entity)\n(instance ${UPSTREAM_MARKER} Entity)\n`;
const MERGE_EDITED = `(instance E2EMergeStub Entity)\n(instance ${LOCAL_MARKER} Entity)\n`;

const EXTRA_V1 = "(instance E2EExtraStub Entity)\n";
const EXTRA_V2 = `(instance E2EExtraStub Entity)\n(instance ${UPSTREAM_MARKER} Entity)\n`;
// What `editAndSave` leaves behind: EXTRA_V1's trailing newline puts the cursor
// on an empty line 2, and the marker is appended after that.
const EXTRA_EDITED = `${EXTRA_V1}\n;; ${LOCAL_MARKER}`;
// `edits/` prefixes every non-sumo origin (editFileName, src/stores/changes.ts).
const URL_EDIT_ENTRY = "url:Extra.kif";

const NEW_FILE = "E2EMine.kif";
const NEW_FILE_TEXT = `(instance ${LOCAL_MARKER}Mine Entity)\n`;

/** Git blob SHA-1, reimplemented here on purpose: the app computes its own in
 *  `blobSha` (src/stores/changes.ts), and a test that reused it could not tell
 *  a wrong hash from a wrong file. */
function blobSha(text) {
  const body = Buffer.from(text, "utf8");
  return crypto
    .createHash("sha1")
    .update(Buffer.concat([Buffer.from(`blob ${body.length}\0`, "utf8"), body]))
    .digest("hex");
}

/** OPFS names are one path component, so the app encodes the constituent name
 *  (`opfsSafeName` in src/stores/changes.ts). */
const opfsSafeName = (name) => encodeURIComponent(name);

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
if (!base.endsWith("/")) base += "/";
// Same origin as the app, so OPFS written here is the OPFS the app boots into,
// but served as a blank page so seeding runs before any app code does.
const SEED_URL = new URL("__e2e_seed", base).href;

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

const IGNORED_CONSOLE = [
  /wheel sensitivity/,
  /status of 403/,
  /Invalid Semantic Tokens Data From Extension/,
];

let failures = 0;
const results = [];

// -- OPFS helpers, run in the page ------------------------------------------

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

/** The file's text, or null when the directory or file is absent. */
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

/** Assert an OPFS file's bytes hash to exactly `expected`'s hash. */
async function expectOpfsContent(page, dir, name, expected, label) {
  const got = await readOpfs(page, dir, opfsSafeName(name));
  if (got === null)
    throw new Error(`${label}: ${dir}/${name} is missing from OPFS entirely`);
  const want = blobSha(expected);
  const have = blobSha(got);
  if (have !== want)
    throw new Error(
      `${label}: ${dir}/${name} content hash ${have} != expected ${want} ` +
        `(on disk: ${JSON.stringify(got)})`,
    );
}

// -- UI helpers --------------------------------------------------------------

/** Open `name` in the Edit tab and return the editor's visible text. */
async function editorText(page, name) {
  await page.locator("nav.tabs button", { hasText: "Edit" }).first().click();
  await page
    .getByRole("button", {
      name: "Open a file, or create a new one",
      exact: true,
    })
    .click();
  await page.locator(".open-list .open-file", { hasText: name }).click();
  await page.waitForSelector(".edit-tab .view-lines", { timeout: 30_000 });
  // Monaco renders asynchronously after the file is swapped in.
  await page.waitForTimeout(500);
  return (await page.locator(".edit-tab .view-lines").allInnerTexts()).join(
    "\n",
  );
}

async function waitDialogGone(page) {
  await page.waitForFunction(
    () => !document.querySelector("dialog[open]"),
    null,
    { timeout: 15_000 },
  );
}

function expectMarkers(text, { has, lacks }, label) {
  if (has && !text.includes(has))
    throw new Error(`${label}: editor lost the local content (${has})`);
  if (lacks && text.includes(lacks))
    throw new Error(`${label}: upstream content (${lacks}) clobbered the file`);
}

/**
 * Make a real edit through the UI and save it, so the case exercises the same
 * write path a user does rather than a pre-seeded fixture. The marker is a KIF
 * COMMENT on purpose: Monaco auto-closes brackets, so typing `(instance ...)`
 * would land a stray `)` in the buffer.
 */
async function editAndSave(page, name) {
  await editorText(page, name); // opens the file in the Edit tab
  await page.locator(".edit-tab .view-lines").first().click();
  await page.keyboard.press(
    process.platform === "darwin" ? "Meta+ArrowDown" : "Control+End",
  );
  await page.keyboard.press("Enter");
  await page.keyboard.type(`;; ${LOCAL_MARKER}`);
  await page
    .getByRole("button", {
      name: "Save to the in-browser knowledge base",
      exact: true,
    })
    .click();
  await page.locator("text=/Saved /").first().waitFor({ timeout: 30_000 });
}

// -- Case runner -------------------------------------------------------------

/**
 * `seed` gets a page already parked on a blank same-origin page, so it can
 * write localStorage and OPFS before the app exists. `check` runs once the KB
 * tab is up.
 */
async function runCase(name, { routes, seed, check }) {
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
    // Nothing may reach the real GitHub API. Beyond keeping cases hermetic,
    // anonymous requests are capped at 60/hour: exhausting the quota makes the
    // app raise a login dialog over the whole UI, and every later case then
    // fails on an unrelated click timeout. An empty tree is the benign answer
    // -- no upstream SHAs means no stale flags and no records dropped.
    // Playwright matches routes newest-first, so the per-case ones below win.
    await page.route("https://api.github.com/**", (route) =>
      route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({ tree: [], truncated: false }),
      }),
    );
    await page.route(SEED_URL, (route) =>
      route.fulfill({ contentType: "text/html", body: "<!doctype html>" }),
    );
    await routes?.(page);
    await page.goto(SEED_URL, { waitUntil: "domcontentloaded" });
    await seed(page);

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
    await page.screenshot({ path: path.join(shots, `survive-${name}.png`) });
    if (HEADED) await page.waitForTimeout(1500);
    console.log(`ok   ${name}`);
    results.push({ name, ok: true });
  } catch (e) {
    failures += 1;
    await page
      .screenshot({ path: path.join(shots, `survive-${name}-FAIL.png`) })
      .catch(() => {});
    if (HEADED) await page.waitForTimeout(3000).catch(() => {});
    const msg = e.message.split("\n")[0];
    console.log(`FAIL ${name}: ${msg}`);
    results.push({ name, ok: false, msg });
  } finally {
    await context.close();
  }
}

/** localStorage seeding shared by every case. `files` is the saved-constituent
 *  manifest; the rest are optional per-case overlays. */
function seedLocalStorage(page, { files, edits, prefs, baselines, library }) {
  return page.evaluate(
    ([keys, payload]) => {
      localStorage.setItem(keys.wordnet, "false");
      localStorage.setItem(keys.files, JSON.stringify(payload.files));
      if (payload.edits)
        localStorage.setItem(keys.edits, JSON.stringify(payload.edits));
      if (payload.prefs)
        localStorage.setItem(keys.prefs, JSON.stringify(payload.prefs));
      if (payload.baselines)
        localStorage.setItem(keys.baselines, JSON.stringify(payload.baselines));
      if (payload.library)
        localStorage.setItem(keys.library, JSON.stringify(payload.library));
    },
    [
      {
        wordnet: WORDNET_ENABLED_KEY,
        files: SUMO_FILE_SETTING,
        edits: EDITS_KEY,
        prefs: UPDATE_PREFS_KEY,
        baselines: UPDATE_BASELINES_KEY,
        library: LIBRARY_KEY,
      },
      { files, edits, prefs, baselines, library },
    ],
  );
}

/** A tracked-edit record, as `recordSave` (src/stores/changes.ts) writes it. */
function editRecord(name, origin, base, saved) {
  return {
    name,
    origin,
    path: name,
    baseBlobSha: blobSha(base),
    savedBlobSha: blobSha(saved),
    savedAt: Date.now(),
    proposed: null,
    prClosed: null,
  };
}

function gitRoutes(page, versionRef) {
  return Promise.all([
    page.route(COMMITS_URL, (route) =>
      route.fulfill({
        contentType: "application/json",
        body: JSON.stringify([
          {
            sha: versionRef.v === 1 ? SHA_V1 : SHA_V2,
            commit: { author: { date: new Date().toISOString() } },
          },
        ]),
      }),
    ),
    page.route(MERGE_RAW_URL, (route) =>
      route.fulfill({
        contentType: "text/plain",
        body: versionRef.v === 1 ? MERGE_V1 : MERGE_V2,
      }),
    ),
  ]);
}

function urlRoutes(page, versionRef) {
  return page.route(URL_SRC, (route) =>
    route.fulfill({
      contentType: "text/plain",
      body: versionRef.v === 1 ? EXTRA_V1 : EXTRA_V2,
    }),
  );
}

/** Auto-update is silent; its only completion signal is the alert it pushes
 *  (src/stores/kb.ts). Waiting on it is what stops a survival assertion from
 *  passing vacuously, by running BEFORE the update it is supposed to survive. */
async function waitForUpdateApplied(page, hasText) {
  await page
    .locator(".src-alert", { hasText })
    .first()
    .waitFor({ timeout: 60_000 });
}

// -- 1. A file the user added is untouched by an upstream change -------------
// Both upstream kinds move at once: neither a git commit nor a changed URL has
// any business rewriting or dropping a file that came from neither.
await runCase("new-file-untouched-by-upstream", {
  routes: async (page) => {
    const v = { v: 2 }; // upstream is already ahead of the seeded baselines
    await gitRoutes(page, v);
    await urlRoutes(page, v);
  },
  seed: async (page) => {
    await seedLocalStorage(page, {
      files: [
        { name: "Merge.kif", origin: SUMO_ORIGIN },
        { name: "Extra.kif", origin: URL_ORIGIN },
        { name: NEW_FILE, origin: { kind: "file" } },
      ],
      prefs: {
        [SUMO_ORIGIN_ID]: "auto-update",
        [URL_ORIGIN_ID]: "auto-update",
      },
      baselines: {
        [SUMO_ORIGIN_ID]: SHA_V1,
        [`url:Extra.kif`]: blobSha(EXTRA_V1),
      },
      library: {
        repos: [],
        entries: [
          {
            kind: "file",
            name: NEW_FILE,
            size: NEW_FILE_TEXT.length,
            added: Date.now(),
          },
        ],
      },
    });
    await seedOpfs(page, [
      { dir: "library", name: opfsSafeName(NEW_FILE), text: NEW_FILE_TEXT },
    ]);
  },
  check: async (page) => {
    await waitForUpdateApplied(page, "updated");

    // The bytes on disk are the real claim; the editor is the corroboration.
    await expectOpfsContent(
      page,
      "library",
      NEW_FILE,
      NEW_FILE_TEXT,
      "new-file",
    );
    const text = await editorText(page, NEW_FILE);
    expectMarkers(
      text,
      { has: `${LOCAL_MARKER}Mine`, lacks: UPSTREAM_MARKER },
      "new-file",
    );
  },
});

// -- 2a. A sumo-sourced edit survives a plain refresh ------------------------
await runCase("sumo-edit-survives-refresh", {
  routes: async (page) => {
    await gitRoutes(page, { v: 1 }); // upstream does NOT move in this case
  },
  seed: async (page) => {
    await seedLocalStorage(page, {
      files: [{ name: "Merge.kif", origin: SUMO_ORIGIN }],
      edits: {
        "sumo:Merge.kif": editRecord(
          "Merge.kif",
          "sumo",
          MERGE_V1,
          MERGE_EDITED,
        ),
      },
      prefs: { [SUMO_ORIGIN_ID]: "auto-update" },
      baselines: { [SUMO_ORIGIN_ID]: SHA_V1 },
    });
    await seedOpfs(page, [
      { dir: "edits", name: opfsSafeName("Merge.kif"), text: MERGE_EDITED },
    ]);
  },
  check: async (page) => {
    await expectOpfsContent(
      page,
      "edits",
      "Merge.kif",
      MERGE_EDITED,
      "sumo-refresh/before",
    );
    expectMarkers(
      await editorText(page, "Merge.kif"),
      { has: LOCAL_MARKER },
      "sumo-refresh/before",
    );

    await page.reload({ waitUntil: "domcontentloaded" });
    await page.waitForSelector("nav.tabs", { timeout: BOOT_TIMEOUT });

    await expectOpfsContent(
      page,
      "edits",
      "Merge.kif",
      MERGE_EDITED,
      "sumo-refresh/after",
    );
    expectMarkers(
      await editorText(page, "Merge.kif"),
      { has: LOCAL_MARKER },
      "sumo-refresh/after",
    );
  },
});

// -- 2b. A url-sourced edit survives a plain refresh -------------------------
// Made through the UI rather than seeded, so this also covers the write half:
// that Save is offered for a url buffer at all, and that it persists.
await runCase("url-edit-survives-refresh", {
  routes: (page) => urlRoutes(page, { v: 1 }), // upstream does NOT move here
  seed: (page) =>
    seedLocalStorage(page, {
      files: [{ name: "Extra.kif", origin: URL_ORIGIN }],
      prefs: { [URL_ORIGIN_ID]: "auto-update" },
      baselines: { [`url:Extra.kif`]: blobSha(EXTRA_V1) },
    }),
  check: async (page) => {
    await editAndSave(page, "Extra.kif");
    await expectOpfsContent(
      page,
      "edits",
      URL_EDIT_ENTRY,
      EXTRA_EDITED,
      "url-refresh/before",
    );

    await page.reload({ waitUntil: "domcontentloaded" });
    await page.waitForSelector("nav.tabs", { timeout: BOOT_TIMEOUT });

    await expectOpfsContent(
      page,
      "edits",
      URL_EDIT_ENTRY,
      EXTRA_EDITED,
      "url-refresh/after",
    );
    expectMarkers(
      await editorText(page, "Extra.kif"),
      { has: LOCAL_MARKER },
      "url-refresh/after",
    );
  },
});

// -- 3. A sumo-sourced edit is not trashed by a git upstream change ----------
await runCase("sumo-edit-survives-git-update", {
  routes: async (page) => {
    await gitRoutes(page, { v: 2 }); // upstream ahead of the seeded baseline
  },
  seed: async (page) => {
    await seedLocalStorage(page, {
      files: [{ name: "Merge.kif", origin: SUMO_ORIGIN }],
      edits: {
        "sumo:Merge.kif": editRecord(
          "Merge.kif",
          "sumo",
          MERGE_V1,
          MERGE_EDITED,
        ),
      },
      prefs: { [SUMO_ORIGIN_ID]: "auto-update" },
      baselines: { [SUMO_ORIGIN_ID]: SHA_V1 },
    });
    await seedOpfs(page, [
      { dir: "edits", name: opfsSafeName("Merge.kif"), text: MERGE_EDITED },
    ]);
  },
  check: async (page) => {
    await waitForUpdateApplied(page, "updated");

    await expectOpfsContent(
      page,
      "edits",
      "Merge.kif",
      MERGE_EDITED,
      "sumo-vs-git",
    );
    expectMarkers(
      await editorText(page, "Merge.kif"),
      { has: LOCAL_MARKER, lacks: UPSTREAM_MARKER },
      "sumo-vs-git",
    );
  },
});

// -- 4. A sumo edit survives a url source updating beside it -----------------
// The nearest real form of "a url upstream change must not trash local work":
// a url source moving has no business touching an edit that belongs to a
// different source. "Update now" would route through review even under
// auto-update (`manual` suppresses the silent branch), so the move is staged
// and the page reloaded -- the background check on the next boot is what
// exercises the silent apply.
const urlUpdateVersion = { v: 1 };
await runCase("sumo-edit-survives-url-update", {
  routes: async (page) => {
    await gitRoutes(page, { v: 1 }); // git stands still; only the url moves
    await urlRoutes(page, urlUpdateVersion);
  },
  seed: async (page) => {
    await seedLocalStorage(page, {
      files: [
        { name: "Merge.kif", origin: SUMO_ORIGIN },
        { name: "Extra.kif", origin: URL_ORIGIN },
      ],
      edits: {
        "sumo:Merge.kif": editRecord(
          "Merge.kif",
          "sumo",
          MERGE_V1,
          MERGE_EDITED,
        ),
      },
      prefs: {
        [SUMO_ORIGIN_ID]: "auto-update",
        [URL_ORIGIN_ID]: "auto-update",
      },
      baselines: {
        [SUMO_ORIGIN_ID]: SHA_V1,
        [`url:Extra.kif`]: blobSha(EXTRA_V1),
      },
    });
    await seedOpfs(page, [
      { dir: "edits", name: opfsSafeName("Merge.kif"), text: MERGE_EDITED },
    ]);
  },
  check: async (page) => {
    urlUpdateVersion.v = 2; // the url source moves out from under the edit
    await page.reload({ waitUntil: "domcontentloaded" });
    await page.waitForSelector("nav.tabs", { timeout: BOOT_TIMEOUT });
    await page
      .locator("nav.tabs button", { hasText: "Knowledge base" })
      .first()
      .click();
    await waitForUpdateApplied(page, "updated");

    await expectOpfsContent(
      page,
      "edits",
      "Merge.kif",
      MERGE_EDITED,
      "sumo-vs-url",
    );
    expectMarkers(
      await editorText(page, "Merge.kif"),
      { has: LOCAL_MARKER, lacks: UPSTREAM_MARKER },
      "sumo-vs-url",
    );
  },
});

// -- 5. A url-sourced edit is not trashed by a url upstream change -----------
// The edited file must NOT take the silent auto-update branch: the upstream
// change is offered for review instead, and the local copy stands until the
// user accepts it.
const urlVsUrlVersion = { v: 1 };
await runCase("url-edit-survives-url-update", {
  routes: (page) => urlRoutes(page, urlVsUrlVersion),
  seed: (page) =>
    seedLocalStorage(page, {
      files: [{ name: "Extra.kif", origin: URL_ORIGIN }],
      prefs: { [URL_ORIGIN_ID]: "auto-update" },
      baselines: { [`url:Extra.kif`]: blobSha(EXTRA_V1) },
    }),
  check: async (page) => {
    await editAndSave(page, "Extra.kif");

    urlVsUrlVersion.v = 2; // upstream moves while the edit is unpushed
    await page.reload({ waitUntil: "domcontentloaded" });
    await page.waitForSelector("nav.tabs", { timeout: BOOT_TIMEOUT });
    await page
      .locator("nav.tabs button", { hasText: "Knowledge base" })
      .first()
      .click();
    // Offered for review, never applied over the edit.
    const alert = page.locator(".src-alert", { hasText: "Extra.kif" }).first();
    await alert.waitFor({ timeout: 60_000 });
    await alert.locator("button.src-alert-review").click();

    const dialog = page.locator('dialog[open]:has-text("Review update")');
    await dialog.waitFor({ timeout: 15_000 });
    await page.waitForFunction(
      () =>
        document.querySelector(".monaco-diff-editor")?.textContent?.length > 0,
      null,
      { timeout: 15_000 },
    );
    // Both sides must be real: upstream's new line on the left, the unpushed
    // local edit on the right. A diff showing only one of them would mean the
    // dialog opened on the wrong pair of texts.
    const diffText = await page.locator(".monaco-diff-editor").innerText();
    if (!diffText.includes(UPSTREAM_MARKER))
      throw new Error(`url-vs-url: diff is missing upstream's change`);
    if (!diffText.includes(LOCAL_MARKER))
      throw new Error(`url-vs-url: diff is missing the local edit`);

    // Skip == keep mine. The edit must be exactly as it was.
    await dialog.locator("button.btn", { hasText: "Skip" }).click();
    await waitDialogGone(page);

    await expectOpfsContent(
      page,
      "edits",
      URL_EDIT_ENTRY,
      EXTRA_EDITED,
      "url-vs-url",
    );
    expectMarkers(
      await editorText(page, "Extra.kif"),
      { has: LOCAL_MARKER, lacks: UPSTREAM_MARKER },
      "url-vs-url",
    );
  },
});

// -- 6. Accepting upstream from the review dialog actually sticks ------------
// The other half of the same flow: when the user DOES take upstream, the edit
// it replaces has to stop being tracked. Left in place, `fromOrigin` would
// prefer it on the next boot and quietly undo the update just accepted.
const urlAcceptVersion = { v: 1 };
await runCase("url-accepting-upstream-sticks", {
  routes: (page) => urlRoutes(page, urlAcceptVersion),
  seed: (page) =>
    seedLocalStorage(page, {
      files: [{ name: "Extra.kif", origin: URL_ORIGIN }],
      prefs: { [URL_ORIGIN_ID]: "auto-update" },
      baselines: { [`url:Extra.kif`]: blobSha(EXTRA_V1) },
    }),
  check: async (page) => {
    await editAndSave(page, "Extra.kif");

    urlAcceptVersion.v = 2;
    await page.reload({ waitUntil: "domcontentloaded" });
    await page.waitForSelector("nav.tabs", { timeout: BOOT_TIMEOUT });
    await page
      .locator("nav.tabs button", { hasText: "Knowledge base" })
      .first()
      .click();

    const alert = page.locator(".src-alert", { hasText: "Extra.kif" }).first();
    await alert.waitFor({ timeout: 60_000 });
    await alert.locator("button.src-alert-review").click();
    const dialog = page.locator('dialog[open]:has-text("Review update")');
    await dialog.waitFor({ timeout: 15_000 });
    await dialog.locator("button.btn", { hasText: "Apply" }).click();
    await waitDialogGone(page);

    expectMarkers(
      await editorText(page, "Extra.kif"),
      { has: UPSTREAM_MARKER, lacks: LOCAL_MARKER },
      "url-accept",
    );
    if ((await readOpfs(page, "edits", opfsSafeName(URL_EDIT_ENTRY))) !== null)
      throw new Error(
        "url-accept: the superseded edit is still in OPFS -- it will outrank " +
          "upstream on the next boot and undo the update",
      );

    // The boot after is what the stale-record bug would actually show up in.
    await page.reload({ waitUntil: "domcontentloaded" });
    await page.waitForSelector("nav.tabs", { timeout: BOOT_TIMEOUT });
    expectMarkers(
      await editorText(page, "Extra.kif"),
      { has: UPSTREAM_MARKER, lacks: LOCAL_MARKER },
      "url-accept/after-reload",
    );
  },
});

await browser.close();
await server?.close();

console.log("");
for (const r of results)
  console.log(
    `  ${r.ok ? "PASS" : "FAIL"}  ${r.name}${r.ok ? "" : ` -- ${r.msg}`}`,
  );

if (failures) {
  console.log(`\nedit-survival e2e: ${failures}/${results.length} failed`);
  process.exit(1);
}
console.log("\nedit-survival e2e: all cases passed");
