/**
 * The worker and the tiny id-keyed RPC over postMessage that drives it.
 *
 * The worker owns the Session; the page owns the constituent list, OPFS,
 * localStorage, and the editor.
 */

const worker = new Worker(new URL('./sigma.worker.js', import.meta.url), { type: 'module' });

let seq = 0;
const pending = new Map();

worker.onmessage = (e) => {
  const { id, result, error } = e.data;
  const p = pending.get(id);
  if (!p) return;
  pending.delete(id);
  error ? p.reject(new Error(error)) : p.resolve(result);
};

export const call = (cmd, args, transfer = []) => new Promise((resolve, reject) => {
  const id = ++seq; pending.set(id, { resolve, reject });
  worker.postMessage({ id, cmd, args }, transfer);
});

worker.onerror = (e) => {
  const m = e.message || `${e.filename || ''}:${e.lineno || ''}`;
  const ov = document.getElementById('overlayErr'); if (ov) ov.textContent = 'worker: ' + m;
  console.error('worker error', e);
};
