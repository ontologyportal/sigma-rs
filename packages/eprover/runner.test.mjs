import assert from "node:assert/strict";
import test from "node:test";
import { runE } from "./runner.mjs";

const base = new URL("./dist/", import.meta.url);
const input =
  "fof(kb_42,axiom,p(a)).\nfof(kb_43,axiom,~p(a)).\nfof(kb_44,axiom,q(b)).\n";

test("filter runner bounds, deduplicates, and preserves named premises", async () => {
  const result = await runE(
    input,
    JSON.stringify({ budget: 10, subsetLimit: 10 }),
    "e_axfilter",
    base,
  );
  assert.ok(result.subsets.length > 0 && result.subsets.length <= 10);
  assert.ok(result.subsets.some((text) => text.includes("kb_42")));
  const outcomes = await Promise.all(
    result.subsets.map((text) =>
      runE(
        text,
        JSON.stringify(["--auto", "--proof-object", "--tstp-format"]),
        "eprover",
        base,
      ),
    ),
  );
  assert.ok(
    outcomes.some((out) => /SZS status Unsatisfiable/.test(out.stdout)),
  );
  assert.ok(outcomes.some((out) => /kb_42/.test(out.stdout)));
  assert.ok(outcomes.every((out) => !/SinE strategy is|sine = Auto/.test(out.stdout)));
});

test("runner rejects invalid filter controls", async () => {
  await assert.rejects(
    runE(input, '{"budget":0,"subsetLimit":1}', "e_axfilter", base),
    /Invalid/,
  );
});

test("empty filtered theory remains an empty queue", async () => {
  const result = await runE(
    "",
    '{"budget":10,"subsetLimit":2}',
    "e_axfilter",
    base,
  );
  assert.deepEqual(result.subsets, []);
});
