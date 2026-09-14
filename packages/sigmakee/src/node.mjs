/**
 * Node-side Vampire bridge for the `sigmakee/sdk` facade.
 *
 * The wasm engine's external prover layer drives Vampire through one global,
 * `globalThis.__sigmaRunVampireSync(tptp, args, timeoutMs)`, which must run
 * the prover synchronously and return `{ stdout, stderr, code }`. In Node that
 * is a `spawnSync` of the `vampire` binary; this module builds that bridge
 * (the browser package installs a worker-backed one instead).
 *
 *     import { init, Session, Backend } from "sigmakee/sdk";
 *     import { installVampireBridge, vampireBridge } from "sigmakee/node";
 *     installVampireBridge(vampireBridge({ vampirePath: "/usr/local/bin/vampire" }));
 *     const s = new Session({ backend: Backend.Vampire });
 */
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

/**
 * A bridge that runs the `vampire` binary at `vampirePath` on each problem.
 * `args` is the engine's command line (it already carries `-t`); the
 * problem is written to a temporary file and appended as the input path.
 * @param {{ vampirePath?: string }} [opts]
 * @returns {(tptp: string, args: string, timeoutMs: number) => { stdout: string, stderr: string, code: number }}
 */
export function vampireBridge({ vampirePath = "vampire" } = {}) {
  return (tptp, args, timeoutMs) => {
    const dir = mkdtempSync(join(tmpdir(), "sigmakee-vampire-"));
    const input = join(dir, "input.p");
    try {
      writeFileSync(input, tptp);
      const r = spawnSync(
        vampirePath,
        [...String(args).split(/\s+/).filter(Boolean), input],
        {
          encoding: "utf8",
          timeout: timeoutMs > 0 ? timeoutMs : undefined,
          maxBuffer: 256 * 1024 * 1024,
        },
      );
      if (r.error) throw r.error;
      return {
        stdout: r.stdout ?? "",
        stderr: r.stderr ?? "",
        code: r.status ?? -1,
      };
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  };
}

/** Install `bridge` as the global the engine's Vampire runner calls. */
export function installVampireBridge(bridge) {
  globalThis.__sigmaRunVampireSync = bridge;
}
