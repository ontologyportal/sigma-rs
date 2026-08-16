/** Where a constituent's text comes from: the upstream repo, an arbitrary URL,
 *  or an OPFS-backed local upload. */

import { rawUrl } from './constants.js';
import { state } from './state.js';

export async function fetchText(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${url}: HTTP ${r.status}`);
  return r.text();
}

export async function fromOrigin(origin, file) {
  if (origin === 'sumo') return await fetchText(rawUrl(file));
  if (origin === 'url') return await fetchText(file);
  if (origin === 'file') {
    if (state.opfsRoot === null) throw new Error('File system not initialized yet');
    const handle = await state.opfsRoot.getFileHandle(file);
    const vFile = await handle.getFile();
    return await vFile.text();
  }
}

/** Fetch every file, up to `limit` at once, returning texts in list order.
 *  A per-file failure is captured rather than thrown so one bad file cannot
 *  abandon the other forty-eight. Sequential fetching would make Full SUMO
 *  a minutes-long wait. */
export async function fetchAllTexts(files, limit, onDone) {
  const out = new Array(files.length);
  let next = 0, done = 0;
  const worker = async () => {
    for (let i = next++; i < files.length; i = next++) {
      try { out[i] = await fetchText(rawUrl(files[i])); }
      catch (e) { out[i] = e instanceof Error ? e : new Error(String(e)); }
      onDone(++done);
    }
  };
  await Promise.all(Array.from({ length: Math.min(limit, files.length) }, worker));
  return out;
}
