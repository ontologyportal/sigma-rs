import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
import { createServer } from "vite";

// Focused graph rendering check, without loading a KB or running a prover.
// Run: npm run test:e2e:proof-graph --workspace @sigma/web
const here = path.dirname(fileURLToPath(import.meta.url));
const server = await createServer({
  root: path.join(here, ".."),
  logLevel: "error",
});
let browser;
try {
  await server.listen();
  browser = await chromium.launch({
    executablePath: process.env.PLAYWRIGHT_CHROMIUM || undefined,
  });
  const page = await browser.newPage();
  const problems = [];
  page.on("pageerror", (error) => problems.push(error.message));
  await page.route("**/__proof_graph_test", (route) =>
    route.fulfill({
      contentType: "text/html",
      body: `<!doctype html><html><head>
        <link rel="stylesheet" href="/assets/styles.css">
        </head><body><div id="graph" style="width:1000px;height:600px"></div>
        </body></html>`,
    }),
  );
  await page.goto(
    new URL("__proof_graph_test", server.resolvedUrls.local[0]).href,
  );
  for (const dark of [false, true]) {
    const result = await page.evaluate(async (dark) => {
      const { renderProofGraph } = await import("/src/services/proof-graph.ts");
      document.documentElement.dataset.theme = dark ? "dark" : "light";
      const container = document.getElementById("graph");
      const steps = [
        {
          index: 0,
          rule: "axiom",
          premises: [],
          kif: "(instance Socrates Human)",
        },
        {
          index: 7,
          rule: "conjecture",
          premises: [],
          kif: "(instance Socrates Mortal)",
        },
        {
          index: 12,
          rule: "resolution",
          premises: [0, 7],
          kif: "(and (instance Socrates Human) (instance Socrates Mortal))",
        },
      ].map((step) => ({ ...step, tptp: null, file: null, line: null }));
      const cy = await renderProofGraph(container, steps, dark);
      await new Promise((resolve, reject) => {
        const timeout = setTimeout(
          () => reject(new Error("graph labels did not render")),
          5000,
        );
        function check() {
          if (
            container.querySelectorAll(".pg-node-label").length === steps.length
          ) {
            clearTimeout(timeout);
            resolve();
          } else requestAnimationFrame(check);
        }
        check();
      });
      const nodes = cy.nodes().map((node) => ({
        index: node.data("index"),
        width: node.width(),
        height: node.height(),
      }));
      const labels = [...container.querySelectorAll(".pg-node-label")].map(
        (el) => {
          const { width, height } = el.getBoundingClientRect();
          return {
            number: el.querySelector(".pg-idx")?.textContent,
            formula: el.textContent.replace(/^\d+\.\s*/, "").trim(),
            width: width / cy.zoom(),
            height: height / cy.zoom(),
          };
        },
      );
      const edges = cy
        .edges()
        .map((edge) => [edge.source().id(), edge.target().id()]);
      cy.destroy();
      const empty = await renderProofGraph(container, [], dark);
      const emptyCount = empty.elements().length;
      empty.destroy();
      return { nodes, labels, edges, emptyCount };
    }, dark);
    assert.deepEqual(
      result.labels.map((label) => label.number),
      ["1.", "8.", "13."],
    );
    assert.deepEqual(
      result.nodes.map((node) => node.index),
      [0, 7, 12],
    );
    assert.match(result.labels[0].formula, /^\(instance Socrates Human\)$/);
    assert.deepEqual(result.edges, [
      ["n0", "n12"],
      ["n7", "n12"],
    ]);
    for (const [i, label] of result.labels.entries()) {
      assert.ok(
        result.nodes[i].width >= label.width - 1,
        "numbered label fits node width",
      );
      assert.ok(
        result.nodes[i].height >= label.height - 1,
        "numbered label fits node height",
      );
    }
    assert.equal(result.emptyCount, 0);
  }
  assert.deepEqual(problems, []);
  console.log("proof-graph e2e: all checks passed");
} finally {
  await browser?.close();
  await server.close();
}
