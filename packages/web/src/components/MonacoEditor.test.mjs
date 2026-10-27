import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import { compileScript, parse } from "@vue/compiler-sfc";
import * as vue from "vue";
import ts from "typescript";

const { descriptor } = parse(
  readFileSync(new URL("./MonacoEditor.vue", import.meta.url), "utf8"),
);
const source = ts.transpileModule(
  compileScript(descriptor, { id: "model-test" }).content,
  {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  },
).outputText;

async function fixture() {
  const mounted = [];
  const unmount = [];
  const models = [];
  function createModel(value, language) {
    const model = {
      value,
      language,
      disposed: false,
      dispose() {
        this.disposed = true;
      },
    };
    models.push(model);
    return model;
  }
  let model;
  let ownsModel = false;
  let position = { lineNumber: 1, column: 1 };
  const editor = {
    getModel: () => model,
    setModel(next) {
      // Monaco owns and disposes a model created from editor options.value.
      if (ownsModel && model !== next) {
        model.dispose();
        ownsModel = false;
      }
      if (next.disposed) throw new Error("Model is disposed!");
      model = next;
      position = { lineNumber: 1, column: 1 };
    },
    getValue: () => model.value,
    setValue(value) {
      model.value = value;
    },
    getPosition: () => position,
    setPosition(next) {
      position = next;
    },
    saveViewState: () => ({ position }),
    restoreViewState(view) {
      position = view.position;
    },
    onDidChangeModelContent() {},
    onDidChangeCursorPosition() {},
    dispose() {},
  };
  const monaco = {
    editor: {
      create(_host, options) {
        ownsModel = !options.model;
        model = options.model ?? createModel(options.value, options.language);
        return editor;
      },
      createModel,
      setModelLanguage(model, language) {
        model.language = language;
      },
    },
  };
  const imports = {
    vue: {
      ...vue,
      onMounted: (fn) => mounted.push(fn),
      onBeforeUnmount: (fn) => unmount.push(fn),
    },
    "../stores/shell": { useShellStore: () => ({ isDark: false }) },
    "../services/monaco": {
      loadMonaco: async () => monaco,
      themeName: () => "vs",
      lspEditor: () => null,
    },
  };
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    require: (name) => imports[name] ?? {},
  });
  const props = vue.reactive({
    modelValue: "first",
    documentKey: "a",
    language: "kif",
    options: {},
  });
  let api;
  const scope = vue.effectScope();
  const setup = scope.run(() =>
    exports.default.setup(props, {
      expose: (value) => {
        api = value;
      },
      emit() {},
    }),
  );
  setup.mount.value = {};
  for (const hook of mounted) await hook();
  return { props, api, editor, models, scope, unmount };
}

test("switching documents restores the original model, text, and cursor", async () => {
  const f = await fixture();
  try {
    const first = f.editor.getModel();
    f.editor.setValue("edited first");
    f.editor.setPosition({ lineNumber: 8, column: 3 });
    f.props.documentKey = "b";
    f.props.modelValue = "second";
    f.props.language = "tptp";
    await vue.nextTick();
    const second = f.editor.getModel();
    assert.notEqual(second, first);
    assert.equal(second.language, "tptp");
    assert.equal(f.editor.getValue(), "second");
    f.props.documentKey = "a";
    f.props.modelValue = "edited first";
    f.props.language = "kif";
    await vue.nextTick();
    assert.equal(f.editor.getModel(), first);
    assert.equal(f.editor.getValue(), "edited first");
    assert.equal(f.editor.getPosition().lineNumber, 8);
    assert.equal(f.editor.getPosition().column, 3);
    assert.equal(second.disposed, false);
  } finally {
    f.scope.stop();
  }
});

test("closed documents release their models and unmount releases the rest", async () => {
  const f = await fixture();
  try {
    const first = f.editor.getModel();
    f.props.documentKey = "b";
    f.props.modelValue = "second";
    await vue.nextTick();
    const second = f.editor.getModel();
    f.api.closeDocument("a");
    assert.equal(first.disposed, true);
    f.api.closeDocument("b");
    assert.equal(second.disposed, false);
    for (const hook of f.unmount) hook();
    assert.equal(second.disposed, true);
  } finally {
    f.scope.stop();
  }
});
