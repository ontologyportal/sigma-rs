// The synchronous bridge the wasm engine's Vampire runner calls
// (`globalThis.__sigmaRunVampireSync`, see crates/wasm/src/vampire.rs).
//
// Rust's `ProverRunner::prove` is a synchronous call inside the engine's
// autoscaling loop, while the Emscripten Vampire is an async JS API -- so the
// run happens in a page-owned worker (vampire.worker.ts, spawned by
// services/vampire-host.ts) reached over a MessagePort, and this worker parks
// on `Atomics.wait` until it signals. That needs SharedArrayBuffer, i.e.
// cross-origin isolation, which the pthreads-built vampire.wasm already
// requires (see public/_headers). Blocking this worker for the run matches
// the native backend, whose `ask` is a synchronous Rust loop on this same
// worker.

interface BridgeResult {
  stdout: string;
  stderr: string;
  code: number;
}

/** Transcript the engine classifies as a timeout (`vampire_proof::is_timeout`). */
function timeoutTranscript(timeoutMs: number): BridgeResult {
  return {
    stdout: "% SZS status Timeout for input\n% Time limit reached!\n",
    stderr:
      `bridge: Vampire did not finish within ${timeoutMs} ms; ` +
      "the Vampire worker was restarted",
    code: -1,
  };
}

let port: MessagePort | null = null;

/** Adopt the port to the page-owned Vampire worker (at boot, and again
 *  after the page restarts that worker). */
export function setVampirePort(p: MessagePort) {
  port = p;
}

/** Ask the page to terminate and respawn the Vampire worker; a fresh port
 *  arrives through {@link setVampirePort} before the next RPC is handled. */
function requestRestart() {
  port = null;
  self.postMessage({ type: "vampire-restart" });
}

function runSync(tptp: string, args: string, timeoutMs: number): BridgeResult {
  if (typeof SharedArrayBuffer === "undefined") {
    throw new Error(
      "the Vampire (WASM) backend needs cross-origin isolation " +
        "(COOP/COEP headers) for SharedArrayBuffer, which this deployment " +
        "does not send. Use the Native backend instead.",
    );
  }
  if (!port) {
    throw new Error(
      "the Vampire worker is not connected (restarting after a timeout?); " +
        "try again",
    );
  }
  const ctrl = new Int32Array(new SharedArrayBuffer(8));
  port.postMessage({ type: "run", tptp, args, ctrl: ctrl.buffer });

  const deadline = timeoutMs > 0 ? Date.now() + timeoutMs : Infinity;
  // Park until ctrl[0] reaches `state`; false on deadline.
  const waitFor = (state: number): boolean => {
    for (;;) {
      const cur = Atomics.load(ctrl, 0);
      if (cur === state) return true;
      const left = deadline - Date.now();
      if (left <= 0) return false;
      Atomics.wait(ctrl, 0, cur, Math.min(left, 2_000_000_000));
    }
  };

  if (!waitFor(1)) {
    requestRestart();
    return timeoutTranscript(timeoutMs);
  }
  const len = Atomics.load(ctrl, 1);
  const data = new SharedArrayBuffer(len);
  port.postMessage({ type: "buffer", data });
  if (!waitFor(2)) {
    requestRestart();
    return timeoutTranscript(timeoutMs);
  }
  // TextDecoder rejects views over shared memory: copy first.
  const bytes = new Uint8Array(data).slice();
  const out = JSON.parse(new TextDecoder().decode(bytes));
  if (out.error) {
    // A failed instantiation leaves the worker in an unknown state.
    requestRestart();
    throw new Error(out.error);
  }
  return { stdout: out.stdout, stderr: out.stderr, code: out.code };
}

export function installVampireBridge(p: MessagePort) {
  setVampirePort(p);
  (globalThis as unknown as Record<string, unknown>).__sigmaRunVampireSync =
    runSync;
}
