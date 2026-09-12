/**
 * The page's side of the Vampire (WASM) worker: spawns it, hands the sigma
 * worker a MessagePort to it, and respawns it when the sigma worker's bridge
 * reports a run that would not finish (src/worker/vampire-bridge.ts).
 *
 * The page owns the worker because a worker nested under the sigma worker
 * cannot make progress while that worker is parked on `Atomics.wait`.
 */

let worker: Worker | null = null;

/** Spawn (or respawn) the Vampire worker; returns the port for the sigma
 *  worker's bridge, to be transferred to it. */
export function spawnVampireWorker(baseUrl: string): MessagePort {
  worker?.terminate();
  worker = new Worker(new URL("../worker/vampire.worker.ts", import.meta.url), {
    type: "module",
  });
  const { port1, port2 } = new MessageChannel();
  worker.postMessage({ type: "port", port: port1, baseUrl }, [port1]);
  return port2;
}
