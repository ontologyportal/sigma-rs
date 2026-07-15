/**
 * Runnable Node demo for sumo-parser-wasm — proves a theorem in-process with
 * the native saturation prover, using a `web`-target build.
 *
 * Build the package first, then run:
 *     ./build-npm.sh            # produces crates/wasm/pkg/  (web target)
 *     node examples/node-demo.mjs
 *
 * The `web` target's default export is an `init(module_or_bytes)` function; in
 * Node we hand it the .wasm bytes directly (there is no fetch() of a URL).
 */
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const pkg = join(here, "..", "pkg");

const mod = await import(join(pkg, "sumo_parser_wasm.js"));
const init = mod.default;
const { WasmNativeProver, WasmKnowledgeBase, Config } = mod;

// Instantiate from the .wasm bytes (browser code would just `await init()`,
// letting it fetch the .wasm sitting next to the JS).
await init({ module_or_path: await readFile(join(pkg, "sumo_parser_wasm_bg.wasm")) });

// --- Prove in-browser-equivalent -------------------------------------------
const prover = new WasmNativeProver();

// Configure the prover (mirrors KBManager's NativeProverConfig).
const cfg = new Config();
cfg.timeLimitSecs = 10;
cfg.wantProof = true;
prover.configure(cfg);

prover.loadKif(
  `
  (instance Socrates Man)
  (=> (instance ?X Man) (instance ?X Mortal))
  `,
  "socrates",
);

const r = prover.ask("(instance Socrates Mortal)");
console.log("status      :", r.status);        // Proved
console.log("proved      :", r.proved);        // true
console.log("given_steps :", r.given_steps);
console.log("proof steps :");
for (const s of r.proof) console.log(`  [${s.index}] ${s.rule}: ${s.kif}`);

// A non-consequence must NOT come back Proved.
const neg = prover.ask("(instance Plato Mortal)");
console.log("\nnon-consequence status:", neg.status);   // Disproved / Unknown

// --- Translate to TPTP ------------------------------------------------------
const kb = new WasmKnowledgeBase();
kb.loadKif("(instance Socrates Man)", "demo");
console.log("\nTPTP:\n" + kb.toTptp("fof", true, undefined));

if (r.status !== "Proved") {
  console.error("\nFAIL: expected Proved");
  process.exit(1);
}
console.log("\nOK");
