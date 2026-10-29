// Proof explanations in the browser, against a mocked /api: the Explain
// button's visibility (no API deployed, signed out, signed in), the request
// it sends, error wording, and rendering of the returned markdown. Proves
// the Ask/Tell default query once per phase; the KB boots from GitHub.
//
//   npm run test:e2e:explain --workspace @sigma/web

import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
import { createServer } from "vite";

const here = path.dirname(fileURLToPath(import.meta.url));
const BOOT_TIMEOUT = 300_000;
const server = await createServer({
  root: path.join(here, ".."),
  logLevel: "error",
});
await server.listen();
const base = server.resolvedUrls.local[0];
const browser = await chromium.launch({
  executablePath: process.env.PLAYWRIGHT_CHROMIUM || undefined,
});

/** "none" = no API deployed; else the mocked /api/me + /api/explain. */
let mode = "none";
let explainReply = null;
const posts = [];

try {
  const context = await browser.newContext({
    viewport: { width: 1200, height: 900 },
  });
  const page = await context.newPage();
  const problems = [];
  page.on("pageerror", (e) => problems.push(e.message));

  await context.route("**/api/me", (route) => {
    if (mode === "none") return route.fallback();
    if (mode === "signed-out")
      return route.fulfill({ status: 401, body: "Not logged in" });
    return route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({
        name: "Octo Cat",
        login: "octo",
        avatarUrl: "",
        token: "gho_test",
      }),
    });
  });
  await context.route("**/api/explain", async (route) => {
    if (mode === "none") return route.fallback();
    const req = route.request();
    if (req.method() === "GET")
      return route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({ enabled: true }),
      });
    posts.push({ body: req.postDataJSON(), headers: req.headers() });
    return route.fulfill(explainReply);
  });

  const explainBtn = page.locator(".explain button.btn");

  async function proveDefault() {
    await page.goto(base, { waitUntil: "domcontentloaded" });
    await page.waitForSelector("nav.tabs", { timeout: BOOT_TIMEOUT });
    await page
      .locator("nav.tabs button", { hasText: "Ask/Tell" })
      .first()
      .click();
    await page.waitForSelector(".monaco-editor", { timeout: 60_000 });
    await page.waitForTimeout(1500);
    await page.locator("button.btn", { hasText: /^Prove/ }).click();
    await page.waitForSelector("ol.refs li", { timeout: 120_000 });
    // explainAvailable() resolves after mount.
    await page.waitForTimeout(800);
  }

  // 1. No API deployed (plain vite): no button at all.
  await proveDefault();
  assert.equal(await page.locator(".explain").count(), 0, "hidden without API");
  console.log("ok   hidden when /api/explain is not deployed");

  // 2. Signed out: the button invites sign-in and sends nothing.
  mode = "signed-out";
  await proveDefault();
  await explainBtn.waitFor({ timeout: 10_000 });
  assert.equal(
    await explainBtn.getAttribute("title"),
    "Sign in with GitHub to explain proofs",
  );
  await explainBtn.click();
  await page.waitForSelector('dialog[open]:has-text("Connect to GitHub")', {
    timeout: 5000,
  });
  assert.equal(posts.length, 0, "signed out posts nothing");
  await page.keyboard.press("Escape");
  console.log("ok   signed out: sign-in prompt, no request");

  // 3. Signed in, rate limited: worded error, button stays usable.
  mode = "signed-in";
  explainReply = {
    status: 429,
    contentType: "application/json",
    headers: { "Retry-After": "60" },
    body: JSON.stringify({
      error: "rate_limited",
      message: "Too many requests.",
      retryAfter: 60,
    }),
  };
  await proveDefault();
  await explainBtn.waitFor({ timeout: 10_000 });
  await explainBtn.click();
  await page.waitForSelector('.explain [role="alert"]', { timeout: 10_000 });
  assert.match(
    await page.locator('.explain [role="alert"]').innerText(),
    /Too many explanations right now\. Try again in 1m 00s\./,
  );
  assert.equal(await explainBtn.isDisabled(), false);
  console.log("ok   rate-limit error is worded");

  // 4. Success: request shape, rendered markdown, memo on re-click.
  explainReply = {
    status: 200,
    contentType: "application/json",
    body: JSON.stringify({
      explanation:
        "The query asks whether **Rex is an animal**.\n\n- step 1 says Rex is a dog\n- <img src=x onerror=alert(1)>dogs are mammals",
      cached: false,
      model: "@cf/zai-org/glm-5.3-flash",
    }),
  };
  await explainBtn.click();
  await page.waitForSelector(".explain-md strong", { timeout: 10_000 });
  const sent = posts.at(-1);
  assert.equal(sent.body.kind, "ask");
  assert.equal(sent.body.query, "(instance Rex Animal)");
  assert.ok(sent.body.steps.length > 0);
  assert.ok(
    sent.body.steps.every(
      (s) => Number.isInteger(s.index) && typeof s.kif === "string",
    ),
  );
  assert.equal(sent.headers.authorization, "Bearer gho_test");
  assert.equal(
    await page.locator(".explain-md strong").innerText(),
    "Rex is an animal",
  );
  assert.equal(await page.locator(".explain-md img").count(), 0, "sanitized");
  assert.match(
    await page.locator(".explain-note").innerText(),
    /zai-org\/glm-5\.3-flash/,
  );
  assert.equal(await explainBtn.count(), 0, "button gives way to the answer");
  await page.screenshot({
    path: path.join(here, "shots", "explain-ask.png"),
    fullPage: true,
  });
  console.log("ok   explanation rendered and sanitized");

  assert.deepEqual(problems, []);
} finally {
  await browser.close();
  await server.close();
}
