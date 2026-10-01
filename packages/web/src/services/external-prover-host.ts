/**
 * The page's side of the external prover (WASM) worker: spawns it, hands the sigma
 * worker a MessagePort to it, and respawns it when the sigma worker's bridge
 * reports a run that would not finish (src/worker/external-prover-bridge.ts).
 *
 * The page owns the worker because a worker nested under the sigma worker
 * cannot make progress while that worker is parked on `Atomics.wait`.
 */

const workers: Partial<Record<"vampire" | "e", Worker>> = {};

/** Spawn (or respawn) an external prover worker; returns the port for the sigma
 *  worker's bridge, to be transferred to it. */
export function spawnVampireWorker(
  baseUrl: string,
  backend: "vampire" | "e" = "vampire",
): MessagePort {
  workers[backend]?.terminate();
  const worker = new Worker(
    new URL("../worker/external-prover.worker.ts", import.meta.url),
    {
      type: "module",
    },
  );
  workers[backend] = worker;
  const { port1, port2 } = new MessageChannel();
  worker.postMessage({ type: "port", port: port1, baseUrl }, [port1]);
  return port2;
}
