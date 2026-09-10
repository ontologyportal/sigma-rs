/** Resolving a constituent's text from wherever its `Origin` says it lives:
 *  the upstream repo, an arbitrary URL, or an OPFS-backed local upload. */

import { rawUrl } from "../constants";
import type { Origin } from "../models/Origin";
import { useBootStore } from "../stores/boot";
import { useChangesStore } from "../stores/changes";

export async function fetchText(url: string): Promise<string> {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${url}: HTTP ${r.status}`);
  return r.text();
}

export async function fromOrigin(
  name: string,
  origin: Origin,
): Promise<string> {
  switch (origin.kind) {
    case "sumo": {
      // An unpushed local edit outranks upstream. The edit store is separate
      // from the KB snapshot cache for exactly this reason: an upstream commit
      // discards that cache and sends every 'sumo' file back through here,
      // which would otherwise silently restore the pre-edit text.
      const edited = await useChangesStore().readEdit(name);
      return edited ?? (await fetchText(rawUrl(name)));
    }
    case "url":
      return fetchText(name);
    case "file": {
      const boot = useBootStore();
      if (!boot.opfsRoot) throw new Error("File system not initialized yet");
      const handle = await boot.opfsRoot.getFileHandle(name);
      const file = await handle.getFile();
      return file.text();
    }
  }
}

/** Fetch every `sumo`-origin file, up to `limit` at once, returning texts in
 *  list order. A per-file failure is captured rather than thrown so one bad
 *  file cannot abandon the rest -- sequential fetching would make Full SUMO
 *  a minutes-long wait. */
export async function fetchAllTexts(
  names: string[],
  limit: number,
  onDone: (done: number) => void,
): Promise<(string | Error)[]> {
  const out = new Array<string | Error>(names.length);
  let next = 0;
  let done = 0;
  const worker = async () => {
    for (let i = next++; i < names.length; i = next++) {
      try {
        out[i] = await fetchText(rawUrl(names[i]));
      } catch (e) {
        out[i] = e instanceof Error ? e : new Error(String(e));
      }
      onDone(++done);
    }
  };
  await Promise.all(
    Array.from({ length: Math.min(limit, names.length) }, worker),
  );
  return out;
}
