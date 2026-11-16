import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import { parse, compileScript } from "@vue/compiler-sfc";
import * as vue from "vue";
import ts from "typescript";

const transpile = (text) =>
  ts.transpileModule(text, {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  }).outputText;
const { descriptor } = parse(
  readFileSync(new URL("./TranslateTab.vue", import.meta.url), "utf8"),
);
const source = transpile(
  compileScript(descriptor, { id: "translate-test" }).content,
);

function fixture(reply) {
  const calls = [];
  const params = [];
  const query = vue.ref({});
  let handler = () => {};
  const imports = {
    vue,
    "../services/sigma": {
      call: async (cmd, args) => {
        calls.push({ cmd, args });
        return reply(args);
      },
    },
    "../router": {
      updateParams: async (q) => {
        params.push(q);
        query.value = q;
        handler(q);
      },
    },
    "../composables/useTabQuery": {
      useTabQuery: () => ({
        query,
        onQuery: (h) => (handler = h),
        str: (v) => (v == null ? "" : String(v)),
      }),
    },
  };
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    require: (name) => imports[name] ?? {},
    Error,
  });
  const scope = vue.effectScope();
  const view = scope.run(() => exports.default.setup({}, { expose() {} }));
  const settle = () => new Promise((r) => setTimeout(r, 0));
  return {
    view,
    calls,
    params,
    scope,
    settle,
    navigate: (q) => {
      query.value = q;
      handler(q);
    },
  };
}

// Objects built inside the vm sandbox carry its realm's prototypes.
const plain = (v) => JSON.parse(JSON.stringify(v));

const translated = (sentences) => ({
  translation: { available: true, error: null, sentences },
});

test("translates the input and mirrors it to the URL once", async () => {
  const f = fixture(() =>
    translated([
      {
        text: "Rex is a dog.",
        offset: 0,
        kind: "assertion",
        kif: "(instance Rex Dog)",
        reason: null,
      },
    ]),
  );
  try {
    f.view.text.value = "  Rex is a dog.  ";
    await f.view.translate();
    await f.settle();
    assert.deepEqual(plain(f.params), [{ q: "Rex is a dog." }]);
    assert.deepEqual(plain(f.calls), [
      { cmd: "translateCelt", args: { text: "Rex is a dog." } },
    ]);
    assert.equal(
      f.view.translation.value.sentences[0].kif,
      "(instance Rex Dog)",
    );
    assert.equal(f.view.error.value, "");
    assert.equal(f.view.busy.value, false);
  } finally {
    f.scope.stop();
  }
});

test("re-translating unchanged text runs again without touching the URL", async () => {
  const f = fixture(() => translated([]));
  try {
    f.navigate({ q: "Is Rex a dog?" });
    await f.settle();
    await f.view.translate();
    assert.equal(f.calls.length, 2);
    assert.deepEqual(plain(f.params), []);
  } finally {
    f.scope.stop();
  }
});

test("refused sentences keep their reason", async () => {
  const f = fixture(() =>
    translated([
      {
        text: "Florp the zib.",
        offset: 0,
        kind: null,
        kif: null,
        reason: "unknown word 'zib'",
      },
    ]),
  );
  try {
    f.navigate({ q: "Florp the zib." });
    await f.settle();
    const [s] = f.view.translation.value.sentences;
    assert.equal(s.kif, null);
    assert.equal(s.reason, "unknown word 'zib'");
  } finally {
    f.scope.stop();
  }
});

test("a missing lexicon surfaces as an error", async () => {
  const f = fixture(() => ({
    translation: { available: false, error: "no lexicon", sentences: [] },
  }));
  try {
    f.navigate({ q: "Rex is a dog." });
    await f.settle();
    assert.equal(f.view.error.value, "no lexicon");
    assert.equal(f.view.translation.value, null);
  } finally {
    f.scope.stop();
  }
});

test("worker failures surface as an error", async () => {
  const f = fixture(() => {
    throw new Error("worker crashed");
  });
  try {
    f.navigate({ q: "Rex is a dog." });
    await f.settle();
    assert.equal(f.view.error.value, "worker crashed");
    assert.equal(f.view.busy.value, false);
  } finally {
    f.scope.stop();
  }
});

test("blank input neither translates nor updates the URL, and clearing q resets", async () => {
  const f = fixture(() => translated([]));
  try {
    f.view.text.value = "   ";
    await f.view.translate();
    assert.equal(f.calls.length, 0);
    assert.deepEqual(plain(f.params), []);

    f.navigate({ q: "Rex is a dog." });
    await f.settle();
    assert.notEqual(f.view.translation.value, null);
    f.navigate({});
    assert.equal(f.view.text.value, "");
    assert.equal(f.view.translation.value, null);
  } finally {
    f.scope.stop();
  }
});
