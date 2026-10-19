import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";

const exports = {};
vm.runInNewContext(
  ts.transpileModule(
    readFileSync(new URL("./kifInput.ts", import.meta.url), "utf8"),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    },
  ).outputText,
  { exports },
);
const {
  isFormula,
  parensBalanced,
  isBlankFormula,
  openParen,
  termToFormula,
  closeParen,
  deletePair,
  wordAt,
  acceptCompletion,
  rankCompletions,
} = exports;

const plain = (v) => JSON.parse(JSON.stringify(v));

test("isFormula keys on a leading paren", () => {
  assert.equal(isFormula("(instance ?X Dog)"), true);
  assert.equal(isFormula("  ("), true);
  assert.equal(isFormula("Human"), false);
  assert.equal(isFormula(""), false);
});

test("parensBalanced", () => {
  assert.equal(parensBalanced("(instance ?X Dog)"), true);
  assert.equal(parensBalanced("(=> (p ?X) (q ?X))"), true);
  assert.equal(parensBalanced("(instance ?X"), false);
  assert.equal(parensBalanced("(p))("), false);
  assert.equal(parensBalanced('(comment X "a ( b")'), true);
  assert.equal(parensBalanced('(comment X "a \\" (")'), true);
  assert.equal(parensBalanced('(comment X "open)'), false);
});

test("isBlankFormula", () => {
  assert.equal(isBlankFormula("()"), true);
  assert.equal(isBlankFormula(" ( ) "), true);
  assert.equal(isBlankFormula("(p)"), false);
});

test("openParen pairs at the caret and wraps a selection", () => {
  assert.deepEqual(plain(openParen("(=> )", 4, 4)), {
    text: "(=> ())",
    caret: 5,
  });
  assert.deepEqual(plain(openParen("(p a b)", 3, 6)), {
    text: "(p (a b))",
    caret: 7,
  });
});

test("termToFormula wraps the whole term", () => {
  assert.deepEqual(plain(termToFormula("")), { text: "()", caret: 1 });
  assert.deepEqual(plain(termToFormula("instance")), {
    text: "(instance)",
    caret: 9,
  });
});

test("closeParen steps over an existing paren only", () => {
  assert.deepEqual(plain(closeParen("(p)", 2)), { text: "(p)", caret: 3 });
  assert.equal(closeParen("(p", 2), null);
});

test("deletePair removes an empty pair only", () => {
  assert.deepEqual(plain(deletePair("(p ())", 4)), { text: "(p )", caret: 3 });
  assert.equal(deletePair("(p)", 2), null);
  assert.equal(deletePair("()", 0), null);
});

test("wordAt finds the symbol under the caret and skips variables", () => {
  assert.deepEqual(plain(wordAt("(inst ?X Dog)", 5)), {
    start: 1,
    end: 5,
    word: "inst",
  });
  assert.deepEqual(plain(wordAt("(instance ?X Do)", 15)), {
    start: 13,
    end: 15,
    word: "Do",
  });
  assert.deepEqual(plain(wordAt("(sub-class X)", 7)), {
    start: 1,
    end: 10,
    word: "sub-class",
  });
  assert.equal(wordAt("(instance ?X", 12), null);
  assert.equal(wordAt("(p @ROW", 7), null);
  assert.equal(wordAt("(p ", 3), null);
});

test("acceptCompletion replaces the word", () => {
  assert.deepEqual(plain(acceptCompletion("(inst ?X)", 1, 5, "instance")), {
    text: "(instance ?X)",
    caret: 9,
  });
});

test("rankCompletions filters, dedupes and puts prefixes first", () => {
  assert.deepEqual(
    plain(
      rankCompletions(
        ["subclass", "Dog", "instance", "immediateInstance", "instance", "Ins"],
        "ins",
      ),
    ),
    ["Ins", "instance", "immediateInstance"],
  );
  assert.deepEqual(plain(rankCompletions(["a", "ab", "abc"], "a", 2)), [
    "a",
    "ab",
  ]);
  assert.deepEqual(plain(rankCompletions([], "x")), []);
});
