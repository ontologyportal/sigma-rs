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
const { formatKif } = await import("./sdk.mjs");

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
