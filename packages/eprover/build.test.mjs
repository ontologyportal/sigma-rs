import assert from "node:assert/strict";
import { mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";

/** Run a fresh native or WASM E instance against a TPTP problem. */
async function run(program, input, args) {
  if (process.env.EPROVER_NATIVE_DIR) {
    const dir = await mkdtemp(join(tmpdir(), "sigma-eprover-test-"));
    try {
      await writeFile(join(dir, "input.p"), input);
      const result = spawnSync(
        resolve(process.env.EPROVER_NATIVE_DIR, program),
        [...args, "input.p"],
        { cwd: dir, encoding: "utf8", timeout: 10000 },
      );
      if (result.error) throw result.error;
      assert.equal(result.signal, null);
      const files = await Promise.all(
        (await readdir(dir))
          .filter((name) => name.endsWith(".p") && name !== "input.p")
          .map((name) => readFile(join(dir, name), "utf8")),
      );
      return {
        code: result.status,
        output: result.stdout + result.stderr,
        files,
      };
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  }
  const { default: createModule } = await import(`./dist/${program}.js`);
  const wasmBinary = await readFile(
    new URL(`./dist/${program}.wasm`, import.meta.url),
  );
  const output = [];
  let code = 0;
  const module = await createModule({
    wasmBinary,
    noInitialRun: true,
    print: (line) => output.push(line),
    printErr: (line) => output.push(line),
    onExit: (status) => {
      code = status;
    },
  });
  module.FS.writeFile("/input.p", input);
  const previousExitCode = process.exitCode;
  try {
    module.callMain([...args, "/input.p"]);
  } finally {
    process.exitCode = previousExitCode;
  }
  const files = module.FS.readdir("/")
    .filter((name) => name.endsWith(".p") && name !== "input.p")
    .map((name) => module.FS.readFile(`/${name}`, { encoding: "utf8" }));
  return { code, output: output.join("\n"), files };
}

test("E proves a contradiction", async () => {
  const result = await run(
    "eprover",
    "fof(a,axiom,p(a)).\nfof(b,axiom,~p(a)).\n",
    ["--auto", "--tptp3-format", "--proof-object"],
  );
  assert.equal(result.code, 0, result.output);
  assert.match(result.output, /SZS status (Unsatisfiable|ContradictoryAxioms)/);
});

test("axiom filter generates symbol-seeded subsets", async () => {
  const result = await run(
    "e_axfilter",
    "fof(a,axiom,p(a)).\nfof(b,axiom,~p(a)).\nfof(c,axiom,q(b)).\n",
    ["--tptp3-format", "--seed-symbols=pfc", "--seed-method=a"],
  );
  assert.equal(result.code, 0, result.output);
  assert.ok(result.files.length > 0, result.output);
  assert.ok(result.files.some((text) => /p\(a\)/.test(text)));
});

test("axiom filter accepts empty input without inventing subsets", async () => {
  const result = await run("e_axfilter", "", [
    "--tptp3-format",
    "--seed-symbols=pfc",
  ]);
  assert.equal(result.code, 0, result.output);
  assert.equal(result.files.length, 0);
});

test("axiom filter rejects malformed TPTP", async () => {
  const result = await run("e_axfilter", "fof(broken,", ["--tptp3-format"]);
  assert.notEqual(result.code, 0, result.output);
});
