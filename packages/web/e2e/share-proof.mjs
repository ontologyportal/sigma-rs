import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { chromium } from "playwright";
import { createServer } from "vite";
import { fileURLToPath } from "node:url";

const server = await createServer({
  root: fileURLToPath(new URL("..", import.meta.url)),
  logLevel: "error",
  plugins: [
    {
      name: "proof-test-page",
      resolveId(id) {
        if (id === "/__proof-runtime.js") return id;
      },
      load(id) {
        if (id === "/__proof-runtime.js")
          return 'export { createApp, h } from "vue"; export { createPinia } from "pinia";';
      },
      configureServer(server) {
        server.middlewares.use("/__share-test", async (_req, res) => {
          res.setHeader("Content-Type", "text/html");
          res.end(
            await server.transformIndexHtml(
              "/__share-test",
              '<html><head><meta name="viewport" content="width=device-width, initial-scale=1"><link rel="stylesheet" href="/assets/styles.css"></head><body><div id="app"></div></body></html>',
            ),
          );
        });
      },
    },
  ],
});
await server.listen();
let browser;
let page;
try {
  browser = await chromium.launch({
    executablePath: process.env.PLAYWRIGHT_CHROMIUM || undefined,
  });
  page = await browser.newPage({ acceptDownloads: true });
  const head = "a".repeat(40);
  let branchHead = head;
  const downloads = [];
  await page.route(
    "https://api.github.com/repos/test/proofs/git/ref/heads/main",
    (route) => route.fulfill({ json: { object: { sha: branchHead } } }),
  );
  await page.route(
    "https://raw.githubusercontent.com/test/proofs/**",
    (route) => {
      downloads.push(route.request().url());
      return route.fulfill({
        contentType: "text/plain",
        body: route.request().url().endsWith("/contradiction.kif")
          ? "(instance Rex Dog)\n(not (instance Rex Dog))"
          : "(subclass Dog Mammal)",
      });
    },
  );
  await page.goto(server.resolvedUrls.local[0] + "__share-test");
  const report = await page.evaluate(async () => {
    const { handlers } = await import("/src/worker/handlers.ts");
    await handlers.boot();
    const files = [
      {
        name: "custom.kif",
        text: "(subclass Dog Mammal)",
        source: {
          repository: "github:test/proofs",
          branch: "main",
          path: "custom.kif",
        },
      },
    ];
    handlers.ingest({ name: files[0].name, text: files[0].text });
    handlers.promoteAll({ names: [files[0].name] });
    const reply = handlers.prove({
      assertions: "(instance Rex Dog)",
      query: "(instance Rex Mammal)",
      config: { timeLimitSecs: 5 },
      reportFiles: files,
    });
    if (reply.reportContext.error) throw Error(reply.reportContext.error);
    const stale = handlers.prove({
      query: "(instance Rex Mammal)",
      reportFiles: [{ name: "custom.kif", text: "(subclass Cat Mammal)" }],
    });
    if (!stale.reportContext.error) throw Error("Stale snapshot was accepted");
    handlers.newSession();
    const auditFiles = [
      {
        name: "contradiction.kif",
        text: "(instance Rex Dog)\n(not (instance Rex Dog))",
        source: {
          repository: "github:test/proofs",
          branch: "main",
          path: "contradiction.kif",
        },
      },
    ];
    handlers.ingest({ name: auditFiles[0].name, text: auditFiles[0].text });
    handlers.promoteAll({ names: [auditFiles[0].name] });
    const audit = handlers.audit({
      config: { timeLimitSecs: 5 },
      request: { count: 2, batch: 1, limit: 1, seed: 0 },
      reportFiles: auditFiles,
    });
    if (!audit.result.contradictions.length || audit.reportContext?.error)
      throw Error("Audit report snapshot failed");
    const { AUDIT_ENGINE } = await import("/src/constants/index.ts");
    const { createApp, h, createPinia } = await import("/__proof-runtime.js");
    const { default: ShareProof } =
      await import("/src/components/ShareProof.vue");
    const report = {
      format: "sigma-proof-report",
      version: 1,
      created: new Date().toISOString(),
      constituents: files,
      result: reply.result,
      config: reply.reportContext.config,
      problem: reply.reportContext.problem,
      defaults: reply.reportContext.defaults,
      engine: AUDIT_ENGINE,
      app: null,
      prover: { backend: "native", version: null, binaries: {} },
      unavailable: "",
    };
    const pinia = createPinia();
    const { default: ImportProofReport } =
      await import("/src/components/ImportProofReport.vue");
    const { router } = await import("/src/router/index.ts");
    const { default: AuditTab } = await import("/src/views/AuditTab.vue");
    createApp({
      render: () =>
        router.currentRoute.value.name === "audit"
          ? h(AuditTab)
          : h("div", [h(ShareProof, { report }), h(ImportProofReport)]),
    })
      .use(pinia)
      .use(router)
      .mount("#app");
    const { call } = await import("/src/services/sigma.ts");
    await call("boot");
    const { useKBStore } = await import("/src/stores/kb.ts");
    await useKBStore().ingest("keep.kif", "(instance Keep Entity)");
    const { exportProofReport } = await import("/src/services/proof-report.ts");
    globalThis.auditReport = await exportProofReport({
      ...report,
      constituents: auditFiles,
      config: audit.reportContext.config,
      problem: audit.reportContext.problem,
      result: audit.result.contradictions[0],
    });
    return report;
  });
  assert.equal(report.result.status, "Proved");
  await page.getByRole("button", { name: "Share result", exact: true }).click();
  const textarea = page.getByRole("textbox", { name: "Copyable proof report" });
  await textarea.waitFor();
  const copied = JSON.parse(await textarea.inputValue());
  assert.deepEqual(
    copied.result.proof,
    JSON.parse(JSON.stringify(report.result.proof)),
  );
  assert.deepEqual(copied.problem, report.problem);
  assert.equal(copied.constituents[0].text, undefined);
  assert.ok(copied.axioms.some((a) => a.kif === "(subclass Dog Mammal)"));
  const download = page.waitForEvent("download");
  await page.getByRole("button", { name: "Download report" }).click();
  assert.equal((await download).suggestedFilename(), "sigma-proof-report.json");
  await page.getByRole("button", { name: "Close", exact: true }).click();
  branchHead = "b".repeat(40);
  const beforeImport = downloads.length;
  const fileInput = {
    setInputFiles: async (file) => {
      const choosing = page.waitForEvent("filechooser");
      await page
        .getByRole("button", { name: "Import proof report", exact: true })
        .click();
      await (await choosing).setFiles(file);
    },
  };
  await fileInput.setInputFiles({
    name: "report.json",
    mimeType: "application/json",
    buffer: Buffer.from(JSON.stringify(copied)),
  });
  await page.getByRole("button", { name: "Import and replay" }).waitFor();
  assert.equal(
    await page.getByRole("button", { name: "Import and replay" }).isDisabled(),
    true,
  );
  assert.equal(downloads.length, beforeImport);
  const loadedNames = () =>
    page.evaluate(async () =>
      (await import("/src/stores/kb.ts"))
        .useKBStore()
        .constituents.map((c) => c.name),
    );
  assert.deepEqual(await loadedNames(), ["keep.kif"]);
  await page.getByRole("button", { name: "Close", exact: true }).last().click();
  assert.deepEqual(await loadedNames(), ["keep.kif"]);
  await fileInput.setInputFiles({
    name: "report.json",
    mimeType: "application/json",
    buffer: Buffer.from(JSON.stringify(copied)),
  });
  await page.getByRole("checkbox").check();
  await page.getByRole("button", { name: "Import and replay" }).click();
  await Promise.race([
    page.getByText("Result reproduced: Proved.", { exact: true }).waitFor(),
    page.getByRole("alert").waitFor(),
  ]);
  assert.equal(
    await page.getByRole("alert").count(),
    0,
    await page.locator("body").innerText(),
  );
  assert.deepEqual(await loadedNames(), ["custom.kif"]);
  assert.ok(
    downloads.slice(beforeImport).every((url) => url.includes(`/${head}/`)),
  );
  await page.getByRole("button", { name: "Close", exact: true }).last().click();
  const auditJson = await page.evaluate(() => globalThis.auditReport);
  await fileInput.setInputFiles({
    name: "audit.json",
    mimeType: "application/json",
    buffer: Buffer.from(auditJson),
  });
  const dialog = page.getByRole("dialog");
  await dialog.getByText("Consistency audit", { exact: true }).waitFor();
  assert.equal(await dialog.locator("details[open]").count(), 0);
  assert.equal(
    await dialog.getByText("Replay will open in the Audit tab.").count(),
    1,
  );
  const shots = new URL("./shots/", import.meta.url);
  await mkdir(shots, { recursive: true });
  await dialog.screenshot({
    path: fileURLToPath(new URL("import-report-desktop.png", shots)),
  });
  await page.setViewportSize({ width: 390, height: 844 });
  assert.equal(
    await dialog.evaluate((el) => el.scrollWidth <= el.clientWidth),
    true,
  );
  await dialog.screenshot({
    path: fileURLToPath(new URL("import-report-mobile.png", shots)),
  });
  await page.emulateMedia({ colorScheme: "dark" });
  await dialog.screenshot({
    path: fileURLToPath(new URL("import-report-dark.png", shots)),
  });
  await page.emulateMedia({ colorScheme: "light" });
  await page.setViewportSize({ width: 1280, height: 720 });
  await page.getByRole("checkbox").check();
  await page.getByRole("button", { name: "Import and replay" }).click();
  await page
    .getByRole("status")
    .filter({
      hasText: "Contradiction reproduced with the reported source axioms.",
    })
    .waitFor();
  assert.equal(new URL(page.url()).pathname, "/audit");
  assert.equal(await page.getByRole("dialog").count(), 0);
  await page.getByText("1 contradiction found", { exact: true }).waitFor();
  console.log(
    "Verified pushed-HEAD export, confirmation/cancellation, pinned import after branch advancement, and real WASM conjecture/contradiction replay.",
  );
} catch (error) {
  console.error(await page?.locator("body").innerText());
  console.error(
    await page?.evaluate(async () =>
      (await import("/src/stores/kb.ts"))
        .useKBStore()
        .constituents.map((c) => c.file),
    ),
  );
  throw error;
} finally {
  await browser?.close();
  await server.close();
}
