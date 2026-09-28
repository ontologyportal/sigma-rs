import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";

const exports = {};
vm.runInNewContext(
  ts.transpileModule(
    readFileSync(new URL("./contradictionReport.ts", import.meta.url), "utf8"),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    },
  ).outputText,
  { exports, crypto, TextEncoder },
);
const {
  reportability,
  contradictionFingerprint,
  fingerprintMarker,
  buildContradictionIssue,
  formLineSpan,
  axiomDiagnostics,
} = exports;

const axiom = (index, kif, file, line) => ({
  index,
  rule: "axiom",
  premises: [],
  kif,
  tptp: null,
  file,
  line,
});
const derived = (index, rule, premises, kif) => ({
  index,
  rule,
  premises,
  kif,
  tptp: null,
  file: null,
  line: null,
});

const STEPS = [
  axiom(0, "(disjoint Plant Animal)", "Merge.kif", 120),
  axiom(1, "(instance Fido Plant)", "Mid-level-ontology.kif", 7),
  axiom(2, "(instance Fido Animal)", "Merge.kif", 300),
  derived(3, "resolution", [0, 1], "(not (instance Fido Animal))"),
  derived(4, "resolution", [2, 3], "FALSE"),
];
const upstream = new Set(["Merge.kif", "Mid-level-ontology.kif"]);

test("reportable when every cited axiom is upstream", () => {
  const r = reportability(STEPS, (f) => upstream.has(f));
  assert.equal(r.ok, true);
  // Spread: arrays from the vm context have a foreign prototype.
  assert.deepEqual([...r.files], ["Merge.kif", "Mid-level-ontology.kif"]);
  assert.deepEqual([...r.blockers], []);
});

test("a local file blocks the report and is named", () => {
  const steps = [...STEPS, axiom(5, "(foo)", "uploads/Mine.kif", 1)];
  const r = reportability(steps, (f) => upstream.has(f));
  assert.equal(r.ok, false);
  assert.equal(r.blockers.length, 1);
  assert.match(r.blockers[0], /uploads\/Mine\.kif/);
});

test("an untraced axiom blocks the report", () => {
  const steps = [...STEPS, axiom(5, "(instance Rex Dog)", null, null)];
  const r = reportability(steps, (f) => upstream.has(f));
  assert.equal(r.ok, false);
  assert.match(r.blockers[0], /1 axiom could not be traced/);
});

test("an untraced hypothesis step blocks the report", () => {
  const steps = [
    ...STEPS,
    { ...axiom(5, "(p)", null, null), rule: "hypothesis" },
  ];
  assert.equal(reportability(steps, (f) => upstream.has(f)).ok, false);
});

test("a proof citing no axioms is not reportable", () => {
  const r = reportability([derived(0, "taxonomy", [], "FALSE")], () => true);
  assert.equal(r.ok, false);
});

test("fingerprint ignores step order and line numbers", async () => {
  const a = await contradictionFingerprint(STEPS);
  const shuffled = [STEPS[2], STEPS[0], STEPS[1], STEPS[4], STEPS[3]].map(
    (s) => (s.file ? { ...s, line: s.line + 50 } : s),
  );
  assert.equal(await contradictionFingerprint(shuffled), a);
  assert.match(a, /^[0-9a-f]{16}$/);
  const other = await contradictionFingerprint([
    axiom(0, "(disjoint Plant Fungus)", "Merge.kif", 1),
  ]);
  assert.notEqual(other, a);
});

test("issue carries axiom links, the full proof, and the marker", () => {
  const { title, body } = buildContradictionIssue({
    steps: STEPS,
    prose: "Fido is a plant and an animal.",
    fingerprint: "abc123",
    blobBase: "https://github.com/ontologyportal/sumo/blob/deadbeef",
    commit: "deadbeef",
    context: ["Found by: SUPr"],
  });
  assert.equal(
    title,
    "Contradictory axioms in Merge.kif, Mid-level-ontology.kif",
  );
  assert.ok(
    body.includes(
      "[Merge.kif:120](https://github.com/ontologyportal/sumo/blob/deadbeef/Merge.kif#L120)",
    ),
  );
  assert.ok(body.includes("Loaded at commit deadbeef."));
  assert.ok(body.includes("### Full proof (5 steps)"));
  assert.ok(body.includes("4. resolution (from steps 1, 2)"));
  assert.ok(body.includes("5. resolution (from steps 3, 4)\n   FALSE"));
  assert.ok(body.includes("Fido is a plant and an animal."));
  assert.ok(body.includes("- Found by: SUPr"));
  assert.ok(body.includes(fingerprintMarker("abc123")));
  assert.ok(!body.includes("\n\n\n"));
});

test("issue lists diagnostics on the axioms only when there are some", () => {
  const base = {
    steps: STEPS,
    fingerprint: "f",
    blobBase: "https://github.com/ontologyportal/sumo/blob/abc",
    context: [],
  };
  const d = {
    file: "Merge.kif",
    line: 120,
    col: 1,
    end_line: 120,
    end_col: 1,
    severity: "error",
    kind: "semantic",
    code: "domain-mismatch",
    message: "argument 2 is not a Class",
  };
  const { body } = buildContradictionIssue({ ...base, diagnostics: [d] });
  assert.ok(body.includes("### Diagnostics on these axioms"));
  assert.ok(
    body.includes(
      "- [Merge.kif:120](https://github.com/ontologyportal/sumo/blob/abc/Merge.kif#L120) **error** `semantic/domain-mismatch`: argument 2 is not a Class",
    ),
  );
  assert.ok(body.indexOf("### Diagnostics") < body.indexOf("### Full proof"));
  const clean = buildContradictionIssue({ ...base, diagnostics: [] }).body;
  assert.ok(!clean.includes("### Diagnostics"));
});

test("an oversized proof is truncated under GitHub's limit", () => {
  const big = Array.from({ length: 4000 }, (_, i) =>
    derived(i + 3, "resolution", [0], `(p ${"x".repeat(30)} ${i})`),
  );
  const { body } = buildContradictionIssue({
    steps: [...STEPS.slice(0, 3), ...big],
    prose: "",
    fingerprint: "f",
    blobBase: "https://example.invalid",
    commit: null,
    context: [],
  });
  assert.ok(body.length <= 60_000, `body is ${body.length} chars`);
  assert.ok(body.includes("(truncated: proof exceeds"));
  assert.ok(body.includes(fingerprintMarker("f")));
  assert.ok(!body.includes("Loaded at commit"));
});

const FILE = [
  "; header comment (with a paren",
  "(instance Fido Plant)",
  "(=>",
  '  (documentation Foo EnglishLanguage "a (quoted) \\"paren\\"")',
  "  ; a ) in a comment",
  "  (bar ?X))",
  "(baz",
].join("\n");

test("formLineSpan follows parens across lines, skipping strings/comments", () => {
  assert.deepEqual([...formLineSpan(FILE, 2)], [2, 2]);
  assert.deepEqual([...formLineSpan(FILE, 3)], [3, 6]);
});

test("formLineSpan falls back to the start line when unbalanced or out of range", () => {
  assert.deepEqual([...formLineSpan(FILE, 7)], [7, 7]);
  assert.deepEqual([...formLineSpan(FILE, 99)], [99, 99]);
});

const diag = (file, line, severity, code, end_line = line) => ({
  file,
  line,
  col: 1,
  end_line,
  end_col: 1,
  severity,
  kind: "semantic",
  code,
  message: code,
});

test("axiomDiagnostics matches diagnostics inside each axiom's span", () => {
  const steps = [
    axiom(0, "(instance Fido Plant)", "A.kif", 2),
    axiom(1, "(=> ...)", "A.kif", 3),
    axiom(2, "(=> ...)", "A.kif", 3),
    derived(3, "resolution", [0, 1], "FALSE"),
  ];
  const diags = [
    diag("A.kif", 5, "hint", "inner-hint"),
    diag("A.kif", 4, "error", "inner-error"),
    diag("A.kif", 2, "warning", "on-fido"),
    diag("A.kif", 7, "error", "after"),
    diag("B.kif", 3, "error", "other-file"),
    { ...diag(null, null, "hint", "unlocated") },
  ];
  const out = axiomDiagnostics(steps, diags, (f) =>
    f === "A.kif" ? FILE : null,
  );
  assert.equal(out.length, 2, "duplicate file:line collapses");
  assert.deepEqual(
    out[0].diagnostics.map((d) => d.code),
    ["on-fido"],
  );
  assert.deepEqual([...out[1].span], [3, 6]);
  assert.deepEqual(
    out[1].diagnostics.map((d) => d.code),
    ["inner-error", "inner-hint"],
  );
});

test("axiomDiagnostics narrows to the start line without file text", () => {
  const out = axiomDiagnostics(
    [axiom(0, "(p)", "C.kif", 3)],
    [diag("C.kif", 3, "error", "hit"), diag("C.kif", 4, "error", "miss")],
    () => null,
  );
  assert.deepEqual(
    out[0].diagnostics.map((d) => d.code),
    ["hit"],
  );
});

const { contradictionKey, summarizeBatches } = exports;

test("contradictionKey ignores step order and derived steps", () => {
  const a = contradictionKey(STEPS);
  const b = contradictionKey([
    STEPS[4],
    STEPS[2],
    STEPS[0],
    STEPS[1],
    STEPS[3],
  ]);
  assert.equal(a, b);
  assert.equal(a, "Merge.kif:120|Merge.kif:300|Mid-level-ontology.kif:7");
  assert.notEqual(a, contradictionKey(STEPS.slice(0, 2)));
});

test("summarizeBatches counts subproblem outcomes", () => {
  const s = summarizeBatches([
    { status: "Consistent", stop_reason: null },
    { status: "Consistent", stop_reason: null },
    { status: "Inconsistent", stop_reason: null },
    { status: "Timeout", stop_reason: "TimeLimit" },
    { status: "Unknown", stop_reason: "StepLimit" },
    { status: "Unknown", stop_reason: "GaveUp" },
    { status: "Crashed", stop_reason: null },
  ]);
  assert.deepEqual(
    { ...s },
    {
      total: 7,
      clean: 2,
      contradictory: 1,
      timeLimit: 1,
      stepLimit: 1,
      crashed: 1,
      other: 1,
    },
  );
});
