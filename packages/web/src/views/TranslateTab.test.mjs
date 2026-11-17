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

function fixture(reply, { stored = null, files = [] } = {}) {
  const storage = new Map(stored === null ? [] : [["celt", stored]]);
  const calls = [];
  const params = [];
  const navigations = [];
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
      navigate: (tab, q) => navigations.push({ tab, ...q }),
      updateParams: async (q) => {
        params.push(q);
        query.value = q;
        handler(q);
      },
    },
    "../constants": { CELT_OPTIONS_KEY: "celt" },
    "../stores/wordnet": { useWordNetStore: () => ({ files }) },
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
    JSON,
    localStorage: {
      getItem: (k) => storage.get(k) ?? null,
      setItem: (k, v) => storage.set(k, v),
    },
  });
  const scope = vue.effectScope();
  const view = scope.run(() => exports.default.setup({}, { expose() {} }));
  const settle = () => new Promise((r) => setTimeout(r, 0));
  return {
    view,
    calls,
    params,
    navigations,
    storage,
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
    assert.equal(f.calls.length, 1);
    assert.equal(f.calls[0].cmd, "translateCelt");
    assert.equal(f.calls[0].args.text, "Rex is a dog.");
    assert.equal(f.calls[0].args.options.useNames, true);
    assert.equal(f.calls[0].args.options.speaker, "");
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

test("marked splits a refused sentence around its span", () => {
  const f = fixture(() => translated([]));
  try {
    const m = f.view.marked({
      text: "Florp the zib.",
      span: { start: 10, end: 13 },
    });
    assert.deepEqual(plain(m), {
      before: "Florp the ",
      mark: "zib",
      after: ".",
    });
  } finally {
    f.scope.stop();
  }
});

test("marked counts code points, not UTF-16 units", () => {
  const f = fixture(() => translated([]));
  try {
    const m = f.view.marked({
      text: "\u{1F415} barks zib.",
      span: { start: 8, end: 11 },
    });
    assert.equal(m.mark, "zib");
    assert.equal(m.before, "\u{1F415} barks ");
  } finally {
    f.scope.stop();
  }
});

test("marked tolerates a missing or out-of-range span", () => {
  const f = fixture(() => translated([]));
  try {
    assert.deepEqual(plain(f.view.marked({ text: "Hi.", span: null })), {
      before: "Hi.",
      mark: "",
      after: "",
    });
    assert.deepEqual(
      plain(f.view.marked({ text: "Hi.", span: { start: 2, end: 99 } })),
      { before: "Hi", mark: ".", after: "" },
    );
    assert.deepEqual(
      plain(f.view.marked({ text: "Hi.", span: { start: 5, end: 1 } })),
      { before: "Hi.", mark: "", after: "" },
    );
  } finally {
    f.scope.stop();
  }
});

test("terms open on the Browse tab", () => {
  const f = fixture(() => translated([]));
  try {
    f.view.openTerm("DomesticDog");
    assert.deepEqual(plain(f.navigations), [
      { tab: "browse", sym: "DomesticDog" },
    ]);
  } finally {
    f.scope.stop();
  }
});

test("word senses open their synset on the WordNet tab", () => {
  const f = fixture(() => translated([]));
  try {
    f.view.openSynset("02084071-n");
    assert.deepEqual(plain(f.navigations), [
      { tab: "wordnet", q: "02084071-n", s: "02084071-n" },
    ]);
  } finally {
    f.scope.stop();
  }
});

test("options are sent with the translation and persisted", async () => {
  const f = fixture(() => translated([]));
  try {
    f.view.options.speaker = "Rex";
    f.view.options.parrot = true;
    await vue.nextTick();
    assert.deepEqual(JSON.parse(f.storage.get("celt")), {
      speaker: "Rex",
      addressee: "",
      genericYou: "",
      now: "",
      assumeNames: false,
      generic: false,
      useNames: true,
      lines: false,
      parrot: true,
    });
    f.navigate({ q: "I am a dog." });
    await f.settle();
    const sent = plain(f.calls.at(-1).args.options);
    assert.equal(sent.speaker, "Rex");
    assert.equal(sent.parrot, true);
    assert.deepEqual(plain(f.view.activeOptions.value), ["speaker", "parrot"]);
  } finally {
    f.scope.stop();
  }
});

test("saved options load, ignoring unknown keys and mistyped values", () => {
  const f = fixture(() => translated([]), {
    stored: JSON.stringify({
      speaker: "Rex",
      generic: "yes",
      bogus: 1,
      lines: true,
    }),
  });
  try {
    assert.equal(f.view.options.speaker, "Rex");
    assert.equal(f.view.options.generic, false);
    assert.equal(f.view.options.lines, true);
    assert.equal("bogus" in f.view.options, false);
  } finally {
    f.scope.stop();
  }
});

test("corrupt saved options fall back to the defaults", () => {
  const f = fixture(() => translated([]), { stored: "{not json" });
  try {
    assert.equal(f.view.options.useNames, true);
    assert.equal(f.view.activeOptions.value.length, 0);
  } finally {
    f.scope.stop();
  }
});

test("changing an option re-translates the current input", async () => {
  const f = fixture(() => translated([]));
  try {
    f.navigate({ q: "I am a dog." });
    await f.settle();
    assert.equal(f.calls.length, 1);
    f.view.options.speaker = "Rex";
    await vue.nextTick();
    await f.settle();
    assert.equal(f.calls.length, 2);
    assert.equal(f.calls[1].args.options.speaker, "Rex");
  } finally {
    f.scope.stop();
  }
});

test("changing an option with no translation does not translate", async () => {
  const f = fixture(() => translated([]));
  try {
    f.view.options.generic = true;
    await vue.nextTick();
    await f.settle();
    assert.equal(f.calls.length, 0);
  } finally {
    f.scope.stop();
  }
});

test("reset restores every default", async () => {
  const f = fixture(() => translated([]));
  try {
    f.view.options.addressee = "Mary";
    f.view.options.lines = true;
    f.view.resetOptions();
    assert.equal(f.view.activeOptions.value.length, 0);
  } finally {
    f.scope.stop();
  }
});

test("first names report unavailable only once WordNet loaded without them", () => {
  const before = fixture(() => translated([]));
  const without = fixture(() => translated([]), {
    files: [{ name: "WordNetMappings30-noun.txt", size: 1 }],
  });
  const withNames = fixture(() => translated([]), {
    files: [
      { name: "WordNetMappings30-noun.txt", size: 1 },
      { name: "FirstNames.csv", size: 1 },
    ],
  });
  try {
    assert.equal(before.view.namesAvailable.value, true);
    assert.equal(without.view.namesAvailable.value, false);
    assert.equal(withNames.view.namesAvailable.value, true);
  } finally {
    before.scope.stop();
    without.scope.stop();
    withNames.scope.stop();
  }
});

test("an option changed mid-translation still re-translates, and only the latest reply shows", async () => {
  const pending = [];
  const f = fixture(
    (args) =>
      new Promise((resolve) =>
        pending.push(() =>
          resolve(
            translated([
              {
                text: "I am a dog.",
                offset: 0,
                kind: "assertion",
                kif: `(speaker ${args.options.speaker || "none"})`,
                reason: null,
                parrot: args.options.parrot ? "parroted" : null,
              },
            ]),
          ),
        ),
      ),
  );
  try {
    f.navigate({ q: "I am a dog." });
    f.view.options.speaker = "Rex";
    await vue.nextTick();
    f.view.options.parrot = true;
    await vue.nextTick();
    assert.equal(f.calls.length, 3);
    assert.equal(f.view.busy.value, true);

    // Out of order: the latest reply lands first, stale ones after it.
    pending[2]();
    await f.settle();
    pending[0]();
    pending[1]();
    await f.settle();
    const [s] = f.view.translation.value.sentences;
    assert.equal(s.kif, "(speaker Rex)");
    assert.equal(s.parrot, "parroted");
    assert.equal(f.view.busy.value, false);
  } finally {
    f.scope.stop();
  }
});
