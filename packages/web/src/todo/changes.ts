/**
 * Tracked local changes: which loaded constituents differ from upstream, where
 * their unpushed text lives, and which ones are already proposed as a pull
 * request.
 *
 * Identity is the git blob SHA (`sha1("blob <len>\0<bytes>")`), computed here
 * from text the page already holds — recording the version an edit started
 * from costs no API call, and one recursive tree read (`fetchSumoTree`)
 * supplies the current SHA of every upstream path at once.
 *
 * Every state the UI shows is DERIVED from three SHAs (base / saved /
 * upstream) plus an optional pull-request stamp; no state name is ever
 * stored. That is what makes a merged pull request resolve itself: upstream's
 * blob comes to match the saved one, and the record is simply dropped, with
 * no dependence on the PR poll having run.
 *
 * `sumo`-origin text lives in its own OPFS directory, deliberately NOT in the
 * KB snapshot cache: that cache is discarded wholesale whenever upstream's
 * HEAD moves (see kb-cache.ts), which would otherwise take every unpushed
 * edit with it.
 */

import { EDITS_KEY, SUMO, rawUrl } from './constants.ts';
import { state } from './state.ts';
import { githubApi, fetchSumoTree } from './github-api.ts';
import { fromBase64 } from './github.ts';

const EDITS_DIR = 'edits';

/** Encode a constituent name into a single OPFS-safe path segment: some SUMO
 *  constituents live in a repo subdirectory (`development/Muscles.kif`), and
 *  `getFileHandle` accepts exactly one path component. */
export function opfsSafeName(name) {
  return encodeURIComponent(name);
}

// -- The record index ---------------------------------------------------------
//
// key -> {
//   name, origin, path,        // `path` is where it goes upstream (== name for 'sumo')
//   baseBlobSha,               // the upstream version this edit started from
//   savedBlobSha,              // what is actually stored right now
//   savedAt,
//   proposed: { number, url, branch, headOwner, blobSha } | null,
//   prClosed: { number, url, merged } | null,
// }

const key = (name, origin) => `${origin}:${name}`;

export interface ProposedInfo { number: number; url: string; branch: string; headOwner: string; blobSha: string; }
export interface PrClosedInfo { number: number; url: string; merged: boolean; }

interface ChangeRecord {
  name: string;
  origin: string;
  path: string;
  baseBlobSha: string | null;
  savedBlobSha: string;
  savedAt: number;
  proposed: ProposedInfo | null;
  prClosed: PrClosedInfo | null;
}

let index: Record<string, ChangeRecord> = (() => {
  try { return JSON.parse(localStorage.getItem(EDITS_KEY) || 'null') || {}; }
  catch { return {}; }
})();

const persist = () => localStorage.setItem(EDITS_KEY, JSON.stringify(index));

/** The tracking record for one constituent, or null if it has none. */
export const changeRecord = (name, origin) => index[key(name, origin)] || null;

/** Stable fingerprint of what is tracked and at which content — folded into
 *  the KB snapshot cache's own fingerprint so a revert cannot be shadowed by
 *  a snapshot taken before it. */
export function editsFingerprint() {
  return Object.values(index)
    .map((r) => `${r.origin}:${r.name}:${r.savedBlobSha || ''}`)
    .sort()
    .join('|');
}

// -- Blob SHAs ----------------------------------------------------------------

/** The git blob SHA of `text`, or null where SubtleCrypto is unavailable (an
 *  insecure context) — callers degrade to "no staleness detection", never to
 *  a wrong answer. */
export async function blobSha(text) {
  if (!globalThis.crypto?.subtle) return null;
  const body = new TextEncoder().encode(text);
  const header = new TextEncoder().encode(`blob ${body.length}\0`);
  const buf = new Uint8Array(header.length + body.length);
  buf.set(header);
  buf.set(body, header.length);
  const digest = await crypto.subtle.digest('SHA-1', buf);
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('');
}

let upstreamShas = new Map();

/** Upstream's current blob SHA for a repo path, or null when unknown (no tree
 *  read has succeeded yet). */
export const upstreamSha = (path) => upstreamShas.get(path) ?? null;

/**
 * Refresh every tracked path's upstream SHA from one tree read, then drop any
 * record whose saved content is what upstream already holds (a landed pull
 * request, or an edit someone else committed verbatim).
 *
 * Best effort: a failed or rate-limited read leaves the previous SHAs in
 * place and reports nothing, since "we could not check" must not read as
 * "nothing changed upstream" anywhere downstream — `upstreamSha` returning
 * null is what suppresses the stale flag.
 */
export async function refreshUpstreamShas({ force = false } = {}) {
  if (!Object.keys(index).length) return;   // nothing tracked, nothing to check
  let tree;
  try { tree = await fetchSumoTree({ force }); }
  catch { return; }
  upstreamShas = new Map(tree.filter((e) => e.type === 'blob').map((e) => [e.path, e.sha]));
  let dropped = false;
  for (const rec of Object.values(index)) {
    const up = upstreamSha(rec.path);
    if (up && up === rec.savedBlobSha) { await forget(rec); dropped = true; }
  }
  if (dropped) persist();
}

/**
 * Poll the open pull requests this page opened. Merged ones normally clear via
 * the blob comparison above; this exists for the other endings — closed
 * without merging (the change is still the user's to carry), and merged with
 * modifications (upstream differs from what was pushed, so the file goes back
 * to looking modified and needs the reason shown).
 */
export async function refreshProposals() {
  const numbers = new Set(Object.values(index).filter((r) => r.proposed).map((r) => r.proposed.number));
  let changed = false;
  for (const n of numbers) {
    let pr;
    try { pr = await githubApi(`/repos/${SUMO.owner}/${SUMO.repo}/pulls/${n}`); }
    catch { continue; }
    if (pr.state !== 'closed') continue;
    for (const rec of Object.values(index)) {
      if (rec.proposed?.number !== n) continue;
      rec.prClosed = { number: n, url: rec.proposed.url, merged: Boolean(pr.merged) };
      rec.proposed = null;
      changed = true;
    }
  }
  if (changed) persist();
}

// -- The unpushed text (OPFS) -------------------------------------------------

let editsDir = null;

async function dir() {
  if (editsDir) return editsDir;
  if (!state.opfsRoot) throw new Error('File system not initialized yet');
  editsDir = await state.opfsRoot.getDirectoryHandle(EDITS_DIR, { create: true });
  return editsDir;
}

/**
 * The unpushed text for a `sumo`-origin constituent, or null if it has none.
 * `fromOrigin` consults this before fetching, which is what keeps local work
 * alive across a boot that re-fetches everything from upstream.
 */
export async function readEdit(name) {
  if (!index[key(name, 'sumo')]) return null;   // untracked: never touch OPFS
  try {
    const handle = await (await dir()).getFileHandle(opfsSafeName(name));
    return await (await handle.getFile()).text();
  } catch {
    return null;
  }
}

async function writeEdit(name, text) {
  const handle = await (await dir()).getFileHandle(opfsSafeName(name), { create: true });
  const w = await handle.createWritable();
  await w.write(text);
  await w.close();
}

/** Drop a record and any text it owns. Does not persist — callers batch that. */
async function forget(rec) {
  if (rec.origin === 'sumo') {
    try { await (await dir()).removeEntry(opfsSafeName(rec.name)); }
    catch { /* nothing written yet, or already gone */ }
  }
  delete index[key(rec.name, rec.origin)];
}

/** Stop tracking a constituent entirely (it was removed from the KB). */
export async function forgetChange(name, origin) {
  const rec = index[key(name, origin)];
  if (!rec) return;
  await forget(rec);
  persist();
}

// -- Recording a save ---------------------------------------------------------

/**
 * Record that `text` was saved as `name`/`origin`, storing the text itself for
 * `sumo` origin (local uploads already live in OPFS under their own name).
 *
 * `pristine` is the content the buffer started from, used ONCE to stamp the
 * upstream version this edit descends from; later saves keep the original
 * stamp, so a file edited five times is still compared against what upstream
 * held when the user first touched it.
 *
 * Saving content that matches the recorded base, or that matches what upstream
 * holds now, stops the tracking instead of extending it — which makes "revert
 * to upstream" and "take upstream's newer copy" the same operation as an
 * ordinary save, with no separate code path to keep consistent.
 *
 * @returns {Promise<object|null>} the record, or null if nothing is tracked.
 */
export async function recordSave(name, origin, text, pristine) {
  const k = key(name, origin);
  const rec = index[k];
  // Local uploads are only tracked once they have been proposed upstream —
  // before that there is nothing to compare them against.
  if (origin !== 'sumo' && !rec) return null;

  const path = rec?.path || name;
  const sha = await blobSha(text);
  const base = rec?.baseBlobSha ?? (pristine === undefined ? null : await blobSha(pristine));
  const up = upstreamSha(path);
  const backToUpstream = sha && ((up && sha === up) || (base && sha === base && !rec?.proposed));
  if (backToUpstream) {
    if (rec) { await forget(rec); persist(); }
    return null;
  }

  if (origin === 'sumo') await writeEdit(name, text);
  index[k] = {
    name, origin, path,
    baseBlobSha: base,
    savedBlobSha: sha,
    savedAt: Date.now(),
    proposed: rec?.proposed || null,
    prClosed: rec?.prClosed || null,
  };
  persist();
  return index[k];
}

/** Stamp every file just pushed with the pull request now carrying it. */
export function markProposed(entries, pr) {
  for (const e of entries) {
    const k = key(e.name, e.origin);
    const rec: ChangeRecord = index[k] || {
      name: e.name, origin: e.origin, path: e.path,
      baseBlobSha: null, savedBlobSha: e.blobSha, savedAt: Date.now(),
      proposed: null, prClosed: null,
    };
    rec.path = e.path;
    rec.savedBlobSha = e.blobSha;
    rec.prClosed = null;
    rec.proposed = {
      number: pr.number, url: pr.url, branch: pr.branch,
      headOwner: pr.headOwner, blobSha: e.blobSha,
    };
    index[k] = rec;
  }
  persist();
}

// -- Derived view -------------------------------------------------------------

/**
 * Upstream's current content for a tracked path. Read by blob SHA where one is
 * known, so the text is exactly the version the staleness check compared
 * against rather than whatever the raw CDN is serving this second.
 */
export async function fetchUpstreamText(path) {
  const sha = upstreamSha(path);
  if (sha) {
    const blob = await githubApi(`/repos/${SUMO.owner}/${SUMO.repo}/git/blobs/${sha}`);
    if (blob?.encoding === 'base64' && blob.content) return fromBase64(blob.content);
  }
  const r = await fetch(rawUrl(path));
  if (!r.ok) throw new Error(`${path}: HTTP ${r.status}`);
  return r.text();
}

/**
 * One row per file the user has something unpushed in, each carrying a derived
 * state:
 *   modified — differs from upstream, not proposed
 *   amended  — proposed, then edited again (the PR is behind the local copy)
 *   review   — proposed, unchanged since
 *   local    — a local upload or new file, never proposed
 * plus an orthogonal `stale` flag: upstream moved under an edit that started
 * from an older version.
 */
export interface ChangeRow {
  name: string;
  origin: string;
  path: string;
  baseBlobSha?: string | null;
  savedBlobSha?: string;
  savedAt?: number;
  proposed: ProposedInfo | null;
  prClosed?: PrClosedInfo | null;
  upstream: string | null;
  stale: boolean;
  state: string;
}

export function changeRows(): ChangeRow[] {
  const loaded = new Set(state.constituents.map((c) => key(c.name, c.origin)));
  const rows: ChangeRow[] = [];
  for (const rec of Object.values(index)) {
    if (!loaded.has(key(rec.name, rec.origin))) continue;   // no longer in the KB
    const up = upstreamSha(rec.path);
    rows.push({
      ...rec,
      upstream: up,
      stale: Boolean(rec.baseBlobSha && up && up !== rec.baseBlobSha),
      state: rec.proposed
        ? (rec.savedBlobSha === rec.proposed.blobSha ? 'review' : 'amended')
        : (rec.origin === 'sumo' ? 'modified' : 'local'),
    });
  }
  for (const c of state.constituents) {
    if (c.origin !== 'file' || index[key(c.name, c.origin)]) continue;
    rows.push({
      name: c.name, origin: 'file', path: c.name, state: 'local',
      stale: false, proposed: null, prClosed: null, upstream: null,
    });
  }
  return rows.sort((a, b) => a.origin.localeCompare(b.origin) || a.name.localeCompare(b.name));
}

/** A row the user still has to do something about — what the badge counts.
 *  Files in review are deliberately excluded: counting them would leave the
 *  badge permanently lit after any successful contribution. */
export const isActionable = (row) => row.state === 'modified' || row.state === 'amended';

export const changeCount = () => changeRows().filter(isActionable).length;
