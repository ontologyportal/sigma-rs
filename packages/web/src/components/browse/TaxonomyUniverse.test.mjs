import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import { parse } from "@vue/compiler-sfc";
import ts from "typescript";

const taxonomyExports = {};
vm.runInNewContext(
  ts.transpileModule(
    readFileSync(
      new URL("../../services/taxonomy-3d.ts", import.meta.url),
      "utf8",
    ),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    },
  ).outputText,
  { exports: taxonomyExports },
);
const { RELATION_COLORS } = taxonomyExports;

const { descriptor } = parse(
  readFileSync(new URL("./TaxonomyUniverse.vue", import.meta.url), "utf8"),
);
const source = ts.transpileModule(
  descriptor.scriptSetup.content +
    `
exports.controls = {
  choose, down, move, up, draw, reset, magnify,
  setup(element, data) { canvas.value = element; pages.value = data; },
  snapshot() { return { selected: selected.value, focused: focused.value, yaw, pitch, zoom, panX, panY, projected }; },
};
`,
  {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  },
).outputText;

function fixture(edges = []) {
  const exports = {};
  const watchers = [];
  const nodes = [
    { name: "Entity", position: [0, 0, 0], depth: 0, weight: 2 },
    { name: "Human", position: [100, 80, 60], depth: 1, weight: 1 },
  ];
  const strokes = [];
  const context = new Proxy(
    {},
    {
      get: (target, key) => {
        if (key === "createRadialGradient")
          return () => ({ addColorStop() {} });
        if (key === "stroke")
          return () =>
            strokes.push({
              color: target.strokeStyle,
              alpha: target.globalAlpha,
              width: target.lineWidth,
            });
        return target[key] ?? (() => {});
      },
      set: (target, key, value) => {
        target[key] = value;
        return true;
      },
    },
  );
  const element = {
    clientWidth: 800,
    clientHeight: 600,
    getContext: () => context,
    getBoundingClientRect: () => ({ left: 20, top: 30 }),
    setPointerCapture() {},
  };
  const imports = {
    vue: {
      ref: (value) => ({ value }),
      shallowRef: (value) => ({ value }),
      computed: (getter) => ({
        get value() {
          return getter();
        },
      }),
      watch: (source, callback) => watchers.push({ source, callback }),
      onMounted() {},
      onBeforeUnmount() {},
    },
    "../../services/sigma": {
      call() {
        throw new Error("Unexpected worker request");
      },
    },
    "../../stores/kb": { useKBStore: () => ({ promoting: false }) },
    "../../utils/format": { errMsg: String },
    "../../services/taxonomy-3d": {
      RELATION_COLORS,
      layoutTaxonomy: () => ({ nodes, edges }),
    },
  };
  vm.runInNewContext(source, {
    exports,
    require: (name) => imports[name],
    defineEmits: () => () => {},
    requestAnimationFrame: () => 1,
    devicePixelRatio: 1,
  });
  const controls = exports.controls;
  controls.setup(
    element,
    new Map(nodes.map((n) => [n.name, { parents: [], children: [] }])),
  );
  controls.draw();
  return { ...controls, nodes, watchers, strokes };
}
function click(f, name) {
  const point = f.snapshot().projected.find((p) => p.node.name === name);
  const event = {
    button: 0,
    pointerId: 1,
    clientX: point.x + 20,
    clientY: point.y + 30,
  };
  f.down(event);
  f.up(event);
  f.draw();
}
function assertCentered(f, name) {
  const point = f.snapshot().projected.find((p) => p.node.name === name);
  assert.equal(point.x, 400);
  assert.equal(point.y, 300);
  assert.equal(point.z, 0);
}

test("clicking a ball centers it without changing rotation or zoom", () => {
  const f = fixture();
  f.magnify(2);
  f.draw();
  const before = f.snapshot();
  click(f, "Human");
  const after = f.snapshot();
  assert.equal(after.selected, "Human");
  assert.equal(after.focused, "Human");
  assert.equal(after.yaw, before.yaw);
  assert.equal(after.pitch, before.pitch);
  assert.equal(after.zoom, before.zoom);
  assertCentered(f, "Human");

  f.down({ button: 0, pointerId: 2, clientX: 0, clientY: 0 });
  f.move({ pointerId: 2, clientX: 60, clientY: 30 });
  f.up({ pointerId: 2, clientX: 60, clientY: 30 });
  f.magnify(1.5);
  f.draw();
  assertCentered(f, "Human");
});
test("dragging does not select; selecting the same term clears panning", () => {
  const f = fixture();
  f.choose("Human");
  f.draw();
  f.down({
    button: 0,
    pointerId: 1,
    clientX: 400,
    clientY: 300,
    shiftKey: true,
  });
  f.move({ pointerId: 1, clientX: 480, clientY: 330 });
  f.up({ pointerId: 1, clientX: 480, clientY: 330 });
  f.draw();
  assert.equal(f.snapshot().focused, "Human");
  assert.equal(f.snapshot().panX, 80);
  click(f, "Human");
  assertCentered(f, "Human");
  f.choose("Missing");
  assert.equal(f.snapshot().focused, "Human");
});
test("reset and filtering away the focus return the camera to Entity", () => {
  const f = fixture();
  f.choose("Human");
  f.magnify(2);
  f.reset();
  f.draw();
  assertCentered(f, "Entity");
  assert.equal(f.snapshot().zoom, 1);
  f.choose("Human");
  f.nodes.pop();
  f.watchers[0].callback();
  f.draw();
  assert.equal(f.snapshot().focused, "Entity");
  assertCentered(f, "Entity");
});

test("edges keep distinct relationship colors when selected and unselected", () => {
  const relations = ["subclass", "instance", "subrelation", "subAttribute"];
  assert.equal(
    new Set(relations.map((r) => RELATION_COLORS[r])).size,
    relations.length,
  );
  const f = fixture(
    relations.map((relation) => ({
      child: "Human",
      parent: "Entity",
      relation,
    })),
  );
  for (const relation of relations) {
    const strokes = f.strokes.filter(
      (s) => s.color === RELATION_COLORS[relation],
    );
    assert.equal(
      strokes.length,
      2,
      "selected edge and arrow use the relationship color",
    );
    assert.ok(strokes.every((s) => s.alpha === 1));
  }
  f.nodes.push({ name: "Other", position: [-100, 0, 0], depth: 1, weight: 1 });
  f.choose("Other");
  f.strokes.length = 0;
  f.draw();
  for (const relation of relations) {
    const strokes = f.strokes.filter(
      (s) => s.color === RELATION_COLORS[relation],
    );
    assert.equal(strokes.length, 1);
    assert.ok(strokes[0].alpha >= 0.5, "unselected colors remain visible");
    assert.ok(strokes[0].width >= 1);
  }
});
