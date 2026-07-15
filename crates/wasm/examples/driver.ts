/**
 * TypeScript driver using the SDK-shaped facade (`@ontologyportal/sumo-wasm/sdk`).
 *
 * This surface mirrors the `sigmakee-rs-sdk` crate — `Session`, `Source`,
 * `Backend`, `Config` — for the browser. For the lower-level bindings
 * (`WasmNativeProver` etc.) see `node-demo.mjs`.
 *
 * Import from the published subpath:
 *     import { init, Session, Source } from "@ontologyportal/sumo-wasm/sdk";
 * or, against a local build:
 *     import { init, Session, Source } from "../pkg/sdk.mjs";
 */
import {
  init, Session, Source, Backend, Config, type AskResult,
} from "@ontologyportal/sumo-wasm/sdk";

export async function main(): Promise<void> {
  await init(); // browser: fetches the .wasm next to the JS

  // --- 1. Native session: prove in-browser ----------------------------------
  const cfg = new Config();
  cfg.timeLimitSecs = 10;
  cfg.wantProof = true;

  const session = new Session({ backend: Backend.Native, config: cfg });

  // Load axioms from any Source. `ingest` is async (URL/GitHub sources fetch).
  await session.ingest(Source.kif(`
    (instance Socrates Man)
    (=> (instance ?X Man) (instance ?X Mortal))
  `, "socrates"));

  // Other sources — same call shape:
  //   await session.ingest(Source.url("https://example.org/ontology.kif"));
  //   await session.ingest(Source.file(fileInput.files[0]));
  //   await session.ingest(Source.gitHub({ owner: "ontologyportal", repo: "sumo", dir: "tests" }));

  const result = session.ask("(instance Socrates Mortal)") as AskResult;
  console.log("status:", result.status);          // "Proved"
  for (const step of result.proof) {
    console.log(`  [${step.index}] ${step.rule}: ${step.kif}`);
  }

  // Session-scoped hypotheses.
  session.tell("(instance Plato Man)", "s1");
  const scoped = session.ask("(instance Plato Mortal)", { session: "s1" }) as AskResult;
  console.log("scoped:", scoped.status);           // "Proved"
  session.flushSession("s1");

  // --- 2. Translation-only session: TPTP + external prover hook -------------
  const tx = new Session({ backend: Backend.TranslationOnly });
  await tx.ingest(Source.kif("(instance Socrates Man)", "demo"));
  console.log("TPTP:\n" + tx.translate({ lang: "fof" }));

  // WASM can't spawn a prover; supply one via a hook (e.g. POST to a server
  // running Vampire/E and return its stdout).
  const external = tx.ask("(instance Socrates Mortal)", {
    hook: (tptp: string) => {
      void tptp; // return await (await fetch("/prove", { method: "POST", body: tptp })).text();
      return "SZS status Unknown";
    },
  });
  console.log("external:", external);
}

main().catch((e) => {
  console.error(e);
  if (typeof process !== "undefined") process.exit(1);
});
