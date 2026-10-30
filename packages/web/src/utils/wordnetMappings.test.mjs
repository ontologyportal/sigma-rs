import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";

const exports = {};
vm.runInNewContext(
  ts.transpileModule(
    readFileSync(new URL("./wordnetMappings.ts", import.meta.url), "utf8"),
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
  rewriteAnchors,
  anchorText,
  parseAnchors,
  synsetKey,
  diffMappings,
  isValidTerm,
} = exports;

const DOG =
  "02084071 05 n 03 dog 0 domestic_dog 0 Canis_familiaris 0 000 | a domesticated canine &%Canine+";

test("rewriteAnchors replaces the whole run, keeping the gloss", () => {
  assert.equal(
    rewriteAnchors(DOG, [
      { term: "DomesticDog", suffix: "=" },
      { term: "Pet", suffix: "+" },
    ]),
    DOG.replace("&%Canine+", "&%DomesticDog= &%Pet+"),
  );
});

test("rewriteAnchors with no anchors removes the run", () => {
  assert.equal(rewriteAnchors(DOG, []), DOG.replace(" &%Canine+", ""));
});

test("rewriteAnchors adds a run to an unmapped record and keeps CRLF", () => {
  const bare = "00001740 03 n 01 entity 0 000 | that which exists  \r";
  assert.equal(
    rewriteAnchors(bare, [{ term: "Entity", suffix: "=" }]),
    "00001740 03 n 01 entity 0 000 | that which exists &%Entity=\r",
  );
});

test("rewriteAnchors rejects non-records and invalid terms", () => {
  assert.throws(() => rewriteAnchors(";; comment", []), /Not a WordNet/);
  assert.throws(
    () => rewriteAnchors(DOG, [{ term: "Bad Term", suffix: "=" }]),
    /Invalid SUMO term/,
  );
  assert.equal(isValidTerm("Dog_2-x"), true);
  assert.equal(isValidTerm(""), false);
  assert.equal(isValidTerm("&%Dog"), false);
});

test("parseAnchors reads a record or a bare run, dropping unknown suffixes", () => {
  const plain = (xs) => JSON.parse(JSON.stringify(xs));
  assert.deepEqual(plain(parseAnchors(DOG)), [{ term: "Canine", suffix: "+" }]);
  assert.deepEqual(plain(parseAnchors("&%Dog= &%Pet+ &%Odd! &%Not[")), [
    { term: "Dog", suffix: "=" },
    { term: "Pet", suffix: "+" },
    { term: "Not", suffix: "[" },
  ]);
  assert.equal(parseAnchors("").length, 0);
  assert.equal(parseAnchors(rewriteAnchors(DOG, [])).length, 0);
});

test("anchorText and synsetKey read a record", () => {
  assert.equal(anchorText(DOG), "&%Canine+");
  assert.equal(anchorText(";; comment"), "");
  assert.equal(synsetKey(DOG), "02084071-n");
  assert.equal(
    synsetKey("00001740 00 s 01 able 0 000 | having the means"),
    "00001740-a",
  );
  assert.equal(synsetKey(";; 1 This software"), null);
});

test("diffMappings reports changed records by identity, not line number", () => {
  const base = [
    ";; header",
    DOG,
    "03100490 06 n 01 car 0 000 | a car &%Automobile=",
  ].join("\n");
  const current = [
    ";; header",
    ";; an extra upstream line",
    rewriteAnchors(DOG, [{ term: "DomesticDog", suffix: "=" }]),
    "03100490 06 n 01 car 0 000 | a car &%Automobile=",
  ].join("\n");
  assert.deepEqual(JSON.parse(JSON.stringify(diffMappings(base, current))), [
    {
      key: "02084071-n",
      line: 3,
      word: "dog",
      before: "&%Canine+",
      after: "&%DomesticDog=",
    },
  ]);
  assert.equal(diffMappings(current, current).length, 0);
});
