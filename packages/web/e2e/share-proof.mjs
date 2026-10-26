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
  let postedIssue = null;
  await page.route(
    "https://api.github.com/repos/ontologyportal/sumo/issues",
    (route) => {
      postedIssue = JSON.parse(route.request().postData());
      return route.fulfill({
        json: {
          number: 42,
          html_url: "https://github.com/ontologyportal/sumo/issues/42",
          title: postedIssue.title,
          state: "open",
        },
      });
    },
  );
  await page.route(
    /https:\/\/api\.github\.com\/repos\/ontologyportal\/sumo\/issues\/(42|43)$/,
    async (route) => {
      const number = Number(route.request().url().split("/").at(-1));
      const body =
        number === 42
          ? postedIssue?.body
          : await page.evaluate(async () => {
              const { proofIssueBody } =
                await import("/src/utils/proofReport.ts");
              return proofIssueBody(
                globalThis.auditReport,
                "Audit contradiction",
              );
            });
      return route.fulfill({ json: { body } });
    },
  );
  await page.route(
    "https://api.github.com/repos/ontologyportal/sumo/git/ref/heads/master",
    (route) => route.fulfill({ json: { object: { sha: head } } }),
  );
  await page.route(
    /https:\/\/api\.github\.com\/repos\/ontologyportal\/sumo\/actions\/workflows\/contradiction-audit\.yml\/runs\?/,
    (route) => route.fulfill({ json: { workflow_runs: [] } }),
  );
  await page.route(
    "https://raw.githubusercontent.com/ontologyportal/sumo/audit-state/**",
    (route) => route.fulfill({ status: 404, body: "" }),
  );
  await page.route(/https:\/\/api\.github\.com\/search\/issues\?/, (route) =>
    route.fulfill({
      json: {
        total_count: 2,
        incomplete_results: false,
        items: [
          {
            number: 43,
            title: "[Contradiction] Synthetic audit",
            html_url: "https://github.com/ontologyportal/sumo/issues/43",
            state: "open",
          },
          {
            number: 44,
            title: "A different contradiction report",
            html_url: "https://github.com/ontologyportal/sumo/issues/44",
            state: "closed",
          },
        ],
      },
    }),
  );
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
    const { default: AskTellTab } = await import("/src/views/AskTellTab.vue");
    const { default: KbTab } = await import("/src/views/KbTab.vue");
    createApp({
      render: () =>
        router.currentRoute.value.name === "audit"
          ? h(AuditTab)
          : router.currentRoute.value.name === "prover"
            ? h(AskTellTab)
            : router.currentRoute.value.name === "kb"
              ? h(KbTab)
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
  await page.evaluate(async () => {
    const auth = (await import("/src/stores/auth.ts")).useAuthStore();
    auth.token = "test-token";
    auth.user = { name: "Tester", login: "tester", avatarUrl: "" };
  });
  await page.getByRole("button", { name: "Report proof to GitHub" }).click();
  await page
    .getByLabel("Title", { exact: true })
    .fill("A useful conjecture proof");
  await page
    .getByLabel("Description", { exact: true })
    .fill("Please review this proof.");
  await page.getByRole("button", { name: "Create SUMO issue" }).click();
  await page.getByRole("link", { name: "issue #42" }).waitFor();
  assert.equal(postedIssue.title, "[Proof] A useful conjecture proof");
  assert.match(postedIssue.body, /Please review this proof/);
  assert.match(postedIssue.body, /sigma-proof-report:v3:start/);
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
  // An audit report cites its contradiction's axioms; recording it needs
  // them in the KB, so load the audited file alongside the imported one.
  await page.evaluate(async () => {
    const kb = (await import("/src/stores/kb.ts")).useKBStore();
    await kb.ingest(
      "contradiction.kif",
      "(instance Rex Dog)\n(not (instance Rex Dog))",
    );
    await kb.reprocess();
  });
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
  // The report's contradiction is recorded against this KB and replayed:
  // its cited axioms audited together.
  await page.getByText("1 contradiction found", { exact: true }).waitFor();
  assert.equal(new URL(page.url()).pathname, "/audit");
  assert.equal(await page.getByRole("dialog").count(), 0);
  await page.getByText(/Replayed a recorded contradiction/).waitFor();
  const recordedCount = () =>
    page.evaluate(async () =>
      (await import("/src/stores/contradictions.ts"))
        .useContradictionsStore()
        .findings.map((f) => f.origin),
    );
  assert.deepEqual(await recordedCount(), ["report"]);

  // Contradiction sources on the KB tab: the shipped GitHub tracker reads
  // the open `[Contradiction]` issue (the mocked #43) on a manual check.
  await page.evaluate(async () => {
    const { useContradictionsStore } =
      await import("/src/stores/contradictions.ts");
    await useContradictionsStore().clearFindings();
    const { navigate } = await import("/src/router/index.ts");
    await navigate("kb", { view: "contradictions" });
  });
  await page.getByText("SUMO nightly audit", { exact: true }).waitFor();
  const tracker = page
    .locator("details.src-report")
    .filter({ hasText: "ontologyportal/sumo issues" });
  await tracker.getByRole("button", { name: "Check now" }).click();
  await page.getByText(/1 new contradiction recorded/).waitFor();
  assert.deepEqual(await recordedCount(), ["github:ontologyportal/sumo#43"]);
  await tracker.locator("summary").click();
  await tracker.getByText("contradiction.kif:2").first().waitFor();
  await page
    .locator(".card")
    .filter({ hasText: "Contradiction sources" })
    .screenshot({
      path: fileURLToPath(new URL("contradiction-sources.png", shots)),
    });
  // A one-shot upload of the same report records nothing new, and is
  // listed for provenance without a watch preference.
  await page.getByRole("radio", { name: "Upload" }).click();
  const uploadChooser = page.waitForEvent("filechooser");
  await page.getByRole("button", { name: /Choose report file/ }).click();
  await (
    await uploadChooser
  ).setFiles({
    name: "audit.json",
    mimeType: "application/json",
    buffer: Buffer.from(auditJson),
  });
  await page.getByText(/audit\.json: 0 new contradictions recorded/).waitFor();
  const upload = page
    .locator("details.src-report")
    .filter({ hasText: "audit.json" });
  assert.equal(
    await upload.getByRole("button", { name: "Check now" }).count(),
    0,
  );

  // Back on the Audit tab, the recorded contradiction replays on demand.
  await page.evaluate(async () => {
    const { navigate } = await import("/src/router/index.ts");
    await navigate("audit");
  });
  await page.getByText("Recorded contradictions", { exact: true }).waitFor();
  await page.getByRole("button", { name: "Show proof" }).click();
  await page.getByRole("button", { name: "Hide proof" }).waitFor();
  await page.getByRole("button", { name: "Replay", exact: true }).click();
  await page.getByText("1 contradiction found", { exact: true }).waitFor();
  await page.getByText(/Replayed a recorded contradiction/).waitFor();
  await page.evaluate(async () => {
    await (await import("/src/router/index.ts")).navigate("prover");
  });
  await page.locator(".monaco-editor").first().waitFor();
  await page.getByRole("radio", { name: "TPTP", exact: true }).click();
  await page.evaluate(async () => {
    const monaco = await (await import("/src/services/monaco.ts")).loadMonaco();
    for (const model of monaco.editor.getModels())
      model.setValue("fof(a, axiom, p).\nfof(q, conjecture, p).");
  });
  await page.getByRole("button", { name: "Prove", exact: true }).click();
  const proof = page
    .locator("details")
    .filter({
      has: page.locator("summary", { hasText: /^proof \(\d+ steps?\)/ }),
    })
    .first();
  await proof.waitFor();
  assert.equal(await proof.getAttribute("open"), "");
  await proof.locator("summary").click();
  assert.equal(await proof.getAttribute("open"), null);
  const share = page.getByRole("button", { name: "Share result", exact: true });
  assert.equal(await share.isVisible(), true);
  await proof.locator("summary").click();
  assert.equal(await proof.getAttribute("open"), "");
  await proof.locator("summary").click();
  await page.getByRole("button", { name: "Prove", exact: true }).click();
  await page.getByRole("button", { name: "Prove", exact: true }).waitFor();
  await page.waitForFunction(
    () => document.querySelector(".proof-actions") != null,
  );
  assert.equal(await proof.getAttribute("open"), "");
  console.log(
    "Verified GitHub issue publishing, pinned import after branch advancement, recorded-contradiction import from a report, a GitHub tracker and an upload, and real WASM conjecture/contradiction replay.",
  );
} catch (error) {
  console.error(error);
  try {
    console.error(await page?.locator("body").innerText());
    console.error(
      await page?.evaluate(async () =>
        (await import("/src/stores/kb.ts"))
          .useKBStore()
          .constituents.map((c) => c.file),
      ),
    );
  } catch {
    /* the page never mounted: the error above is the one that matters */
  }
  throw error;
} finally {
  await browser?.close();
  await server.close();
}
