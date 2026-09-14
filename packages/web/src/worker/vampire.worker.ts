// The dedicated Vampire (WASM) worker. Owns the Emscripten instance so the
// sigma worker -- which drives it through the synchronous bridge in
// vampire-bridge.ts -- can park on `Atomics.wait` while a run is in flight.
//
// The PAGE spawns this worker (services/vampire-host.ts) and hands the sigma
// worker one end of a MessageChannel: a worker nested under the parked sigma
// worker never gets to load or run, since its resource loading rides its
// parent's thread.
//
// Protocol on the port (one run at a time, both messages from the bridge):
//   { type: "run", tptp, args, ctrl }   ctrl: SharedArrayBuffer(8)
//     -> run Vampire, JSON-encode { stdout, stderr, code, error? }, publish
//        the byte length in ctrl[1], set ctrl[0] = 1, notify.
//   { type: "buffer", data }            data: SharedArrayBuffer(len)
//     -> copy the bytes in, set ctrl[0] = 2, notify.

let pending: Uint8Array | null = null;
let ctrl: Int32Array | null = null;
let baseUrl = self.location.href;

interface RunMessage {
  type: "run";
  tptp: string;
  args: string;
  ctrl: SharedArrayBuffer;
}
interface BufferMessage {
  type: "buffer";
  data: SharedArrayBuffer;
}

async function run(m: RunMessage) {
  ctrl = new Int32Array(m.ctrl);
  let out: { stdout: string; stderr: string; code: number; error?: string };
  try {
    // `@vite-ignore`: the runner is a static passthrough asset (mirrored from
    // @sigma/vampire) that is legitimately absent from builds that skipped
    // the Emscripten step; a bundler-resolved import would fail the build.
    const url = new URL("vampire/vampire-runner.js", baseUrl).href;
    const { runVampire } = await import(/* @vite-ignore */ url);
    const r = await runVampire(m.tptp, m.args, {});
    out = { stdout: r.stdout ?? "", stderr: r.stderr ?? "", code: r.code ?? 0 };
  } catch (err) {
    const msg = (err as Error)?.message || String(err);
    out = {
      stdout: "",
      stderr: "",
      code: -1,
      error: /vampire-runner|Failed to fetch|import/.test(msg)
        ? "The Vampire (WASM) backend is not included in this deployment " +
          "(@sigma/vampire output missing). Use the Native backend instead."
        : msg,
    };
  }
  pending = new TextEncoder().encode(JSON.stringify(out));
  Atomics.store(ctrl, 1, pending.byteLength);
  Atomics.store(ctrl, 0, 1);
  Atomics.notify(ctrl, 0);
}

function deliver(m: BufferMessage) {
  if (pending && ctrl) {
    new Uint8Array(m.data).set(pending);
    pending = null;
    Atomics.store(ctrl, 0, 2);
    Atomics.notify(ctrl, 0);
  }
}

// The page's handshake: the port the sigma worker's bridge will talk on, and
// the site base the runner asset resolves against.
self.onmessage = (
  e: MessageEvent<{ type: "port"; port: MessagePort; baseUrl: string }>,
) => {
  if (e.data?.type !== "port") return;
  baseUrl = e.data.baseUrl || baseUrl;
  e.data.port.onmessage = (ev: MessageEvent<RunMessage | BufferMessage>) => {
    const m = ev.data;
    if (m.type === "run") void run(m);
    else if (m.type === "buffer") deliver(m);
  };
};
