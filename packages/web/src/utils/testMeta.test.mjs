import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";

const exports = {};
vm.runInNewContext(
  ts.transpileModule(
    readFileSync(new URL("./testMeta.ts", import.meta.url), "utf8"),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    },
  ).outputText,
  { exports },
);
const { emptyTestMeta, metaFromTest, metaSummary, testDirectives } = exports;

const parsed = (over = {}) => ({
  name: "t.kif.tq",
  note: "t.kif.tq",
  noteGiven: false,
  timeout: 30,
  timeGiven: false,
  categories: [],
  queryKif: "(p)",
  axiomKif: "",
  expectedProof: true,
  expectedAnswer: null,
  extraFiles: [],
  ...over,
});

test("a parsed test fills the form; a default note stays blank", () => {
  assert.equal(metaFromTest(parsed()).note, "");
  const m = metaFromTest(
    parsed({
      note: "Astronomy_4",
      noteGiven: true,
      categories: ["Astronomy", "Space"],
      extraFiles: ["Astronomy.kif"],
      expectedProof: false,
    }),
  );
  assert.deepEqual(
    { ...m },
    {
      note: "Astronomy_4",
      categories: "Astronomy, Space",
      answer: "no",
      bindings: "",
      files: "Astronomy.kif",
    },
  );
  assert.equal(
    metaFromTest(parsed({ expectedProof: null, expectedAnswer: ["Rex"] }))
      .answer,
    "bindings",
  );
  assert.equal(metaFromTest(parsed({ expectedProof: null })).answer, "none");
});

test("the summary names what is set", () => {
  assert.equal(metaSummary(emptyTestMeta()), "Test details — expects yes");
  assert.equal(
    metaSummary({ ...emptyTestMeta(), answer: "none" }),
    "Test details",
  );
  assert.equal(
    metaSummary({
      ...emptyTestMeta(),
      note: "SP01",
      categories: "spatial",
      answer: "bindings",
      bindings: "A B",
    }),
    "Test details — SP01 · spatial · expects A B",
  );
});

test("directives round-trip the form for formatTest", () => {
  const d = testDirectives({
    note: " SP01 ",
    categories: "a, b,",
    answer: "bindings",
    bindings: "Rex  Fido",
    files: "X.kif\nY.kif",
  });
  assert.equal(d.note, "SP01");
  assert.deepEqual([...d.categories], ["a", "b"]);
  assert.deepEqual([...d.extraFiles], ["X.kif", "Y.kif"]);
  assert.equal(d.expectedProof, true);
  assert.deepEqual([...d.expectedAnswer], ["Rex", "Fido"]);
  assert.equal(
    testDirectives({ ...emptyTestMeta(), answer: "no" }).expectedProof,
    false,
  );
});
