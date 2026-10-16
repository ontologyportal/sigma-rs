import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";

test("branch replacement reads the selected commit instead of a saved edit", async () => {
  const requests = [];
  const exports = {};
  const source = ts.transpileModule(
    readFileSync(new URL("./sources.ts", import.meta.url), "utf8"),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    },
  ).outputText;
  vm.runInNewContext(source, {
    exports,
    require: () => ({
      useChangesStore: () => ({ readEdit: async () => "(local edit)" }),
    }),
    fetch: async (url) => {
      requests.push(url);
      return { ok: true, text: async () => "(branch content)" };
    },
  });
  const origin = {
    kind: "sumo",
    pathOf: () => "Merge.kif",
    rawUrl: (path, ref) =>
      `https://raw.githubusercontent.com/alice/sumo/${ref}/${path}`,
  };
  const entries = [{ name: "alice/sumo/Merge.kif", origin }];
  const normal = await exports.fetchAllTexts(
    entries,
    6,
    () => {},
    () => "sha",
  );
  assert.equal(normal[0], "(local edit)");
  assert.equal(requests.length, 0);
  const fresh = await exports.fetchAllTexts(
    entries,
    6,
    () => {},
    () => "sha",
    true,
  );
  assert.equal(fresh[0], "(branch content)");
  assert.deepEqual(requests, [
    "https://raw.githubusercontent.com/alice/sumo/sha/Merge.kif",
  ]);
});
