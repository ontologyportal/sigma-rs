/**
 * "Back up data" -- everything this browser holds for the app, as one zip.
 *
 * Two stores together make up that state, and either alone is useless:
 * OPFS holds the file CONTENT (`edits/` unpushed edits, `library/` uploads,
 * `sumo-cache/` the boot snapshot), while localStorage holds the BOOKKEEPING
 * that gives it meaning -- the tracked-edit index, the saved-constituent
 * manifest, update preferences and baselines. An `edits/` file whose record
 * is missing is never read back (`readEdit` returns null for an untracked
 * name), so the archive carries localStorage.json beside the files.
 */

import { zip, type Zippable } from "fflate";
import { useBootStore } from "../stores/boot";
import { downloadBlob } from "../utils/format";

interface Entry {
  path: string;
  bytes: Uint8Array;
}

/** Every file under `dir`, depth-first, with `/`-joined paths. */
async function walk(
  dir: FileSystemDirectoryHandle,
  prefix: string,
  out: Entry[],
): Promise<void> {
  for await (const [name, handle] of dir.entries()) {
    const path = prefix ? `${prefix}/${name}` : name;
    if (handle.kind === "directory") {
      await walk(handle as FileSystemDirectoryHandle, path, out);
      continue;
    }
    try {
      const file = await (handle as FileSystemFileHandle).getFile();
      out.push({ path, bytes: new Uint8Array(await file.arrayBuffer()) });
    } catch {
      // A handle that vanished mid-walk is not worth failing a backup over.
    }
  }
}

/** Everything in localStorage, as the archive's `localStorage.json`. */
function localStorageEntry(): Entry {
  const all: Record<string, string> = {};
  for (let i = 0; i < localStorage.length; i++) {
    const k = localStorage.key(i);
    if (k !== null) all[k] = localStorage.getItem(k) ?? "";
  }
  return {
    path: "localStorage.json",
    bytes: new TextEncoder().encode(JSON.stringify(all, null, 2)),
  };
}

/** fflate's callback API as a promise. The async form deflates off the main
 *  thread, which a multi-megabyte snapshot needs if the button is to keep
 *  rendering its pending state. */
function archive(files: Zippable): Promise<Uint8Array> {
  return new Promise((resolve, reject) =>
    zip(files, { level: 6 }, (err, data) =>
      err ? reject(err) : resolve(data),
    ),
  );
}

/**
 * Download every file in OPFS plus localStorage as a single zip. Resolves
 * with the number of archived entries; throws when OPFS is unavailable, so
 * the caller can say so rather than hand over an empty archive.
 */
export async function downloadBackup(): Promise<number> {
  const root =
    useBootStore().opfsRoot ?? (await navigator.storage.getDirectory());
  const entries: Entry[] = [];
  await walk(root, "", entries);
  entries.push(localStorageEntry());

  // A flat map keyed by path: fflate reads `/` as nesting, so the walk's
  // paths reproduce the OPFS tree without building nested objects.
  const files: Zippable = {};
  for (const e of entries) files[e.path] = e.bytes;

  const stamp = new Date().toISOString().slice(0, 10);
  downloadBlob(
    `sumo-backup-${stamp}.zip`,
    new Blob([await archive(files)] as BlobPart[], {
      type: "application/zip",
    }),
  );
  return entries.length;
}
