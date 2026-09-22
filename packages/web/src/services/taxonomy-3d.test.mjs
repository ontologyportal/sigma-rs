import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";

const exports = {};
vm.runInNewContext(
  ts.transpileModule(
    readFileSync(new URL("./taxonomy-3d.ts", import.meta.url), "utf8"),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    },
  ).outputText,
  { exports },
);
const { layoutTaxonomy, loadTaxonomy } = exports;
const relations = ["subclass", "instance", "subrelation", "subAttribute"];
function fixture(assertions) {
  const pages = new Map();
  for (const [child, parent, relation = "subclass"] of assertions) {
    for (const name of [child, parent]) {
      if (!pages.has(name)) pages.set(name, { parents: [], children: [] });
    }
    pages.get(child).parents.push({ relation, parent });
    pages.get(parent).children.push({ relation, parent: child });
  }
  return pages;
}
test("empty and isolated root are safe", () => {
  assert.equal(layoutTaxonomy(new Map(), relations).nodes.length, 0);
  const { nodes, edges } = layoutTaxonomy(
    new Map([["Entity", { parents: [], children: [] }]]),
    relations,
  );
  assert.equal(nodes.length, 1);
  assert.equal(Math.hypot(...nodes[0].position), 0);
  assert.equal(edges.length, 0);
});
test("cycles and multiple inheritance keep assertions without duplicate nodes", () => {
  const pages = fixture([
    ["A", "Entity"],
    ["B", "Entity"],
    ["C", "A"],
    ["C", "B"],
    ["A", "C"],
    ["A", "Entity"],
    ["item", "C", "instance"],
  ]);
  const { nodes, edges } = layoutTaxonomy(pages, relations);
  assert.equal(nodes.length, 5);
  assert.equal(edges.length, 6);
  assert.equal(new Set(nodes.map((n) => n.name)).size, nodes.length);
  for (const node of nodes) assert.ok(node.position.every(Number.isFinite));
  assert.equal(layoutTaxonomy(pages, ["subclass"]).nodes.length, 4);
});
test("larger branches receive proportionally larger angular shares", () => {
  const pages = fixture([
    ["A", "Entity"],
    ["B", "Entity"],
    ["A1", "A"],
    ["A2", "A"],
    ["A3", "A"],
    ["A4", "A1"],
  ]);
  const { nodes } = layoutTaxonomy(pages, relations);
  const a = nodes.find((n) => n.name === "A"),
    b = nodes.find((n) => n.name === "B");
  assert.equal(a.weight, 5);
  assert.ok(Math.abs(a.share / b.share - 5) < 1e-9);
  assert.ok(Math.abs(a.share + b.share - 1) < 1e-9);
  const a1 = nodes.find((n) => n.name === "A1");
  const movement = a1.position.map((value, i) => value - a.position[i]);
  assert.ok(
    movement.reduce((sum, value, i) => sum + value * a.position[i], 0) > 0,
  );
  assert.ok(nodes.some((n) => Math.abs(n.position[2]) > 1));
});
test("every relation is retained and can be filtered", () => {
  const pages = fixture(relations.map((r, i) => ["n" + i, "Entity", r]));
  assert.equal(layoutTaxonomy(pages, relations).edges.length, 4);
  assert.equal(
    layoutTaxonomy(pages, ["subrelation"]).edges[0].relation,
    "subrelation",
  );
  assert.equal(layoutTaxonomy(pages, []).nodes.length, 1);
});
test("loading obeys depth and budget and visits cyclic nodes once", async () => {
  const pages = fixture([
    ["A", "Entity"],
    ["B", "A"],
    ["C", "B"],
    ["Entity", "C"],
  ]);
  const calls = [];
  const fetch = async (name) => {
    calls.push(name);
    return pages.get(name);
  };
  const shallow = await loadTaxonomy(
    fetch,
    1,
    () => false,
    () => {},
  );
  assert.equal(shallow.pages.size, 2);
  assert.equal(shallow.truncated, true);
  const full = await loadTaxonomy(
    fetch,
    Infinity,
    () => false,
    () => {},
  );
  assert.equal(full.pages.size, 4);
  assert.equal(full.truncated, false);
  const bounded = await loadTaxonomy(
    fetch,
    Infinity,
    () => false,
    () => {},
    3,
  );
  assert.equal(bounded.pages.size, 3);
  assert.equal(bounded.truncated, true);
  assert.equal(calls.length, 9);
});
test("cancellation discards stale responses and failures propagate", async () => {
  let cancelled = false;
  const result = await loadTaxonomy(
    async () => {
      cancelled = true;
      return { parents: [], children: [] };
    },
    4,
    () => cancelled,
    () => assert.fail("Stale progress"),
  );
  assert.equal(result, null);
  await assert.rejects(
    loadTaxonomy(
      async () => {
        throw new Error("Worker unavailable");
      },
      4,
      () => false,
      () => {},
    ),
    /Worker unavailable/,
  );
});
