/** Build first; exercise an untouched production session after stopping its server. */
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile, readdir } from "node:fs/promises";
import { extname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";

const dist = fileURLToPath(
  new URL(`../${process.env.WEB_DIST || "dist"}/`, import.meta.url),
);
const html = await readFile(join(dist, "index.html"), "utf8");
const base = html.match(/src="([^"]*)assets\/index-[^"]+\.js"/)?.[1] || "/";
const types = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".mjs": "text/javascript",
  ".css": "text/css",
  ".wasm": "application/wasm",
  ".json": "application/json",
  ".ttf": "font/ttf",
  ".png": "image/png",
  ".gif": "image/gif",
  ".ico": "image/x-icon",
};
let releaseBackground;
let markBackgroundStarted;
const backgroundGate = new Promise((resolve) => {
  releaseBackground = resolve;
});
const backgroundStarted = new Promise((resolve) => {
  markBackgroundStarted = resolve;
});
const server = createServer(async (req, res) => {
  const path = new URL(req.url, "http://localhost").pathname;
  res.setHeader("Cross-Origin-Opener-Policy", "same-origin");
  res.setHeader("Cross-Origin-Embedder-Policy", "require-corp");
  res.setHeader("Cache-Control", "no-store");
  if (path === "/api/me") {
    res.writeHead(401, { "Content-Type": "application/json" });
    res.end("{}");
    return;
  }
  const relative = path.startsWith(base)
    ? path.slice(base.length)
    : path.slice(1);
  if (
    relative === "apple-touch-icon.png" &&
    req.headers["sec-fetch-dest"] === "empty"
  ) {
    markBackgroundStarted();
    await backgroundGate;
  }
  try {
    const file = relative || "index.html";
    res.setHeader(
      "Content-Type",
      types[extname(file)] || "application/octet-stream",
    );
    res.end(await readFile(join(dist, file)));
  } catch {
    res.writeHead(404);
    res.end();
  }
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const origin = `http://127.0.0.1:${server.address().port}`;
const browser = await chromium.launch({
  executablePath: process.env.PLAYWRIGHT_CHROMIUM || undefined,
});
try {
  const context = await browser.newContext();
  await context.addInitScript(() => {
    localStorage.setItem("sumoFiles", "[]");
    localStorage.setItem("sumoTests", "[]");
    localStorage.setItem("sumoBrowserWordNetEnabled", "false");
  });
  await context.route("https://api.github.com/**", (route) =>
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({ tree: [] }),
    }),
  );
  const page = await context.newPage();
  page.setDefaultTimeout(60000);
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(origin + base);
  await page
    .waitForSelector("nav.tabs", { timeout: 60000 })
    .catch(async (error) => {
      console.error("Startup:", await page.locator("body").innerText(), errors);
      throw error;
    });
  let gateTimer;
  try {
    await Promise.race([
      backgroundStarted,
      new Promise((_, reject) => {
        gateTimer = setTimeout(
          () => reject(new Error("Background download never started")),
          15000,
        );
      }),
    ]);
  } finally {
    clearTimeout(gateTimer);
  }
  assert.equal(
    await page.evaluate(
      () =>
        document
          .querySelector("#app")
          .__vue_app__.config.globalProperties.$pinia._s.get("boot")
          .assetCacheState,
    ),
    "caching",
  );
  console.log("ok app opens while background caching is blocked");
  await page.waitForFunction(
    () =>
      !document
        .querySelector("#app")
        .__vue_app__.config.globalProperties.$pinia._s.get("kb").promoting,
  );
  assert.equal(
    await page.evaluate(
      () =>
        document
          .querySelector("#app")
          .__vue_app__.config.globalProperties.$pinia._s.get("boot")
          .assetCacheWarning,
    ),
    "",
  );
  await page.evaluate(async () => {
    await document
      .querySelector("#app")
      .__vue_app__.config.globalProperties.$router.push({ name: "prover" });
  });
  await page.waitForSelector(".monaco-editor", { timeout: 15000 });
  assert.equal(
    await page.evaluate(
      () =>
        document.querySelector("#app").__vue_app__.config.globalProperties
          .$router.currentRoute.value.name,
    ),
    "prover",
  );
  console.log("ok clicked tab and editor bypass the blocked background queue");
  releaseBackground();
  await page.waitForFunction(
    () =>
      document
        .querySelector("#app")
        .__vue_app__.config.globalProperties.$pinia._s.get("boot")
        .assetCacheState === "ready",
  );
  assert.equal(
    await page.evaluate(() => !!navigator.serviceWorker.controller),
    true,
  );
  const session = await context.newCDPSession(page);
  await session.send("Network.enable");
  await session.send("Network.setCacheDisabled", { cacheDisabled: true });
  server.closeAllConnections();
  await new Promise((resolve) => server.close(resolve));

  for (const name of [
    "visualize",
    "prover",
    "audit",
    "edit",
    "diagnostics",
    "kb",
    "problems",
    "browse",
  ]) {
    await page.evaluate(async (name) => {
      const router =
        document.querySelector("#app").__vue_app__.config.globalProperties
          .$router;
      await router.push({ name });
      if (router.currentRoute.value.name !== name)
        throw new Error("Navigation failed: " + name);
    }, name);
    console.log("ok disconnected tab:", name);
  }
  await page.evaluate(async () => {
    const app = document.querySelector("#app").__vue_app__;
    await app.config.globalProperties.$router.push({ name: "prover" });
  });
  await page.waitForSelector(".monaco-editor");
  console.log("ok editor loads after disconnect");

  for (const editor of await page
    .locator(".monaco-editor textarea.inputarea")
    .all()) {
    await editor.focus();
    await page.keyboard.press("Control+a");
    await page.keyboard.insertText("(likes Alice Bob)");
  }
  for (const backend of ["e", "vampire"]) {
    await page.evaluate((backend) => {
      document
        .querySelector("#app")
        .__vue_app__.config.globalProperties.$pinia._s.get("prover").backend =
        backend;
    }, backend);
    await page.getByRole("button", { name: "Prove", exact: true }).click();
    await page.waitForSelector(".status.Proved");
    await page
      .getByText(backend === "e" ? "via E" : "via Vampire", { exact: true })
      .waitFor();
    console.log(
      "ok original app prover worker after background caching:",
      backend,
    );
  }

  const assets = await readdir(join(dist, "assets"));
  const proofGraph = assets.find((name) => /^proof-graph-.*\.js$/.test(name));
  await page.evaluate(
    async (url) => {
      const module = await import(url);
      const loader = Object.values(module).find(
        (value) => typeof value === "function" && value.name,
      );
      // Importing the module verifies its cached dependencies; request graph libraries below too.
      if (!loader) throw new Error("Missing graph module exports");
    },
    origin + base + "assets/" + proofGraph,
  );
  for (const name of assets.filter((name) => /^cytoscape.*\.js$/.test(name)))
    await page.evaluate(
      (url) => import(url).then(() => true),
      origin + base + "assets/" + name,
    );
  console.log("ok graph libraries load after disconnect");

  // New dedicated workers must be able to import previously unused prover assets.
  const external = assets.find((name) =>
    /^external-prover.worker-.*\.js$/.test(name),
  );
  for (const [backend, program, args] of [
    ["e", "eprover", JSON.stringify(["--auto", "--cpu-limit=5"])],
    ["vampire", "vampire", "--mode vampire -t 5"],
  ]) {
    const result = await page.evaluate(
      async ({ url, baseUrl, backend, program, args }) => {
        const worker = new Worker(url, { type: "module" });
        const channel = new MessageChannel();
        worker.postMessage({ type: "port", port: channel.port1, baseUrl }, [
          channel.port1,
        ]);
        const ctrl = new Int32Array(new SharedArrayBuffer(16));
        channel.port2.postMessage({
          type: "run",
          program,
          args,
          ctrl: ctrl.buffer,
          tptp: "fof(a,axiom,p(a)).\nfof(q,conjecture,p(a)).",
        });
        const deadline = Date.now() + 30000;
        while (Atomics.load(ctrl, 0) !== 1) {
          if (Date.now() > deadline)
            throw new Error("Prover timed out: " + backend);
          await new Promise((resolve) => setTimeout(resolve, 20));
        }
        const bytes = new Uint8Array(
          new SharedArrayBuffer(Atomics.load(ctrl, 1)),
        );
        channel.port2.postMessage({ type: "buffer", data: bytes.buffer });
        while (Atomics.load(ctrl, 0) !== 2) {
          if (Date.now() > deadline) throw new Error("Prover result timed out");
          await new Promise((resolve) => setTimeout(resolve, 20));
        }
        worker.terminate();
        return JSON.parse(new TextDecoder().decode(bytes.slice()));
      },
      {
        url: origin + base + "assets/" + external,
        baseUrl: origin + base,
        backend,
        program,
        args,
      },
    );
    assert.equal(result.error, undefined, JSON.stringify(result));
    assert.match(result.stdout, /SZS status Theorem/);
    console.log("ok disconnected prover:", backend);
  }
  await page.evaluate(async () => {
    const boot = document
      .querySelector("#app")
      .__vue_app__.config.globalProperties.$pinia._s.get("boot");
    await boot.recoverWorker();
  });
  console.log("ok core worker restarts after disconnect");
  assert.deepEqual(errors, []);
} finally {
  releaseBackground();
  await browser.close();
  server.closeAllConnections();
  await new Promise((resolve) => server.close(resolve));
}
