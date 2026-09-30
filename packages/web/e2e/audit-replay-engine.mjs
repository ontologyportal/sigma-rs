// Real CLI/WASM smoke test; requires a built CLI and sigmakee package.
// Run from the repo root: node packages/web/e2e/audit-replay-engine.mjs
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import vm from "node:vm";
import ts from "typescript";
import { init, Session, Source, Config } from "sigmakee/sdk";

const root = fileURLToPath(new URL("../../../", import.meta.url));
const directory = mkdtempSync(join(tmpdir(), "sigma-replay-smoke-"));
const exports = {};
vm.runInNewContext(
  ts.transpileModule(
    readFileSync(
      new URL("../src/utils/auditReplay.ts", import.meta.url),
      "utf8",
    ),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    },
  ).outputText,
  { exports },
);
const text =
  "(instance ReplayEntity Human)\n(not (instance ReplayEntity Human))\n";
const configPath = join(directory, "config.xml");
const base = ["--config", configPath, "--ugly"];
function cli(args) {
  const result = spawnSync(
    join(root, "target/debug/sumo"),
    [...base, ...args],
    {
      cwd: directory,
      encoding: "utf8",
      timeout: 60000,
    },
  );
  assert.ifError(result.error);
  assert.ok([0, 1].includes(result.status), result.stderr);
  return result;
}
try {
  writeFileSync(join(directory, "Replay.kif"), text);
  writeFileSync(
    configPath,
    `<configuration><preference name="sumokbname" value="SUMO"/><preference name="editDir" value="${directory}"/><kb name="SUMO">\n</kb></configuration>`,
  );
  assert.equal(cli(["-f", "Replay.kif", "load"]).status, 0);
  const native = JSON.parse(
    cli([
      "audit",
      "--backend",
      "native",
      "--json",
      "--seed",
      "0",
      "--step",
      "0",
      "--count",
      "1",
      "--batch",
      "1",
      "--timeout",
      "10",
    ]).stdout,
  );
  assert.ok(
    native.contradictions.length > 0,
    "native fixture must produce a contradiction",
  );
  await init({
    module_or_path: readFileSync(
      join(root, "packages/sigmakee/dist/sumo_parser_wasm_bg.wasm"),
    ),
  });
  const config = new Config();
  config.maxSteps = 500000;
  config.maxLits = 12;
  config.timeLimitSecs = 10;
  config.forwardClose = true;
  config.wantProof = true;
  const session = new Session({ config });
  const loaded = await session.ingest(Source.kif(text, "Replay.kif"), {
    promote: false,
  });
  assert.deepEqual(loaded.errors, []);
  session.promote("Replay.kif");
  const browser = session.auditConsistency({
    seed: 0,
    step: 0,
    count: 1,
    batch: 1,
    limit: 64,
  });
  assert.equal(browser.next_step, native.next_step);
  assert.equal(browser.total, native.total);
  const expected = native.contradictions
    .map((c) => exports.replayAxiomKey(c.axioms))
    .sort();
  const actual = browser.contradictions
    .map((c) => exports.replayAxiomKey(c.steps.filter((s) => s.file != null)))
    .sort();
  assert.deepEqual(actual, expected);
  console.log(
    `Native and WASM reproduced the same ${actual.length} contradiction(s) at seed 0, step 0.`,
  );
} finally {
  rmSync(directory, { recursive: true, force: true });
}
