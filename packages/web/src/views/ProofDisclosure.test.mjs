import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { parse } from "@vue/compiler-sfc";

function template(name) {
  const source = readFileSync(new URL(name, import.meta.url), "utf8");
  return parse(source).descriptor.template.content.replace(/\s+/g, " ");
}

test("audit contradiction headings toggle their full proof", () => {
  const source = template("./AuditTab.vue");
  assert.match(
    source,
    /<Disclosure :summary="heading\(c, i\)" primary> <ProofView[\s\S]*?<\/Disclosure>/,
  );
  assert.doesNotMatch(source, /class="contradiction-hd"/);
});

test("Ask/Tell proof is collapsed behind its step-count heading", () => {
  const source = template("./AskTellTab.vue");
  assert.match(
    source,
    /<Disclosure v-if="result" :summary="proofHeading" primary> <ProofView[\s\S]*?<\/Disclosure>/,
  );
});
