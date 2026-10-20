/** Find tooltips must remain readable and leave their buttons clickable.
 * Run: npm run test:e2e:editor-hover --workspace @sigma/web
 * BASE_URL may point to an existing Vite development server.
 */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
import { parse, compileStyle } from "@vue/compiler-sfc";

const moduleRoot =
  process.env.EDITOR_TEST_MODULE_ROOT ||
  "/@fs/" +
    fileURLToPath(
      new URL("../../../node_modules/", import.meta.url),
    ).replaceAll("\\", "/");
const styles = ["EditTab", "AskTellTab"]
  .map((name) => {
    const { descriptor } = parse(
      readFileSync(
        new URL(`../src/views/${name}.vue`, import.meta.url),
        "utf8",
      ),
    );
    return compileStyle({
      source: descriptor.styles[0].content,
      id: "hover-test",
      scoped: false,
    }).code;
  })
  .join("\n");
let server;
let browser;
try {
  let base = process.env.BASE_URL;
  if (!base) {
    const { createServer } = await import("vite");
    server = await createServer({
      root: fileURLToPath(new URL("../", import.meta.url)),
      logLevel: "error",
    });
    await server.listen();
    base = server.resolvedUrls.local[0];
  }
  browser = await chromium.launch({
    executablePath: process.env.PLAYWRIGHT_CHROMIUM || undefined,
  });
  const page = await browser.newPage();
  const problems = [];
  page.on("pageerror", (error) => problems.push(error.message));
  // Exercise the real wrapper and Monaco without booting a KB or its worker.
  await page.route("**/src/services/monaco.ts", (route) =>
    route.fulfill({
      contentType: "text/javascript",
      body: `
      import '${moduleRoot}monaco-editor/esm/vs/editor/editor.all.js';
      import * as m from '${moduleRoot}monaco-editor/esm/vs/editor/editor.api.js';
      import EditorWorker from '${moduleRoot}monaco-editor/esm/vs/editor/editor.worker.js?worker';
      self.MonacoEnvironment = {getWorker: () => new EditorWorker()};
      export async function loadMonaco() { return m; }
      export const themeName = dark => dark ? 'vs-dark' : 'vs';
      export const lspEditor = () => null;
      export function setLspEditor() {}
      export const diagsToMarkers = () => [];
    `,
    }),
  );
  await page.route("**/src/stores/shell.ts", (route) =>
    route.fulfill({
      contentType: "text/javascript",
      body: "export const useShellStore = () => ({isDark: window.testDark});",
    }),
  );
  await page.route("**/__editor_hover_test", (route) =>
    route.fulfill({
      contentType: "text/html",
      body: `<!doctype html><html><head>
      <link rel="stylesheet" href="/assets/styles.css">
      <style>${styles}
        body { padding: 120px 80px; }
        .test-frame { width: min(650px, calc(100vw - 160px)); position: relative; }
        .edit-pane.test-frame { height: 320px; }
        #app { height: 100%; }
      </style></head><body>
      <div class="edit-tab"><div id="frame" class="card edit-pane test-frame"><div id="app"></div></div></div>
      </body></html>`,
    }),
  );

  async function checkTooltip(control, text) {
    await control.hover();
    const tooltip = page
      .locator(".monaco-hover.workbench-hover")
      .filter({ hasText: text });
    await tooltip.waitFor({ state: "visible" });
    const button = await control.boundingBox();
    const tip = await tooltip.boundingBox();
    assert.ok(
      tip.y + tip.height <= button.y ||
        tip.y >= button.y + button.height ||
        tip.x + tip.width <= button.x ||
        tip.x >= button.x + button.width,
      `${text}: tooltip overlaps its control`,
    );
    assert.equal(
      await control.evaluate((el) => {
        const r = el.getBoundingClientRect();
        return el.contains(
          document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2),
        );
      }),
      true,
      `${text}: tooltip intercepts pointer events`,
    );
    assert.equal(
      await tooltip.evaluate((el) => {
        const r = el.getBoundingClientRect();
        return [
          [r.left + 2, r.top + 2],
          [r.right - 2, r.bottom - 2],
        ].every(([x, y]) => el.contains(document.elementFromPoint(x, y)));
      }),
      true,
      `${text}: tooltip is clipped or covered`,
    );
  }

  for (const scenario of [
    { name: "editor-light", width: 1100 },
    { name: "editor-dark-narrow", width: 780, dark: true },
    { name: "editor-fullscreen", width: 1100, fullscreen: true },
    { name: "ask-tell", width: 1100, compact: true },
  ]) {
    await page.setViewportSize({ width: scenario.width, height: 850 });
    await page.goto(new URL("__editor_hover_test", base).href);
    const source = await (
      await page.request.get(
        new URL("src/components/MonacoEditor.vue", base).href,
      )
    ).text();
    // Share the exact Vue module URL used by the compiled SFC.
    const vuePath = source.match(/from "([^"]*\/deps\/vue.js[^"]*)"/)[1];
    await page.evaluate(
      async ({ vuePath, scenario }) => {
        window.testDark = !!scenario.dark;
        if (scenario.fullscreen) document.body.classList.add("edit-fullscreen");
        if (scenario.compact)
          document.getElementById("frame").className = "pane-editor test-frame";
        const { createApp, h } = await import(vuePath);
        const { default: Editor } =
          await import("/src/components/MonacoEditor.vue");
        createApp({
          render: () =>
            h(Editor, {
              modelValue: "(instance Socrates Human)\n(instance Plato Human)",
              compact: !!scenario.compact,
              onReady: (editor, monaco) => {
                window.editor = editor;
                window.monaco = monaco;
              },
            }),
        }).mount("#app");
      },
      { vuePath, scenario },
    );
    await page.waitForFunction(() => window.editor);
    await page.evaluate(() => {
      window.editor.focus();
      window.editor.setSelection(new window.monaco.Selection(1, 1, 2, 23));
    });
    await page.keyboard.press(
      process.platform === "darwin" ? "Meta+f" : "Control+f",
    );
    const selection = page.locator(
      '.find-widget [aria-label^="Find in Selection"]',
    );
    await checkTooltip(selection, "Find in Selection");
    await selection.click();
    assert.equal(await selection.getAttribute("aria-checked"), "true");
    await page.keyboard.press("Alt+l");
    assert.equal(await selection.getAttribute("aria-checked"), "false");
    for (const label of [
      "Match Case",
      "Match Whole Word",
      "Use Regular Expression",
    ]) {
      await checkTooltip(
        page.locator(`.find-widget [aria-label^="${label}"]`),
        label,
      );
    }
    const close = page.locator(".find-widget .codicon-widget-close");
    await checkTooltip(close, "Escape");
    await close.click();
    await page.waitForFunction(
      () =>
        document.querySelector(".find-widget")?.getAttribute("aria-hidden") ===
        "true",
    );
    console.log(
      `ok ${scenario.name}: tooltips visible, selection toggles, close clickable`,
    );
  }
  assert.deepEqual(problems, []);
} finally {
  await browser?.close();
  await server?.close();
}
