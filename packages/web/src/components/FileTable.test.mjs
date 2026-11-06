import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import { parse, compileScript, compileTemplate } from "@vue/compiler-sfc";
import * as vue from "vue";
import ts from "typescript";

const { descriptor } = parse(
  readFileSync(new URL("./FileTable.vue", import.meta.url), "utf8"),
);
const source = ts.transpileModule(
  compileScript(descriptor, { id: "file-table" }).content,
  {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  },
).outputText;
function fixture(directories = true) {
  const selected = vue.ref(new Set());
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    require: (name) =>
      name === "vue"
        ? {
            ...vue,
            onMounted() {},
            onBeforeUnmount() {},
            useModel: () => selected,
          }
        : {},
  });
  const rows = ["root.p", "CASC/a.p", "CASC/nested/b.p", "other/c.p"].map(
    (name) => ({
      key: name,
      name,
      origin: { kind: "file" },
      source: "Local",
      size: 1,
      loaded: true,
    }),
  );
  const props = vue.reactive({
    rows,
    directories,
    selectOnly: true,
    checkedIsLoaded: false,
  });
  const state = exports.default.setup(props, { expose() {}, emit() {} });
  return { ...state, props, selected };
}
const names = (state) =>
  Array.from(state.displayedRows.value, (row) => row.name);

test("folders expand and collapse recursively while retaining selection", () => {
  const s = fixture();
  assert.deepEqual(names(s), ["root.p"]);
  s.toggleDirectory("CASC/");
  assert.deepEqual(names(s), ["root.p", "CASC/a.p"]);
  s.toggleDirectory("CASC/nested/");
  s.toggle(s.props.rows[2]);
  s.toggleDirectory("CASC/");
  assert.deepEqual(names(s), ["root.p"]);
  assert.equal(s.selected.value.has("CASC/nested/b.p"), true);
  s.toggleDirectory("CASC/");
  assert.deepEqual(names(s), ["root.p", "CASC/a.p", "CASC/nested/b.p"]);
});
test("search reveals matches and restores collapsed browsing afterwards", async () => {
  const s = fixture();
  s.search.value = "b.p";
  await vue.nextTick();
  assert.deepEqual(names(s), ["CASC/nested/b.p"]);
  s.toggleDirectory("CASC/");
  assert.deepEqual(names(s), []);
  s.search.value = "";
  await vue.nextTick();
  assert.deepEqual(names(s), ["root.p"]);
  s.search.value = "missing";
  assert.equal(s.entries.value.length, 0);
});
test("shift selection skips collapsed descendants", () => {
  const s = fixture();
  s.toggleDirectory("other/");
  s.toggle(s.props.rows[0]);
  s.toggle(s.props.rows[3], { shiftKey: true });
  assert.deepEqual([...s.selected.value], ["root.p", "other/c.p"]);
});
test("flat tables preserve full paths and empty tables have no folders", () => {
  const s = fixture(false);
  assert.equal(s.entries.value.length, 4);
  assert.ok(s.entries.value.every((entry) => entry.name === entry.row.name));
  s.props.rows = [];
  assert.equal(s.entries.value.length, 0);
});
test("table template compiles", () => {
  const result = compileTemplate({
    source: descriptor.template.content,
    filename: "FileTable.vue",
    id: "file-table",
  });
  assert.deepEqual(result.errors, []);
});

test("tests root is omitted without changing file identities or nested folders", () => {
  const s = fixture();
  s.props.directoryRoot = "tests";
  s.props.rows = [
    "tests/root.p",
    "tests/CASC/a.p",
    "tests/CASC/nested/b.p",
    "testsmith/c.p",
    "local.p",
  ].map((name) => ({
    ...s.props.rows[0],
    key: name,
    name,
  }));
  assert.deepEqual(names(s), ["local.p", "tests/root.p"]);
  assert.equal(
    s.entries.value.some((entry) => entry.name === "tests"),
    false,
  );
  assert.equal(
    s.entries.value.find((entry) => entry.name === "root.p").depth,
    0,
  );
  assert.ok(s.entries.value.some((entry) => entry.name === "testsmith"));
  s.toggleDirectory("CASC/");
  assert.deepEqual(names(s), ["local.p", "tests/root.p", "tests/CASC/a.p"]);
  s.toggle(s.props.rows[1]);
  assert.equal(s.selected.value.has("tests/CASC/a.p"), true);
  s.toggleDirectory("CASC/");
  assert.deepEqual(names(s), ["local.p", "tests/root.p"]);
});

test("repository display paths hide identity prefixes and preserve selection identities", async () => {
  const s = fixture();
  const prefix = "gitlab/https%3A%2F%2Fgitlab.nps.edu/fsg/sumo/";
  s.props.rows = [
    "Anatomy.kif",
    "domains/Biology.kif",
    "domains/nested/Cells.kif",
  ].map((path) => ({
    ...s.props.rows[0],
    key: prefix + path,
    name: prefix + path,
    displayPath: path,
  }));
  assert.deepEqual(
    Array.from(s.entries.value, (e) => e.name),
    ["Anatomy.kif", "domains"],
  );
  s.toggleDirectory("domains/");
  assert.deepEqual(
    Array.from(s.entries.value, (e) => e.name),
    ["Anatomy.kif", "domains", "Biology.kif", "nested"],
  );
  s.toggleDirectory("domains/nested/");
  const cells = s.entries.value.find((e) => e.name === "Cells.kif");
  assert.equal(cells.depth, 2);
  s.toggle(cells.row);
  assert.ok(s.selected.value.has(prefix + "domains/nested/Cells.kif"));
  s.search.value = "Cells";
  await vue.nextTick();
  assert.deepEqual(names(s), [prefix + "domains/nested/Cells.kif"]);
});
