/**
 * The worker and the tiny id-keyed RPC over postMessage that drives it.
 *
 * The worker owns the Session; the page owns the constituent list, OPFS,
 * localStorage, and the editor.
 */

const worker = new Worker(new URL('./sigma.worker.ts', import.meta.url), { type: 'module' });

let seq = 0;
const pending = new Map<number, { resolve: (value: any) => void; reject: (reason?: unknown) => void }>();

worker.onmessage = (e) => {
  const { id, result, error } = e.data;
  const p = pending.get(id);
  if (!p) return;
  pending.delete(id);
  error ? p.reject(new Error(error)) : p.resolve(result);
};

// `T` defaults to `any` rather than modelling every command's response shape:
// the worker dispatches on `cmd` by string (see sigma.worker.ts), so a real
// mapping would need a cmd -> response type table. Callers that want checked
// results can opt in with `call<SomeType>(...)`.
export const call = <T = any>(cmd: string, args?: unknown, transfer: Transferable[] = []): Promise<T> =>
  new Promise<T>((resolve, reject) => {
    const id = ++seq; pending.set(id, { resolve, reject });
    worker.postMessage({ id, cmd, args }, transfer);
  });

worker.onerror = (e) => {
  const m = e.message || `${e.filename || ''}:${e.lineno || ''}`;
  const ov = document.getElementById('overlayErr'); if (ov) ov.textContent = 'worker: ' + m;
  console.error('worker error', e);
};
