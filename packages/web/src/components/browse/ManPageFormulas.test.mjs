import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import { parse } from "@vue/compiler-sfc";
import ts from "typescript";

const { descriptor } = parse(
  readFileSync(new URL("./ManPageFormulas.vue", import.meta.url), "utf8"),
);
const source = ts.transpileModule(
  descriptor.scriptSetup.content +
    "\nexports.controls = { shownRefs, filter, note };",
  {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  },
).outputText;

function fixture(references) {
  const exports = {};
  const vue = {
    ref: (value) => ({ value }),
    computed: (get) => ({
      get value() {
        return get();
      },
    }),
    watch() {},
  };
  vm.runInNewContext(source, {
    exports,
    require: (name) => (name === "vue" ? vue : {}),
    defineProps: () => ({ page: { references } }),
  });
  return exports.controls;
}

test("definitions precede signatures, biconditionals, rules, and other uses", () => {
  const refs = [
    { kind: "fact", head: "located", arg_pos: 1, file: "Merge.kif" },
    { kind: "=>", head: null, arg_pos: null, file: "Merge.kif" },
    { kind: "<=>", head: null, arg_pos: null, file: "Local.kif" },
    { kind: "fact", head: "domain", arg_pos: 1, file: "Local.kif" },
    { kind: "taxonomy", head: "subclass", arg_pos: 1, file: "Local.kif" },
    { kind: "taxonomy", head: "instance", arg_pos: 1, file: "Local.kif" },
  ];
  const before = [...refs];
  const f = fixture(refs);
  assert.deepEqual(Array.from(f.shownRefs.value), [
    refs[4],
    refs[5],
    refs[3],
    refs[2],
    refs[1],
    refs[0],
  ]);
  assert.deepEqual(refs, before);
  f.filter.value = "taxonomy";
  assert.deepEqual(Array.from(f.shownRefs.value), [refs[4], refs[5]]);
});

test("child, relation-head, and negated appearances do not rank as declarations", () => {
  const refs = [
    { kind: "taxonomy", head: "subclass", arg_pos: 2, file: "Merge.kif" },
    { kind: "taxonomy", head: "instance", arg_pos: 2, file: "Merge.kif" },
    { kind: "taxonomy", head: "subclass", arg_pos: 0, file: "Merge.kif" },
    {
      kind: "fact",
      head: null,
      arg_pos: 1,
      kif: "(not (subclass Dog Plant))",
      file: "Merge.kif",
    },
    { kind: "taxonomy", head: "subrelation", arg_pos: 1, file: "Local.kif" },
    { kind: "taxonomy", head: "subAttribute", arg_pos: 1, file: "Local.kif" },
  ];
  assert.deepEqual(Array.from(fixture(refs).shownRefs.value), [
    refs[4],
    refs[5],
    ...refs.slice(0, 4),
  ]);
});

test("source priorities and stable ties apply within each relevance tier", () => {
  const refs = [
    { kind: "fact", head: "range", arg_pos: 1, file: null },
    {
      kind: "fact",
      head: "rangeSubclass",
      arg_pos: 1,
      file: "Mid-level-ontology.kif",
    },
    { kind: "fact", head: "domainSubclass", arg_pos: 1, file: "Merge.kif" },
    { kind: "fact", head: "valence", arg_pos: 1, file: "Merge.kif" },
  ];
  assert.deepEqual(Array.from(fixture(refs).shownRefs.value), [
    refs[2],
    refs[3],
    refs[1],
    refs[0],
  ]);
});

test("all statements remain visible by default, including documentation and formats", () => {
  const refs = ["documentation", "termFormat", "format"].map((head) => ({
    head,
    kind: "doc",
    arg_pos: head === "documentation" ? 1 : 2,
    file: null,
  }));
  refs.push({ head: null, kind: "other", arg_pos: null, file: null });
  const f = fixture(refs);
  assert.deepEqual(Array.from(f.shownRefs.value), refs);
  assert.match(f.note.value, /appears in 4 formulas/);
  assert.doesNotMatch(f.note.value, /omitted|not shown/);
  f.filter.value = "doc";
  assert.deepEqual(Array.from(f.shownRefs.value), refs.slice(0, 3));
  f.filter.value = "";
  assert.deepEqual(Array.from(f.shownRefs.value), refs);
  const empty = fixture([]);
  assert.deepEqual(Array.from(empty.shownRefs.value), []);
  assert.equal(empty.note.value, "appears in no formulas");
});
