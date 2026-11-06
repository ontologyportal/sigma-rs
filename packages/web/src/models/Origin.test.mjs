import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";

const exports = {};
vm.runInNewContext(
  ts.transpileModule(
    readFileSync(new URL("./Origin.ts", import.meta.url), "utf8"),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    },
  ).outputText,
  {
    exports,
    URL,
    require: () => ({
      SUMO: { owner: "ontologyportal", repo: "sumo", branch: "master" },
    }),
  },
);

test("GitLab origin survives persistence with distinct source identity and subgroup paths", () => {
  const g = new exports.GitOrigin(
    "gitlab",
    "group/subgroup",
    "kb",
    "work/ontology",
  );
  const name = g.nameFor("dir/a.kif");
  assert.equal(g.pathOf(name), "dir/a.kif");
  assert.equal(exports.originId(g), "gitlab:group/subgroup/kb@work/ontology");
  const saved = exports.serializeOrigin(g);
  const restored = exports.parseOrigin(saved);
  assert.equal(restored.service, "gitlab");
  assert.equal(restored.id, g.id);
  assert.equal(restored.pathOf(name), "dir/a.kif");
  assert.equal(exports.originId(saved), g.id);
});

test("legacy GitHub origins retain their identity and GitLab never becomes default SUMO", () => {
  const legacy = {
    kind: "sumo",
    owner: "ontologyportal",
    repo: "sumo",
    branch: "master",
  };
  assert.equal(exports.parseOrigin(legacy).service, "github");
  assert.equal(exports.parseOrigin(legacy).isDefault, true);
  const gitlab = new exports.GitOrigin(
    "gitlab",
    legacy.owner,
    legacy.repo,
    legacy.branch,
  );
  assert.equal(gitlab.isDefault, false);
  assert.notEqual(gitlab.nameFor("a.kif"), "a.kif");
  assert.notEqual(exports.originId(gitlab), exports.originId(legacy));
});

test("repositories on different GitLab servers retain distinct names and identities", () => {
  const nps = new exports.GitOrigin(
    "gitlab",
    "fsg",
    "kb",
    "main",
    "https://gitlab.nps.edu/fsg/",
  );
  const other = new exports.GitOrigin(
    "gitlab",
    "fsg",
    "kb",
    "main",
    "https://gitlab.example.org",
  );
  const saved = exports.serializeOrigin(nps);
  const restored = exports.parseOrigin(saved);
  assert.equal(restored.server, "https://gitlab.nps.edu");
  assert.equal(exports.originId(saved), nps.id);
  assert.equal(restored.id, nps.id);
  assert.equal(restored.pathOf(nps.nameFor("a.kif")), "a.kif");
  assert.notEqual(nps.id, other.id);
  assert.notEqual(nps.nameFor("a.kif"), other.nameFor("a.kif"));
  assert.match(nps.rawUrl("a.kif"), /^https:\/\/gitlab.nps.edu\//);
});
