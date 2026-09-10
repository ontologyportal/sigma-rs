/**
 * KB snapshot cache (OPFS).
 *
 * Boot's expensive work is re-fetching every 'sumo'-origin file over HTTP and
 * re-running ingest+promote+validate from scratch. The core KB has its own
 * freeze/thaw seam (session.snapshot()/restore()) built exactly for this; this
 * cache pairs a frozen snapshot with a cached copy of each 'sumo' file's text
 * (needed for the Edit tab / file-size display, which read `constituents[i]
 * .text` regardless of how the KB itself got built) so a matching boot can
 * skip BOTH the network fetch and the ingest+promote -- not just one of them.
 *
 * Validity: the cached upstream commit SHA must match the CURRENT SHA
 * (`sumo`-origin content is pinned to a commit, not versioned per-file), and
 * a fingerprint of the exact (name, origin) set loaded -- plus each tracked
 * local edit's content -- must match (a different file set, or a different
 * edit on top of it, obviously needs a different snapshot). `file`-origin
 * text has no commit to pin to, so its cache freshness instead relies on
 * every mutation path (ingest/update/remove/reset) funnelling through
 * promoteAndValidate, which rewrites the cache after every successful
 * promote -- there is no separate "invalidate" step, just "always keep the
 * cache as fresh as the last successful promote".
 *
 * `url`-origin constituents have no version signal at all (an arbitrary
 * link's content can change with nothing to detect it against), so their
 * presence disables caching for that boot entirely rather than risk serving
 * stale content silently.
 */

import { call } from "./sigma";
import { fromOrigin } from "./sources";
import { fetchLastCommitInfo } from "../api/github";
import { Constituent } from "../models/Constituent";
import { GitOrigin, originId, parseOrigin } from "../models/Origin";
import { useBootStore } from "../stores/boot";
import { useKBStore } from "../stores/kb";
import { useChangesStore, opfsSafeName } from "../stores/changes";
import { useWordNetStore } from "../stores/wordnet";

const SUMO_CACHE_DIR = "sumo-cache";
const SUMO_CACHE_META = "meta.json";
const SUMO_CACHE_SNAPSHOT = "snapshot.bin";

// A snapshot costs hundreds of ms of WORKER time (blocking every query behind
// it) plus a multi-MB write, and only ever pays off on the next boot -- so a
// burst of mutations coalesces into one write instead of one write each.
const KB_CACHE_SAVE_DELAY_MS = 5000;

// Lazily opened, separate from the top-level OPFS dir 'file'-origin uploads
// already use.
let sumoCacheDirHandle: FileSystemDirectoryHandle | null = null;

// name -> the exact text last written to the cache this session.
const cachedText = new Map<string, string>();

async function getSumoCacheDir(): Promise<FileSystemDirectoryHandle> {
  if (sumoCacheDirHandle) return sumoCacheDirHandle;
  const boot = useBootStore();
  if (!boot.opfsRoot) throw new Error("File system not initialized yet");
  sumoCacheDirHandle = await boot.opfsRoot.getDirectoryHandle(SUMO_CACHE_DIR, {
    create: true,
  });
  return sumoCacheDirHandle;
}

async function writeOpfsFile(
  dir: FileSystemDirectoryHandle,
  name: string,
  contents: string | Uint8Array<ArrayBuffer>,
) {
  const handle = await dir.getFileHandle(name, { create: true });
  const w = await handle.createWritable();
  await w.write(contents);
  await w.close();
}

async function readOpfsText(
  dir: FileSystemDirectoryHandle,
  name: string,
): Promise<string> {
  return (await (await dir.getFileHandle(name)).getFile()).text();
}

/** Stable fingerprint of the current constituent SET (name+origin pairs) --
 *  changes on add/remove, independent of any file's content -- plus the
 *  tracked local edits, so a revert (which puts a `sumo` file back to
 *  upstream's text, leaving the commit SHA and the file set both unchanged)
 *  still invalidates the snapshot it no longer matches. */
function constituentsFingerprint(): string {
  const files = useKBStore()
    .saved.map((c) => `${originId(c.origin)}:${c.name}`)
    .sort()
    .join("|");
  return `${files}#${useChangesStore().fingerprint}`;
}

/** `false` when any loaded constituent has no stable version signal to cache
 *  against (`url` origin, or a git repo other than upstream -- only
 *  upstream's commit SHA is checked) -- caching is skipped entirely for that
 *  boot. */
function kbCacheEligible(): boolean {
  const { saved } = useKBStore();
  const upstream = originId(GitOrigin.default());
  return (
    saved.length > 0 &&
    saved.every(
      (c) => c.origin.kind === "file" || originId(c.origin) === upstream,
    )
  );
}

/**
 * Attempt a cache-hit boot: restore the KB from a cached snapshot and
 * populate `kb.constituents` from cached ('sumo') / OPFS ('file') text,
 * skipping the fetch+ingest+promote loop entirely.
 *
 * Returns `true` on success -- the caller skips straight past the fetch loop
 * and `reprocess()`. Returns `false` for any reason at all (no cache yet, a
 * stale one, a corrupt read, an unsupported browser) and touches no state
 * the normal boot path wouldn't also set, so the caller can unconditionally
 * fall through to it. Progress is reported through `onProgress` only once
 * the hit is confirmed, so a miss (the common case) leaves the boot store's
 * counters untouched for the fetch loop.
 */
export async function tryRestore(
  onProgress: (msg: string) => void,
): Promise<boolean> {
  if (!kbCacheEligible()) return false;
  const kb = useKBStore();
  // Offline / rate-limited: trust whatever's cached rather than fail the
  // whole boot -- the normal path needs this same network access anyway, so a
  // cache miss here doesn't cost anything a fresh boot wasn't already risking.
  let info: { sha: string | null } | null;
  try {
    info = await fetchLastCommitInfo();
  } catch {
    info = null;
  }
  try {
    const dir = await getSumoCacheDir();
    const meta = JSON.parse(await readOpfsText(dir, SUMO_CACHE_META));
    if (info && meta.commitSha !== info.sha) return false;
    if (meta.fingerprint !== constituentsFingerprint()) return false;

    onProgress("Restoring from cache...");
    const bytes = new Uint8Array(
      await (
        await (await dir.getFileHandle(SUMO_CACHE_SNAPSHOT)).getFile()
      ).arrayBuffer(),
    );
    await call("restore", { bytes }, [bytes.buffer]);
    // The snapshot covers the KB itself, not the lexicon (a separate sidecar,
    // never part of the snapshot), so it's fetched fresh here too.
    onProgress("Fetching WordNet lexicon...");
    await useWordNetStore().install();
    onProgress("Loading WordNet lexicon...");

    const built: Constituent[] = [];
    for (const { name, origin: json } of kb.saved) {
      const origin = parseOrigin(json, name);
      const text =
        origin.kind === "sumo"
          ? await readOpfsText(dir, opfsSafeName(name))
          : await fromOrigin(name, origin);
      if (origin.kind === "sumo") cachedText.set(name, text);
      built.push(new Constituent(name, origin, text));
    }
    kb.constituents = built;
    // The restored KB is already promoted -- this is the read-only structural
    // pass reprocess() would otherwise run, not a rebuild, so it's cheap.
    kb.diagnostics = (await call("validate")).diagnostics;
    onProgress("Cache restored");
    return true;
  } catch {
    return false; // no cache dir yet, a missing/corrupt entry, restore() rejected, ...
  }
}

let saveTimer: ReturnType<typeof setTimeout> | undefined;
let saveScheduled = false;

/**
 * Persist the current, just-promoted KB as the cache for next boot. Best
 * effort and fire-and-forget from the caller's perspective: any failure
 * (OPFS quota, an unsupported browser, offline) just means the next boot
 * does a normal fetch+ingest -- never surfaced to the user.
 */
async function save() {
  saveScheduled = false;
  if (!kbCacheEligible()) return;
  try {
    const info = await fetchLastCommitInfo();
    if (!info?.sha) return;
    const bytes = (await call("snapshot")).bytes;
    const dir = await getSumoCacheDir();
    for (const { name, origin, text } of useKBStore().constituents) {
      // Upstream text cannot change within a session and a local edit changes
      // it at most once per save, so the comparison writes each file about
      // once rather than on every promote.
      if (origin.kind !== "sumo" || cachedText.get(name) === text) continue;
      await writeOpfsFile(dir, opfsSafeName(name), text);
      cachedText.set(name, text);
    }
    await writeOpfsFile(dir, SUMO_CACHE_SNAPSHOT, bytes);
    await writeOpfsFile(
      dir,
      SUMO_CACHE_META,
      JSON.stringify({
        commitSha: info.sha,
        fingerprint: constituentsFingerprint(),
      }),
    );
  } catch (e) {
    console.warn("KB snapshot cache: failed to save", e);
  }
}

/** Queue a cache write, coalescing a burst of mutations into one. */
export function scheduleSave(): void {
  saveScheduled = true;
  clearTimeout(saveTimer);
  saveTimer = setTimeout(save, KB_CACHE_SAVE_DELAY_MS);
}

/** Leaving the page must not lose a pending write -- the whole point of the
 *  cache is the boot after this one. Installs the visibilitychange flush and
 *  returns its remover; the app shell calls this once on mount. */
export function installFlushOnHide(): () => void {
  const onVisibilityChange = () => {
    if (document.visibilityState === "hidden" && saveScheduled) {
      clearTimeout(saveTimer);
      save();
    }
  };
  document.addEventListener("visibilitychange", onVisibilityChange);
  return () =>
    document.removeEventListener("visibilitychange", onVisibilityChange);
}

/** Settings-dialog maintenance action -- a manual escape hatch for a stale/
 *  corrupt cache. Only clears the persisted OPFS cache; the live in-memory
 *  KB is untouched, so a fresh boot (a manual reload) is what actually
 *  exercises the change. */
export async function clearCache(): Promise<void> {
  const boot = useBootStore();
  try {
    if (boot.opfsRoot)
      await boot.opfsRoot.removeEntry(SUMO_CACHE_DIR, { recursive: true });
  } catch {
    // Nothing cached yet is not a failure -- either way the cache is now clear.
  }
  // The lazily-opened handle would otherwise point at a removed directory.
  sumoCacheDirHandle = null;
  cachedText.clear();
}
