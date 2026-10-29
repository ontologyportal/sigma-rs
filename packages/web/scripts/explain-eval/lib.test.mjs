import assert from "node:assert/strict";
import { test } from "node:test";
import {
  balanced,
  bootstrapCi,
  citedLocations,
  explainModule,
  extractKif,
  faultCandidates,
  flatAtom,
  inventedSymbols,
  isRepairOf,
  kbSymbolSet,
  ontologyFacts,
  paired,
  parseArgs,
  rng,
  sameKif,
  signTest,
  stepRefs,
  topLevelForms,
  unsupportedSymbols,
} from "./lib.mjs";

const KB = `; comment (with parens)
(subclass Animal Organism)
(subclass Plant Organism)
(disjoint Animal Plant)
(subclass Dog Animal)
(instance Rex Dog)
(documentation Dog EnglishLanguage "A (dog) \\"quoted\\" café")
(domain owns 1 Animal)
(domain owns 2 Plant)
(owns Rex Fern)
(=> (instance ?X Dog)
    (instance ?X Animal))
`;

test("topLevelForms reports lines and UTF-8 byte offsets", () => {
  const forms = topLevelForms(KB);
  assert.equal(forms.length, 10);
  assert.equal(forms[0].kif, "(subclass Animal Organism)");
  assert.equal(forms[0].line, 2);
  for (const f of forms)
    assert.equal(
      Buffer.from(KB).subarray(f.offset).toString().startsWith(f.kif),
      true,
    );
  assert.equal(forms[9].line, 11);
  assert.match(forms[5].kif, /café"\)$/);
});

test("flatAtom only accepts ground atoms", () => {
  assert.deepEqual(flatAtom("(instance Rex Dog)"), {
    head: "instance",
    args: ["Rex", "Dog"],
  });
  assert.equal(flatAtom("(instance ?X Dog)"), null);
  assert.equal(flatAtom("(not (instance Rex Dog))"), null);
});

test("fault candidates cover each type from the taxonomy", () => {
  const forms = topLevelForms(KB);
  const facts = ontologyFacts([forms]);
  const cands = faultCandidates(forms, facts, rng(1));
  const byType = Object.fromEntries(cands.map((c) => [c.type, c]));
  assert.equal(byType["instance-class"].mutated, "(instance Rex Plant)");
  assert.equal(byType["subclass-parent"].mutated, "(subclass Dog Plant)");
  assert.equal(byType["arg-swap"].mutated, "(owns Fern Rex)");
  assert.equal(
    byType["disjoint-ancestor"].mutated,
    "(disjoint Animal Organism)",
  );
  const rel = faultCandidates(
    topLevelForms(
      "(subclass BinaryPredicate Relation)\n(subclass TernaryRelation Relation)\n(disjoint BinaryPredicate TernaryRelation)\n(instance part BinaryPredicate)",
    ),
    ontologyFacts([
      topLevelForms(
        "(subclass BinaryPredicate Relation)\n(subclass TernaryRelation Relation)\n(disjoint BinaryPredicate TernaryRelation)",
      ),
    ]),
    rng(1),
  ).find((c) => c.original === "(instance part BinaryPredicate)");
  assert.equal(rel.type, "relation-class");
  const order = balanced(cands, rng(2)).map((c) => c.type);
  assert.equal(new Set(order.slice(0, 4)).size, 4, "interleaved by type");
  const c = byType["arg-swap"];
  assert.equal(KB.slice(c.startIndex, c.endIndex), "(owns Rex Fern)");
});

test("answer analysis", () => {
  const text = [
    "**Conflict**: Merge.kif:1111 and `Merge.kif` line 2848 clash (steps 2-4 and step 7, see [9]).",
    "**Why**: the BinaryPredicate class and FooBarBaz.",
    "**Possible fixes**:",
    "- Change it to `(instance component\n   BinaryPredicate)`.",
    "- Or ( not this ) or (remove Organism).",
  ].join("\n");
  assert.deepEqual([...citedLocations(text)].sort(), [
    "Merge.kif:1111",
    "Merge.kif:2848",
  ]);
  assert.deepEqual(
    [...stepRefs(text)].sort((a, b) => a - b),
    [2, 3, 4, 7, 9],
  );
  assert.deepEqual(extractKif(text), [
    "(instance component BinaryPredicate)",
    "(remove Organism)",
  ]);
  const kb = new Set([
    "BinaryPredicate",
    "Organism",
    "Park",
    "The",
    "instance",
  ]);
  assert.deepEqual(
    [...unsupportedSymbols(text, kb, "(instance x y)")],
    ["BinaryPredicate"],
  );
  assert.deepEqual(
    [...unsupportedSymbols("The Park is big. A `Organism` here.", kb, "")],
    ["Organism"],
  );
  assert.deepEqual([...inventedSymbols(text, kb)], ["FooBarBaz"]);
  assert.deepEqual(
    [
      ...inventedSymbols(
        "JohnsHouse and FooBarBaz",
        kb,
        "(owns John JohnsHouse)",
      ),
    ],
    ["FooBarBaz"],
  );
});

test("kbSymbolSet ignores comments and strings", () => {
  const s = kbSymbolSet([KB]);
  assert.ok(s.has("Rex") && s.has("documentation"));
  assert.ok(!s.has("comment") && !s.has("quoted"));
});

test("repair matching", () => {
  assert.ok(
    isRepairOf(
      "(instance Rex Dog)",
      "(instance Rex Plant)",
      "(instance Rex Dog)",
    ),
  );
  assert.ok(
    isRepairOf(
      "(instance Rex Mammal)",
      "(instance Rex Plant)",
      "(instance Rex Dog)",
    ),
  );
  assert.ok(
    !isRepairOf(
      "(subclass Rex Dog)",
      "(instance Rex Plant)",
      "(instance Rex Dog)",
    ),
  );
  assert.ok(
    !isRepairOf(
      "(instance Fido Cat)",
      "(instance Rex Plant)",
      "(instance Rex Dog)",
    ),
  );
  assert.ok(sameKif("( instance  Rex\nDog )", "(instance Rex Dog)"));
});

test("statistics", () => {
  assert.equal(signTest(0, 0), 1);
  assert.ok(Math.abs(signTest(10, 0) - 2 / 1024) < 1e-12);
  assert.ok(Math.abs(signTest(5, 5) - 1) < 1e-12);
  const a = new Map([
    ["x", 1],
    ["y", 1],
    ["z", 0],
    ["w", NaN],
  ]);
  const b = new Map([
    ["x", 0],
    ["y", 1],
    ["z", 1],
    ["w", 1],
  ]);
  assert.deepEqual(paired(a, b), { wins: 1, losses: 1, ties: 1, p: 1 });
  const [lo, hi] = bootstrapCi([0, 1, 0, 1, 1, 1, 0, 1], rng(3));
  assert.ok(lo < 0.625 && hi > 0.625);
});

test("parseArgs", () => {
  const o = parseArgs(["--samples", "2", "--dry-run", "--model=x"], {
    samples: 3,
    dryRun: false,
    model: "m",
  });
  assert.deepEqual(o, { samples: 2, dryRun: true, model: "x" });
  assert.throws(() => parseArgs(["--nope"], { a: 1 }), /unknown option/);
});

test("prompt formats share everything but the proof rendering", () => {
  const ex = explainModule();
  const req = {
    kind: "ask",
    query: "(instance Rex Animal)",
    steps: [
      {
        index: 0,
        rule: "axiom",
        premises: [],
        kif: "(instance Rex Dog)",
        file: "a.kif",
        line: 1,
      },
      { index: 1, rule: "resolve", premises: [0], kif: "FALSE" },
    ],
  };
  const opts = {
    paraphrases: ["Rex is an instance of dog", "a contradiction"],
    queryParaphrase: "Rex is an instance of animal",
    narrative: "We want to show that Rex is an animal.",
  };
  const kif = ex.buildMessages(req);
  assert.deepEqual(ex.buildMessages(req, { format: "kif", ...opts }), kif);
  const para = ex.buildMessages(req, { format: "paraphrase", ...opts });
  assert.equal(
    para[1].content,
    "PROOF DATA\nQuery: Rex is an instance of animal\nSteps:\n[1] axiom (a.kif:1): Rex is an instance of dog\n[2] resolve from [1]: a contradiction",
  );
  const both = ex.buildMessages(req, { format: "both", ...opts });
  assert.match(
    both[1].content,
    /\[1\] axiom \(a\.kif:1\): \(instance Rex Dog\)\n {2}English: Rex is an instance of dog/,
  );
  assert.match(
    both[1].content,
    /Query: \(instance Rex Animal\)\n {2}English: Rex is an instance of animal/,
  );
  const narr = ex.buildMessages(req, { format: "narrative", ...opts });
  assert.equal(
    narr[1].content,
    "PROOF DATA\nProof:\nWe want to show that Rex is an animal.",
  );
  const sys = (m) => m[0].content.split("\n");
  for (const other of [para, both, narr]) {
    const diff = sys(other).filter((l, i) => l !== sys(kif)[i]);
    assert.equal(diff.length, 1, "only the step-shape sentence differs");
  }
});

test("readModelOutput drops a reasoning block", () => {
  const ex = explainModule();
  assert.equal(
    ex.readModelOutput({ response: "<think>hmm</think>\n\nAnswer." }).text,
    "Answer.",
  );
  assert.equal(
    ex.readModelOutput({
      choices: [{ message: { content: "Hi" } }],
      usage: { prompt_tokens: 3, completion_tokens: 1 },
    }).inputTokens,
    3,
  );
});
