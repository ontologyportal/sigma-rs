// Real WASM engine with deterministic GitHub inputs; no remote writes.
// Run: node packages/web/e2e/branches.mjs
import assert from "node:assert/strict";
import { mkdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
import { createServer } from "vite";

const longBranch = "work/" + "a-very-long-ontology-branch-name-".repeat(4);
const root = fileURLToPath(new URL("../", import.meta.url));
const server = await createServer({ root, logLevel: "error" });
await server.listen();
const browser = await chromium.launch({
  executablePath: process.env.PLAYWRIGHT_CHROMIUM || undefined,
});
try {
  const page = await browser.newPage({
    viewport: { width: 1280, height: 900 },
  });
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.route("https://api.github.com/**", (route) => {
    const url = new URL(route.request().url());
    const data = url.pathname.endsWith("/branches")
      ? ["master", "work/ontology", "missing/files", longBranch].map(
          (name) => ({ name }),
        )
      : url.pathname.includes("/commits")
        ? [
            {
              sha: "1111111111111111111111111111111111111111",
              commit: { author: { date: "2026-01-01T00:00:00Z" } },
            },
          ]
        : url.pathname.includes("/git/trees/")
          ? {
              tree: [
                { path: "Merge.kif", type: "blob", sha: "blob", size: 30 },
              ],
            }
          : [];
    return route.fulfill({ json: data });
  });
  let failDownloads = false;
  await page.route("https://raw.githubusercontent.com/**", (route) =>
    route.fulfill({
      status: failDownloads ? 404 : 200,
      body: "(instance BranchExample Entity)\n",
    }),
  );
  await page.route("**/api/me", (route) =>
    route.fulfill({ json: { user: null } }),
  );
  await page.addInitScript(() => {
    if (localStorage.getItem("sumoFiles")) return;
    localStorage.setItem(
      "sumoFiles",
      JSON.stringify([
        {
          name: "Merge.kif",
          origin: {
            kind: "sumo",
            owner: "ontologyportal",
            repo: "sumo",
            branch: "master",
          },
        },
      ]),
    );
    localStorage.setItem("sumoBrowserWordNetEnabled", "false");
  });
  await page.goto(new URL("kb", server.resolvedUrls.local[0]).href);
  const button = page.getByRole("button", { name: /^Branch:/ });
  await button.waitFor({ timeout: 120000 });
  const importButton = page.getByRole("button", {
    name: "Import files",
    exact: true,
  });
  assert.ok(
    (await importButton.boundingBox()).x < (await button.boundingBox()).x,
  );
  await importButton.click();
  const dialog = page.locator("dialog[open]");
  await dialog
    .getByRole("button", { name: "Files & folders", exact: true })
    .waitFor();
  assert.equal(
    await dialog.getByRole("button", { name: "GitLab", exact: true }).count(),
    0,
  );
  await dialog.getByRole("button", { name: "File URL", exact: true }).click();
  await dialog.getByLabel("Address of a .kif file").waitFor();
  await dialog.getByRole("button", { name: "Close", exact: true }).click();
  assert.equal(
    await page
      .getByRole("button", { name: "Add repository", exact: true })
      .count(),
    0,
  );
  await page
    .getByRole("button", { name: "Switch repository", exact: true })
    .click();
  const repoDialog = page.locator("dialog[open]");
  assert.equal(
    await repoDialog
      .getByRole("tab", { name: "GitHub", exact: true })
      .getAttribute("aria-selected"),
    "true",
  );
  assert.ok(
    await repoDialog
      .getByRole("tab", { name: "GitLab", exact: true })
      .isDisabled(),
  );
  await repoDialog.getByLabel("Repository", { exact: true }).fill("alice/sumo");
  assert.equal(await repoDialog.getByLabel("Starting branch").count(), 0);
  const dismissed = new Promise((resolve) =>
    page.once("dialog", async (confirmation) => {
      await confirmation.dismiss();
      resolve();
    }),
  );
  await repoDialog
    .getByRole("button", { name: "Switch repository", exact: true })
    .click();
  await dismissed;
  assert.match(await button.innerText(), /master/);
  page.once("dialog", (confirmation) => confirmation.accept());
  await repoDialog
    .getByRole("button", { name: "Switch repository", exact: true })
    .click();
  await page.waitForFunction(
    () =>
      JSON.parse(localStorage.getItem("sumoFiles"))[0]?.origin.owner ===
      "alice",
  );
  assert.match(await button.innerText(), /master/);
  await button.click();
  page.once("dialog", (confirmation) => confirmation.accept());
  await page
    .getByRole("menuitemradio", { name: "work/ontology", exact: true })
    .click();
  await page.waitForFunction(
    () =>
      JSON.parse(localStorage.getItem("sumoFiles"))[0]?.origin.branch ===
      "work/ontology",
  );
  await page
    .getByRole("button", { name: "Branch: work/ontology", exact: false })
    .waitFor();
  await page.waitForFunction(
    () => JSON.parse(localStorage.getItem("sumoLibrary")).repos.length === 1,
  );
  const selectedSource = await page.evaluate(
    () => JSON.parse(localStorage.getItem("sumoLibrary")).repos[0],
  );
  assert.deepEqual(selectedSource, {
    owner: "alice",
    repo: "sumo",
    branch: "work/ontology",
  });
  assert.equal(
    await page.locator("tbody").getByText("GitHub", { exact: true }).count(),
    0,
  );
  await page.reload();
  await button.waitFor({ timeout: 120000 });
  assert.match(await button.innerText(), /work\/ontology/);
  const before = await page.evaluate(() => localStorage.getItem("sumoFiles"));
  failDownloads = true;
  await button.click();
  page.once("dialog", (dialog) => dialog.accept());
  await page
    .getByRole("menuitemradio", { name: "missing/files", exact: true })
    .click();
  await page
    .getByText(/HTTP 404/)
    .first()
    .waitFor();
  assert.equal(
    await page.evaluate(() => localStorage.getItem("sumoFiles")),
    before,
  );
  assert.match(await button.innerText(), /work\/ontology/);
  await button.click();
  await page
    .getByRole("menuitemradio", { name: "master", exact: true })
    .waitFor();
  failDownloads = false;
  page.once("dialog", (dialog) => dialog.accept());
  await page
    .getByRole("menuitemradio", { name: longBranch, exact: true })
    .click();
  await page.waitForFunction(
    (name) => document.querySelector(".branch-button")?.title === name,
    longBranch,
  );
  const truncation = await page.locator(".branch-name").evaluate((element) => ({
    overflow: element.scrollWidth > element.clientWidth,
    ellipsis: getComputedStyle(element).textOverflow,
  }));
  assert.equal(truncation.overflow, true);
  assert.equal(truncation.ellipsis, "ellipsis");
  const shots = fileURLToPath(new URL("./shots/", import.meta.url));
  mkdirSync(shots, { recursive: true });
  await page.screenshot({ path: `${shots}/branches.png`, fullPage: true });
  assert.deepEqual(errors, []);
  console.log(
    "PASS: cancel, switch fork branch, restore after reload, and preserve KB on failed download",
  );
} finally {
  await browser.close();
  await server.close();
}
