/** Synchronous bridge shared by the browser's external provers. */
type Backend = "vampire" | "e";
const ports: Partial<Record<Backend, MessagePort>> = {};

/** Adopt a replacement port after a timed-out worker has been restarted. */
export function setVampirePort(
  port: MessagePort,
  backend: Backend = "vampire",
) {
  ports[backend] = port;
}

function runSync(
  backend: Backend,
  program: string,
  tptp: string,
  args: string,
  timeoutMs: number,
) {
  if (typeof SharedArrayBuffer === "undefined") {
    throw new Error(
      "External provers require cross-origin isolation (COOP/COEP headers)",
    );
  }
  const port = ports[backend];
  if (!port) throw new Error("Prover worker is restarting; try again");
  const restart = () => {
    delete ports[backend];
    self.postMessage({ type: "vampire-restart", backend });
  };
  const timeout = () => {
    restart();
    return {
      stdout: "% SZS status Timeout for input\n",
      stderr: "Worker deadline reached",
      code: -1,
    };
  };
  const ctrl = new Int32Array(new SharedArrayBuffer(8));
  port.postMessage({ type: "run", program, tptp, args, ctrl: ctrl.buffer });
  const deadline = timeoutMs > 0 ? Date.now() + timeoutMs : Infinity;
  const waitFor = (state: number) => {
    for (;;) {
      const current = Atomics.load(ctrl, 0);
      if (current === state) return true;
      const remaining = deadline - Date.now();
      if (remaining <= 0) return false;
      Atomics.wait(ctrl, 0, current, Math.min(remaining, 2_000_000_000));
    }
  };
  if (!waitFor(1)) return timeout();
  const length = Atomics.load(ctrl, 1);
  if (length < 0 || length > 256 * 1024 * 1024) {
    restart();
    throw new Error("Prover output exceeded the bridge limit");
  }
  const data = new SharedArrayBuffer(length);
  port.postMessage({ type: "buffer", data });
  if (!waitFor(2)) return timeout();
  const result = JSON.parse(
    new TextDecoder().decode(new Uint8Array(data).slice()),
  );
  if (result.error) {
    restart();
    throw new Error(result.error);
  }
  return result;
}

/** Install the backend's synchronous runner globals in the sigma worker. */
export function installVampireBridge(
  port: MessagePort,
  backend: Backend = "vampire",
) {
  setVampirePort(port, backend);
  const global = globalThis as unknown as Record<string, unknown>;
  if (backend === "e") {
    global.__sigmaRunEproverSync = (
      text: string,
      args: string,
      timeout: number,
    ) => runSync("e", "eprover", text, args, timeout);
    global.__sigmaRunAxfilterSync = (
      text: string,
      args: string,
      timeout: number,
    ) => runSync("e", "e_axfilter", text, args, timeout);
  } else {
    global.__sigmaRunVampireSync = (
      text: string,
      args: string,
      timeout: number,
    ) => runSync("vampire", "vampire", text, args, timeout);
  }
}
