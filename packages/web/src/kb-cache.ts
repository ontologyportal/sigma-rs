/**
 * KB snapshot cache (OPFS).
 *
 * Boot's expensive work is re-fetching every 'sumo'-origin file over HTTP and
 * re-running ingest+promote+validate from scratch. The core KB has its own
 * freeze/thaw seam (session.snapshot()/restore()) built exactly for this; this
 * cache pairs a frozen snapshot with a cached copy of each 'sumo' file's text
 * (needed for the Edit tab / file-size display, which read `constituents[i]
 * .text` regardless of how the KB itself got built) so a matching boot can
 * skip BOTH the network fetch and the ingest+promote — not just one of them.
 *
 * Validity: the cached upstream commit SHA must match the CURRENT SHA
 * (`sumo`-origin content is pinned to a commit, not versioned per-file), and
 * a fingerprint of the exact (name, origin) set loaded — plus each tracked
 * local edit's content — must match (a different file set, or a different
 * edit on top of it, obviously needs a different snapshot). `file`-origin
 * text has no commit to pin to, so its cache freshness instead relies on
 * every mutation path (ingest/update/remove/reset) funnelling through
 * promoteAndValidate, which rewrites the cache after every successful
 * promote — there is no separate "invalidate" step, just "always keep the
 * cache as fresh as the last successful promote".
 *
 * `url`-origin constituents have no version signal at all (an arbitrary
 * link's content can change with nothing to detect it against), so their
 * presence disables caching for that boot entirely rather than risk serving
 * stale content silently.
 */

import { state } from './state.ts';
import { call } from './rpc.ts';
import { $ } from './dom.ts';
import { fromOrigin } from './sources.ts';
import { opfsSafeName, editsFingerprint } from './changes.ts';
import { bootProgress, resetBootProgress } from './boot.ts';
import { fetchLastCommitInfo } from './tabs/home-stats.ts';

const SUMO_CACHE_DIR      = 'sumo-cache';
const SUMO_CACHE_META     = 'meta.json';
const SUMO_CACHE_SNAPSHOT = 'snapshot.bin';

// A snapshot costs hundreds of ms of WORKER time (blocking every query behind
// it) plus a multi-MB write, and only ever pays off on the next boot — so a
// burst of mutations coalesces into one write instead of one write each.
const KB_CACHE_SAVE_DELAY_MS = 5000;

let sumoCacheDirHandle = null;   // lazily opened, separate from the top-level
                                 // OPFS dir 'file'-origin uploads already use

// name -> the exact text last written to the cache this session.
const cachedText = new Map();

async function getSumoCacheDir() {
  if (sumoCacheDirHandle) return sumoCacheDirHandle;
  if (!state.opfsRoot) throw new Error('File system not initialized yet');
  sumoCacheDirHandle = await state.opfsRoot.getDirectoryHandle(SUMO_CACHE_DIR, { create: true });
  return sumoCacheDirHandle;
}

async function writeOpfsFile(dir, name, contents) {
  const handle = await dir.getFileHandle(name, { create: true });
  const w = await handle.createWritable();
  await w.write(contents);
  await w.close();
}

/** Stable fingerprint of the current constituent SET (name+origin pairs) —
 *  changes on add/remove, independent of any file's content — plus the tracked
 *  local edits, so a revert (which puts a `sumo` file back to upstream's text,
 *  leaving the commit SHA and the file set both unchanged) still invalidates
 *  the snapshot it no longer matches. */
function constituentsFingerprint() {
  const files = state.savedConstituents.map((c) => `${c.origin}:${c.name}`).sort().join('|');
  return `${files}#${editsFingerprint()}`;
}

/** `false` when any loaded constituent has no stable version signal to cache
 *  against (`url` origin) — caching is skipped entirely for that boot. */
function kbCacheEligible() {
  return state.savedConstituents.length > 0
    && state.savedConstituents.every((c) => c.origin === 'sumo' || c.origin === 'file');
}

/**
 * Attempt a cache-hit boot: restore the KB from a cached snapshot and
 * populate `constituents` from cached ('sumo') / OPFS ('file') text,
 * skipping the fetch+ingest+promote loop entirely.
 *
 * Returns `true` on success — the caller still renders/routes, just skips
 * straight past the fetch loop and `reprocess()`. Returns `false` for any
 * reason at all (no cache yet, a stale one, a corrupt read, an unsupported
 * browser) and touches no state the normal boot path wouldn't also set, so
 * the caller can unconditionally fall through to it.
 */
export async function tryRestoreFromCache() {
  if (!kbCacheEligible()) return false;
  // Offline / rate-limited: trust whatever's cached rather than fail the
  // whole boot — the normal path needs this same network access anyway, so a
  // cache miss here doesn't cost anything a fresh boot wasn't already risking.
  let info;
  try { info = await fetchLastCommitInfo(); } catch { info = null; }
  try {
    const dir = await getSumoCacheDir();
    const meta = JSON.parse(await (await (await dir.getFileHandle(SUMO_CACHE_META)).getFile()).text());
    if (info && meta.commitSha !== info.sha) return false;
    if (meta.fingerprint !== constituentsFingerprint()) return false;

    // Confirmed hit — only now touch the shared boot-progress counters. Doing
    // this any earlier corrupts them for the normal fetch loop on a MISS
    // (the common case): the total got left at a tiny fixed number while the
    // step counter kept climbing once per file, so the bar rushed to 100%
    // almost immediately and then sat pinned there while the per-file status
    // label kept changing underneath it.
    resetBootProgress(2);   // short, fixed sequence — unlike the fetch loop, not sized per constituent
    bootProgress('Restoring from cache…');
    const bytes = new Uint8Array(await (await (await dir.getFileHandle(SUMO_CACHE_SNAPSHOT)).getFile()).arrayBuffer());
    await call('restore', { bytes }, [bytes.buffer]);

    const built = [];
    for (const { name, origin } of state.savedConstituents) {
      const text = origin === 'sumo'
        ? await (await (await dir.getFileHandle(opfsSafeName(name))).getFile()).text()
        : await fromOrigin(origin, name);
      if (origin === 'sumo') cachedText.set(name, text);
      built.push({ name, origin, text });
    }
    state.constituents = built;
    // The restored KB is already promoted — this is the read-only structural
    // pass reprocess() would otherwise run, not a rebuild, so it's cheap.
    state.diagnostics = (await call('validate')).diagnostics;
    bootProgress('Cache restored');
    return true;
  } catch {
    return false;   // no cache dir yet, a missing/corrupt entry, restore() rejected, …
  }
}

/**
 * Persist the current, just-promoted KB as the cache for next boot. Best
 * effort and fire-and-forget from the caller's perspective: any failure
 * (OPFS quota, an unsupported browser, offline) just means the next boot
 * does a normal fetch+ingest — never surfaced to the user.
 */
async function saveKbCache() {
  saveScheduled = false;
  if (!kbCacheEligible()) return;
  try {
    const info = await fetchLastCommitInfo();
    if (!info?.sha) return;
    const bytes = (await call('snapshot')).bytes;
    const dir = await getSumoCacheDir();
    for (const { name, origin, text } of state.constituents) {
      // Upstream text cannot change within a session and a local edit changes
      // it at most once per save, so the comparison writes each file about
      // once rather than on every promote.
      if (origin !== 'sumo' || cachedText.get(name) === text) continue;
      await writeOpfsFile(dir, opfsSafeName(name), text);
      cachedText.set(name, text);
    }
    await writeOpfsFile(dir, SUMO_CACHE_SNAPSHOT, bytes);
    await writeOpfsFile(dir, SUMO_CACHE_META, JSON.stringify({
      commitSha: info.sha, fingerprint: constituentsFingerprint(),
    }));
  } catch (e) {
    console.warn('KB snapshot cache: failed to save', e);
  }
}

let saveTimer = 0;
let saveScheduled = false;

/** Queue a cache write, coalescing a burst of mutations into one. */
export function scheduleKbCacheSave() {
  saveScheduled = true;
  clearTimeout(saveTimer);
  saveTimer = setTimeout(saveKbCache, KB_CACHE_SAVE_DELAY_MS);
}

// Leaving the page must not lose a pending write — the whole point of the
// cache is the boot after this one.
document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'hidden' && saveScheduled) {
    clearTimeout(saveTimer);
    saveKbCache();
  }
});

// Settings-modal maintenance action — a manual escape hatch for a stale/
// corrupt cache, not a feature to promote day-to-day. Only clears the
// persisted OPFS cache; the live in-memory KB is untouched, so a fresh boot
// (a manual reload) is what actually exercises the change.
$('clearCacheLink')?.addEventListener('click', async (e) => {
  e.preventDefault();
  const link = $('clearCacheLink');
  const original = link.textContent;
  try {
    if (state.opfsRoot) await state.opfsRoot.removeEntry(SUMO_CACHE_DIR, { recursive: true });
  } catch {
    // Nothing cached yet is not a failure — either way the cache is now clear.
  }
  sumoCacheDirHandle = null;   // the lazily-opened handle would otherwise point at a removed directory
  link.textContent = 'cache cleared';
  setTimeout(() => { link.textContent = original; }, 2000);
});
