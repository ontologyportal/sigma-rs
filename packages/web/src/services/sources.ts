/** Resolving a constituent's text from wherever its `Origin` says it lives:
 *  a git repo, an arbitrary URL, or the OPFS-backed local library. */

import type { GitOrigin, Origin, RemoteOrigin } from "../models/Origin";
import { useChangesStore } from "../stores/changes";
import { useLibraryStore } from "../stores/library";

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
      if (edited !== null) return edited;
      const git = origin as GitOrigin;
      return fetchText(git.rawUrl(git.pathOf(name)));
    }
    case "url":
      return fetchText((origin as RemoteOrigin).url || name);
    case "file":
      return useLibraryStore().readLocal(name);
  }
}

/** Fetch every file's text, up to `limit` at once, returning texts in list
 *  order. A per-file failure is captured rather than thrown so one bad file
 *  cannot abandon the rest -- sequential fetching would make Full SUMO a
 *  minutes-long wait. */
export async function fetchAllTexts(
  files: { name: string; origin: Origin }[],
  limit: number,
  onDone: (done: number) => void,
): Promise<(string | Error)[]> {
  const out = new Array<string | Error>(files.length);
  let next = 0;
  let done = 0;
  const worker = async () => {
    for (let i = next++; i < files.length; i = next++) {
      try {
        out[i] = await fromOrigin(files[i].name, files[i].origin);
      } catch (e) {
        out[i] = e instanceof Error ? e : new Error(String(e));
      }
      onDone(++done);
    }
  };
  await Promise.all(
    Array.from({ length: Math.min(limit, files.length) }, worker),
  );
  return out;
}
