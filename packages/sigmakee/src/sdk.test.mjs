import assert from "node:assert/strict";
import { registerHooks } from "node:module";
import { test } from "node:test";

// `sdk.mjs` imports the wasm-bindgen glue, which only exists in `dist/` after a
// build; `formatKif` is pure JS, so stub the glue out and test the source.
const WASM_STUB =
  "data:text/javascript," +
  encodeURIComponent(
    "export default async () => {}; export class Session {} export class Config {}" +
      " export const parseTest = () => {}, parseTptpTest = () => {}, sumoSymbols = () => ({});",
  );
registerHooks({
  resolve(specifier, context, next) {
    if (specifier === "./sumo_parser_wasm.js")
      return { url: WASM_STUB, shortCircuit: true };
    return next(specifier, context);
  },
});
const { formatKif, formatTest } = await import("./sdk.mjs");

const EXPECTED = [
  "(=>",
  "  (instance ?M Meteoroid)",
  "  (exists (?L)",
  "    (and",
  "      (instance ?L LengthMeasure)",
  "      (equal ?L",
  "        (MeasureFn ?L Meter))",
  "      (length ?M ?L)",
  "      (lessThanOrEqualTo ?L 1.0))))",
].join("\n");

test("formatKif re-indents tab indentation", () => {
  const src = [
    "(=>",
    "\t(instance ?M Meteoroid)",
    "\t(exists (?L)",
    "\t\t(and",
    "      (instance ?L LengthMeasure)",
    "      (equal ?L",
    "        (MeasureFn ?L Meter))",
    "\t\t\t(length ?M ?L)",
    "\t\t\t(lessThanOrEqualTo ?L 1.0))))",
  ].join("\n");
  const out = formatKif(src);
  assert.equal(out, EXPECTED);
  assert.ok(!out.includes("\t"));
});

test("formatKif re-indents ragged space indentation", () => {
  const src = [
    "(=>",
    "        (instance ?M Meteoroid)",
    "        (exists (?L)",
    "                (and",
    "      (instance ?L LengthMeasure)",
    "      (equal ?L",
    "        (MeasureFn ?L Meter))",
    "                        (length ?M ?L)",
    "                        (lessThanOrEqualTo ?L 1.0))))",
  ].join("\n");
  assert.equal(formatKif(src), EXPECTED);
});

test("formatKif is idempotent", () => {
  assert.equal(formatKif(EXPECTED), EXPECTED);
});

test("formatTest writes every harness directive", () => {
  assert.equal(
    formatTest({
      note: "Astronomy_4",
      categories: ["Astronomy", "Space Science"],
      timeout: 30,
      extraFiles: ["Astronomy.kif"],
      assertions: "(instance EX1 Exoplanet)",
      query: "(instance EX1 Planet)",
      expectedProof: true,
    }),
    [
      "(note Astronomy_4)",
      "(category Astronomy)",
      '(category "Space Science")',
      "(time 30)",
      '(file "Astronomy.kif")',
      "(instance EX1 Exoplanet)",
      "(query (instance EX1 Planet))",
      "(answer yes)",
      "",
    ].join("\n"),
  );
});

test("formatTest omits absent directives and prefers bindings", () => {
  assert.equal(
    formatTest({
      note: "a note",
      query: "(p ?X)",
      expectedProof: true,
      expectedAnswer: ["Rex", " "],
    }),
    '(note "a note")\n(query (p ?X))\n(answer Rex)\n',
  );
  assert.equal(formatTest({ query: "(p a)" }), "(query (p a))\n");
  assert.equal(
    formatTest({ query: "(p a)", expectedProof: false }),
    "(query (p a))\n(answer no)\n",
  );
});
