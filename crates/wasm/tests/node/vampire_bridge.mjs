// Node harness for the wasm Vampire runner: the `Session` binding drives the
// external prover layer through `globalThis.__sigmaRunVampireSync`, which this
// script stubs with canned transcripts (no Vampire binary involved).
//
//   cargo build -p sumo-parser-wasm --target wasm32-unknown-unknown --release
//   wasm-bindgen --target nodejs --out-dir /tmp/sumo-node \
//     target/wasm32-unknown-unknown/release/sumo_parser_wasm.wasm
//   node crates/wasm/tests/node/vampire_bridge.mjs /tmp/sumo-node
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { resolve } from "node:path";

const pkgDir = resolve(process.argv[2] ?? "/tmp/sumo-node");
const require = createRequire(import.meta.url);
const { Session, Config } = require(resolve(pkgDir, "sumo_parser_wasm.js"));

const THEOREM = `% SZS status Theorem for input
% SZS output start Proof for input
fof(f1, axiom, p, file('/dev/stdin', kb_1)).
fof(f2, conjecture, p, file('/dev/stdin', query_0)).
fof(f3, negated_conjecture, ~p, inference(negated_conjecture, [], [f2])).
fof(f4, plain, $false, inference(resolution, [], [f1, f3])).
% SZS output end Proof for input
`;

const KIF = "(subclass Dog Mammal)\n(subclass Mammal Animal)\n(instance Rex Dog)\n";

function session(backend, extra = {}) {
  const s = new Session();
  const errs = s.loadKif(KIF, "test.kif");
  assert.deepEqual(errs, [], "load errors");
  const cfg = new Config();
  cfg.backend = backend;
  cfg.timeLimitSecs = 7;
  for (const [k, v] of Object.entries(extra)) cfg[k] = v;
  s.configure(cfg);
  return s;
}

// 1. No bridge installed: a clear Unknown, never a throw.
{
  delete globalThis.__sigmaRunVampireSync;
  const r = session("vampire").ask("(instance Rex Animal)");
  assert.equal(r.status, "Unknown");
  assert.match(r.raw_output, /__sigmaRunVampireSync is not installed/);
}

// 2. A canned Theorem transcript comes back as a Proved result with the
//    proof lowered, and the runner was handed the translated problem with
//    the fixed CLI args, the time limit, and the extra args.
{
  const calls = [];
  globalThis.__sigmaRunVampireSync = (tptp, args, timeoutMs) => {
    calls.push({ tptp, args, timeoutMs });
    return { stdout: THEOREM, stderr: "", code: 0 };
  };
  const s = session("vampire", { vampireArgs: "--foo bar", keepTptp: true });
  const r = s.ask("(instance Rex Animal)");
  assert.equal(r.status, "Proved", r.raw_output);
  assert.equal(r.proved, true);
  assert.equal(r.proof.length, 4);
  assert.equal(r.proof[3].rule, "resolution");
  assert.ok(calls.length >= 1, "bridge never called");
  const c = calls[0];
  assert.match(c.args, /--mode vampire/);
  assert.match(c.args, /--sine_selection off/);
  // The autoscaling loop slices the 7 s budget per attempt, as the CLI does;
  // the bridge deadline is that slice plus a fixed grace.
  const slice = Number(/-t (\d+)\b/.exec(c.args)?.[1]);
  assert.ok(slice >= 1 && slice <= 7, c.args);
  assert.match(c.args, /--foo bar$/);
  assert.equal(c.timeoutMs, slice * 1000 + 5000);
  assert.match(c.tptp, /conjecture/);
  assert.match(c.tptp, /s__Rex/);
  assert.equal(r.input_tptp, calls[calls.length - 1].tptp);
}

// 3. Without keepTptp the result carries no problem text.
{
  globalThis.__sigmaRunVampireSync = () => ({ stdout: THEOREM, stderr: "" });
  const r = session("vampire").ask("(instance Rex Animal)");
  assert.equal(r.status, "Proved");
  assert.equal(r.input_tptp, undefined);
}

// 4. A bridge that throws reports the message.
{
  globalThis.__sigmaRunVampireSync = () => {
    throw new Error("worker exploded");
  };
  const r = session("vampire").ask("(instance Rex Animal)");
  assert.equal(r.status, "Unknown");
  assert.match(r.raw_output, /worker exploded/);
}

// 5. A CounterSatisfiable transcript is Disproved; the saturation verdict
//    reaches the caller through the same autoscaling loop as the CLI.
{
  globalThis.__sigmaRunVampireSync = () => ({
    stdout: "% SZS status CounterSatisfiable for input\n% Termination reason: Satisfiable\n",
    stderr: "",
  });
  const r = session("vampire").ask("(instance Rex Plant)");
  assert.equal(r.status, "Disproved", r.raw_output);
}

// 6. Audit on the Vampire backend runs a consistency check (no conjecture).
{
  const calls = [];
  globalThis.__sigmaRunVampireSync = (tptp, args) => {
    calls.push({ tptp, args });
    return { stdout: "% SZS status Satisfiable for input\n", stderr: "" };
  };
  const r = session("vampire").auditConsistency(3);
  assert.equal(r.status, "Consistent", r.raw_output);
  assert.equal(r.inconsistent, false);
  assert.equal(calls.length, 1);
  assert.doesNotMatch(calls[0].tptp, /conjecture/);
}

// 7. The native backend on the same stack still proves, without the bridge.
{
  delete globalThis.__sigmaRunVampireSync;
  const s = session("native");
  const r = s.ask("(instance Rex Animal)");
  assert.equal(r.status, "Proved", r.raw_output);
  const a = s.auditConsistency(2);
  assert.equal(a.status, "Consistent", a.raw_output);
  assert.ok(s.clausify().length > 0);
}

// 8. Session support (tell) reaches both engines; a restore keeps the runner.
{
  globalThis.__sigmaRunVampireSync = () => ({ stdout: THEOREM, stderr: "" });
  const s = session("vampire");
  assert.equal(s.tell("(instance Fido Dog)", "s1").ok, true);
  assert.equal(s.ask("(instance Fido Animal)", "s1").status, "Proved");
  const bytes = s.snapshot();
  s.restore(bytes);
  assert.equal(s.ask("(instance Rex Animal)").status, "Proved");
  const cfg = new Config();
  cfg.backend = "native";
  s.configure(cfg);
  assert.equal(s.ask("(instance Rex Animal)").status, "Proved");
}

// 9. An unknown backend name is rejected at configuration time.
{
  const cfg = new Config();
  assert.throws(() => {
    cfg.backend = "eprover";
  }, /unknown backend/);
}

console.log("vampire_bridge: ok");
