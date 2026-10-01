/**
 * The worker and the tiny id-keyed RPC over postMessage that drives it.
 *
 * The worker owns the Session; the page owns the constituent list, OPFS,
 * localStorage, and the editor.
 */

import { useBootStore } from "../stores/boot";
import { spawnVampireWorker } from "./vampire-host";
import type { Handlers } from "../worker/handlers";

/** A command the worker answers. */
export type Cmd = keyof Handlers;
/** `cmd`'s argument object. */
export type CmdArgs<C extends Cmd> = Parameters<Handlers[C]>[0];
/** What `call(cmd, ...)` resolves to. */
export type CmdResult<C extends Cmd> = Awaited<ReturnType<Handlers[C]>>;

// Commands whose args object may be omitted -- the handler takes no
// parameter, or an optional one. Tested by tuple assignability rather than
// `undefined extends CmdArgs<C>`: this program has `strictNullChecks` off,
// where that test is true for every type.
type NullaryCmd = {
  [C in Cmd]: [] extends Parameters<Handlers[C]> ? C : never;
}[Cmd];

function spawnWorker(): Worker {
  const w = new Worker(new URL("../worker/sigma.worker.ts", import.meta.url), {
    type: "module",
  });
  w.onmessage = onMessage;
  w.onerror = onError;
  return w;
}

/** A trap inside wasm -- a Rust panic or a failed allocation -- surfaces as a
 *  bare `unreachable`; the instance is unusable afterwards, and later calls
 *  fail with "recursive use of an object". */
export function isWasmAbort(message: string): boolean {
  return /^unreachable$|recursive use of an object/i.test(message.trim());
}

let seq = 0;
const pending = new Map<
  number,
  { resolve: (value: unknown) => void; reject: (reason?: unknown) => void }
>();

/** Site base the Vampire runner asset resolves against; set at boot. */
let vampireBaseUrl = location.href;

/** Spawn the Vampire worker and hand the sigma worker its port. */
export function connectVampire(baseUrl: string) {
  vampireBaseUrl = baseUrl;
  for (const backend of ["vampire", "e"] as const) {
    const port = spawnVampireWorker(baseUrl, backend);
    worker.postMessage({ cmd: "vampirePort", args: { port, backend } }, [port]);
  }
}

function onMessage(e: MessageEvent) {
  // The sigma worker's bridge gave up on a Vampire run: replace the worker.
  if (e.data?.type === "vampire-restart") {
    const backend = e.data.backend === "e" ? "e" : "vampire";
    const port = spawnVampireWorker(vampireBaseUrl, backend);
    worker.postMessage({ cmd: "vampirePort", args: { port, backend } }, [port]);
    return;
  }
  const { id, result, error } = e.data;
  const p = pending.get(id);
  if (!p) return;
  pending.delete(id);
  if (error) {
    if (isWasmAbort(error)) void useBootStore().recoverWorker();
    p.reject(new Error(error));
  } else p.resolve(result);
}

/** Replace the worker with a fresh one; the caller re-boots it and reloads
 *  the KB (see the boot store's `recoverWorker`). Calls still in flight on
 *  the old worker reject. */
export function replaceWorker() {
  worker.terminate();
  for (const p of pending.values()) p.reject(new Error("the engine restarted"));
  pending.clear();
  worker = spawnWorker();
}

/**
 * Call one of the worker's commands. Args and result are the handler's own
 * types: the cmd -> shape mapping is derived from `handlers` itself (see
 * worker/handlers.ts), so it cannot drift from what the worker answers.
 */
export function call<C extends NullaryCmd>(
  cmd: C,
  args?: CmdArgs<C>,
  transfer?: Transferable[],
): Promise<CmdResult<C>>;
export function call<C extends Cmd>(
  cmd: C,
  args: CmdArgs<C>,
  transfer?: Transferable[],
): Promise<CmdResult<C>>;
export function call<C extends Cmd>(
  cmd: C,
  args?: CmdArgs<C>,
  transfer: Transferable[] = [],
): Promise<CmdResult<C>> {
  // The one place the cmd -> result mapping is asserted rather than checked:
  // the reply arrives as JSON with only its `id` to identify it, so the
  // pending map cannot be keyed by command type.
  return new Promise<unknown>((resolve, reject) => {
    const id = ++seq;
    pending.set(id, { resolve, reject });
    worker.postMessage({ id, cmd, args }, transfer);
  }) as Promise<CmdResult<C>>;
}

// An uncaught worker error during boot is fatal for the page: surface it
// on the loading screen. Later ones are logged; the failing call itself
// rejects through the pending map.
function onError(e: ErrorEvent) {
  const m = e.message || `${e.filename || ""}:${e.lineno || ""}`;
  console.error("worker error", e);
  const boot = useBootStore();
  if (!boot.finished) {
    boot.failed = true;
    boot.error = "worker: " + m;
  }
}

let worker = spawnWorker();
