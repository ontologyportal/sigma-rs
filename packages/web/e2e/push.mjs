// Exercise the real contribution panel and edit store against mocked GitHub.
// Run: node packages/web/e2e/push.mjs
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
import { createServer } from "vite";

const original = "(instance Original Entity)\n";
const edited = "(instance Edited Entity)\n";
const hash = (text) =>
  createHash("sha1")
    .update(`blob ${Buffer.byteLength(text)}\0${text}`)
    .digest("hex");
const requests = [];
const root = fileURLToPath(new URL("../", import.meta.url));
const server = await createServer({
  root,
  logLevel: "error",
  plugins: [
    {
      name: "push-panel-test",
      resolveId(id) {
        if (id === "virtual:push-test") return id;
      },
      load(id) {
        if (id === "virtual:push-test")
          return `
        import { createApp, h } from 'vue';
        import { createPinia, setActivePinia } from 'pinia';
        import Panel from '/src/components/edit/ContributePanel.vue';
        import { GitOrigin } from '/src/models/Origin.ts';
        import { Constituent } from '/src/models/Constituent.ts';
        import { useKBStore } from '/src/stores/kb.ts';
        import { useBootStore } from '/src/stores/boot.ts';
        import { useAuthStore } from '/src/stores/auth.ts';
        import { useChangesStore } from '/src/stores/changes.ts';
        import '/assets/styles.css';
        const pinia = createPinia(); setActivePinia(pinia);
        useBootStore().opfsRoot = await navigator.storage.getDirectory();
        const origin = new GitOrigin('github', 'alice', 'sumo', 'work/ontology');
        const name = origin.nameFor('Merge.kif');
        useKBStore().constituents = [new Constituent(name, origin, ${JSON.stringify(edited)})];
        await useChangesStore().recordSave(name, 'sumo', ${JSON.stringify(edited)}, ${JSON.stringify(original)});
        useAuthStore().token = 'test-token';
        createApp({ render: () => h(Panel, { open: true, current: { name, origin }, dirty: false }) }).use(pinia).mount('#app');
`;
      },

      configureServer(server) {
        server.middlewares.use((req, res, next) => {
          if (req.url !== "/__push-test") return next();
          res.setHeader("Content-Type", "text/html");
          res.end(
            '<html><head><meta name="viewport" content="width=device-width, initial-scale=1"></head><body style="max-width:900px;margin:32px auto"><div id="app"></div><script type="module" src="/@id/virtual:push-test"></script></body></html>',
          );
        });
      },
    },
  ],
});
await server.listen();
let browser;
try {
  browser = await chromium.launch({
    executablePath: process.env.PLAYWRIGHT_CHROMIUM || undefined,
  });
  const page = await browser.newPage({
    viewport: { width: 1200, height: 900 },
  });
  const errors = [];
  page.on("pageerror", (e) => {
    errors.push(e.message);
    console.error(e.message);
  });
  page.on("console", (m) => {
    if (m.type() === "error") console.error(m.text());
  });
  await page.route("https://api.github.com/**", (route) => {
    const req = route.request();
    const path = new URL(req.url()).pathname;
    const method = req.method();
    const body = req.postDataJSON();
    requests.push({ path, method, body });
    let data;
    if (/^\/repos\/[^/]+\/sumo$/.test(path))
      data = { permissions: { push: true }, default_branch: "master" };
    else if (path.includes("/git/ref/heads/"))
      data = { object: { sha: "parent" } };
    else if (method === "GET" && path.includes("/git/trees/"))
      data = {
        tree: [
          {
            path: "Merge.kif",
            sha: hash(original),
            mode: "100644",
            type: "blob",
          },
        ],
      };
    else if (path.endsWith("/git/blobs")) data = { sha: hash(edited) };
    else if (path.endsWith("/git/commits/parent"))
      data = { tree: { sha: "base-tree" } };
    else if (path.endsWith("/git/trees")) data = { sha: "new-tree" };
    else if (path.endsWith("/git/commits")) data = { sha: "commit" };
    else if (method === "PATCH") data = {};
    else if (path.endsWith("/pulls"))
      data =
        method === "GET"
          ? []
          : { number: 7, html_url: "https://github.com/pr/7" };
    else assert.fail(`Unexpected GitHub request: ${method} ${path}`);
    return route.fulfill({ json: data });
  });
  await page.goto(new URL("__push-test", server.resolvedUrls.local[0]).href);
  await page
    .getByRole("button", { name: "Push to work/ontology", exact: true })
    .click();
  await page.getByRole("link", { name: "View commit", exact: true }).waitFor();
  assert.ok(!requests.some((r) => r.path.endsWith("/pulls")));
  assert.deepEqual(requests.find((r) => r.method === "PATCH").body, {
    sha: "commit",
    force: false,
  });
  assert.equal(
    await page.evaluate(
      () =>
        Object.keys(JSON.parse(localStorage.getItem("sumoBrowserEdits")))
          .length,
    ),
    0,
  );
  const shots = fileURLToPath(new URL("./shots/", import.meta.url));
  mkdirSync(shots, { recursive: true });
  await page.screenshot({ path: `${shots}/push.png`, fullPage: true });
  await page
    .getByRole("button", {
      name: "Create pull request from this branch",
      exact: true,
    })
    .click();
  await page.getByRole("link", { name: /pull request #7/ }).waitFor();
  assert.equal(
    requests.filter((r) => r.method === "POST" && r.path.endsWith("/pulls"))
      .length,
    1,
  );
  assert.deepEqual(errors, []);
  console.log(
    "PASS: saved fork edits push to the selected branch, clear local tracking, and can become a PR",
  );
} finally {
  await browser?.close();
  await server.close();
}
