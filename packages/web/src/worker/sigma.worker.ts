// Web Worker host for the sigmakee wasm engine. Keeps the synchronous prover
// (ingest / promote / ask / audit / validate) off the UI thread. Owns only the
// Session; the page (src/main.ts and friends) owns the constituent list, OPFS, localStorage, and
// the editor, and drives this worker over a tiny id-keyed RPC.
//
// The RPC surface itself lives in handlers.ts, which the page imports
// type-only to type `call`.

import { handlers } from "./handlers";
import { installVampireBridge } from "./vampire-bridge";

/** Hand a result's byte payload over instead of copying it, when it owns its
 *  whole (non-shared) buffer. */
function transferables(result: unknown): Transferable[] {
  const b = (result as { bytes?: unknown })?.bytes;
  if (!(b instanceof Uint8Array)) return [];
  const buf = b.buffer;
  return buf instanceof ArrayBuffer &&
    b.byteOffset === 0 &&
    b.byteLength === buf.byteLength
    ? [buf]
    : [];
}

self.onmessage = async (e) => {
  const { id, cmd, args } = e.data;
  // The page's port to its Vampire worker (at boot and after a restart);
  // not an RPC, nothing to answer.
  if (cmd === "vampirePort") {
    installVampireBridge(args.port);
    return;
  }
  try {
    const fn = handlers[cmd as keyof typeof handlers];
    if (!fn) throw new Error(`unknown cmd: ${cmd}`);
    const result = await (fn as (a: unknown) => unknown)(args || {});
    self.postMessage({ id, result }, transferables(result));
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    self.postMessage({ id, error: msg });
  }
};
