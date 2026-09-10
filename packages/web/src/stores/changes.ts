/**
 * Tracked local changes: which loaded constituents differ from upstream, where
 * their unpushed text lives, and which ones are already proposed as a pull
 * request.
 *
 * Identity is the git blob SHA (`sha1("blob <len>\0<bytes>")`), computed here
 * from text the page already holds -- recording the version an edit started
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
 * HEAD moves (see services/kb-cache.ts), which would otherwise take every
 * unpushed edit with it.
 */

import { defineStore } from "pinia";
import { EDITS_KEY, SUMO, rawUrl } from "../constants";
import type { OriginKind } from "../models/Origin";
import { githubApi, fetchSumoTree, fromBase64 } from "../api/github";
import { useBootStore } from "./boot";
import { useKBStore } from "./kb";

const EDITS_DIR = "edits";

/** Encode a constituent name into a single OPFS-safe path segment: some SUMO
 *  constituents live in a repo subdirectory (`development/Muscles.kif`), and
 *  `getFileHandle` accepts exactly one path component. */
export function opfsSafeName(name: string): string {
  return encodeURIComponent(name);
}

const key = (name: string, origin: OriginKind) => `${origin}:${name}`;

export interface ProposedInfo {
  number: number;
  url: string;
  branch: string;
  headOwner: string;
  blobSha: string;
}

export interface PrClosedInfo {
  number: number;
  url: string;
  merged: boolean;
}

/** One tracked constituent. `path` is where it goes upstream (== name for
 *  'sumo'); `baseBlobSha` is the upstream version this edit started from and
 *  `savedBlobSha` what is actually stored right now. */
export interface ChangeRecord {
  name: string;
  origin: OriginKind;
  path: string;
  baseBlobSha: string | null;
  savedBlobSha: string | null;
  savedAt: number;
  proposed: ProposedInfo | null;
  prClosed: PrClosedInfo | null;
}

/**
 * One row per file the user has something unpushed in, carrying a derived
 * state:
 *   modified - differs from upstream, not proposed
 *   amended  - proposed, then edited again (the PR is behind the local copy)
 *   review   - proposed, unchanged since
 *   local    - a local upload or new file, never proposed
 * plus an orthogonal `stale` flag: upstream moved under an edit that started
 * from an older version.
 */
export interface ChangeRow {
  name: string;
  origin: OriginKind;
  path: string;
  baseBlobSha?: string | null;
  savedBlobSha?: string | null;
  savedAt?: number;
  proposed: ProposedInfo | null;
  prClosed?: PrClosedInfo | null;
  upstream: string | null;
  stale: boolean;
  state: "modified" | "amended" | "review" | "local";
}

/** A row the user still has to do something about -- what the badge counts.
 *  Files in review are deliberately excluded: counting them would leave the
 *  badge permanently lit after any successful contribution. */
export const isActionable = (row: ChangeRow) =>
  row.state === "modified" || row.state === "amended";

/** The git blob SHA of `text`, or null where SubtleCrypto is unavailable (an
 *  insecure context) -- callers degrade to "no staleness detection", never to
 *  a wrong answer. */
export async function blobSha(text: string): Promise<string | null> {
  if (!globalThis.crypto?.subtle) return null;
  const body = new TextEncoder().encode(text);
  const header = new TextEncoder().encode(`blob ${body.length}\0`);
  const buf = new Uint8Array(header.length + body.length);
  buf.set(header);
  buf.set(body, header.length);
  const digest = await crypto.subtle.digest("SHA-1", buf);
  return [...new Uint8Array(digest)]
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

function loadIndex(): Record<string, ChangeRecord> {
  try {
    return JSON.parse(localStorage.getItem(EDITS_KEY) || "null") || {};
  } catch {
    return {};
  }
}

// Lazily opened, separate from the top-level OPFS dir 'file'-origin uploads use.
let editsDir: FileSystemDirectoryHandle | null = null;

async function dir(): Promise<FileSystemDirectoryHandle> {
  if (editsDir) return editsDir;
  const boot = useBootStore();
  if (!boot.opfsRoot) throw new Error("File system not initialized yet");
  editsDir = await boot.opfsRoot.getDirectoryHandle(EDITS_DIR, {
    create: true,
  });
  return editsDir;
}

async function writeEdit(name: string, text: string) {
  const handle = await (
    await dir()
  ).getFileHandle(opfsSafeName(name), { create: true });
  const w = await handle.createWritable();
  await w.write(text);
  await w.close();
}

export const useChangesStore = defineStore("changes", {
  state: () => ({
    /** The record index, mirrored to localStorage. */
    index: loadIndex(),
    /** Upstream's current blob SHA per repo path, from the last tree read. */
    upstreamShas: {} as Record<string, string>,
  }),
  getters: {
    /** The tracking record for one constituent, or null if it has none. */
    record:
      (state) =>
      (name: string, origin: OriginKind): ChangeRecord | null =>
        state.index[key(name, origin)] || null,

    /** Upstream's current blob SHA for a repo path, or null when unknown (no
     *  tree read has succeeded yet). */
    upstreamSha:
      (state) =>
      (path: string): string | null =>
        state.upstreamShas[path] ?? null,

    /** Stable fingerprint of what is tracked and at which content -- folded
     *  into the KB snapshot cache's own fingerprint so a revert cannot be
     *  shadowed by a snapshot taken before it. */
    fingerprint: (state) =>
      Object.values(state.index)
        .map((r) => `${r.origin}:${r.name}:${r.savedBlobSha || ""}`)
        .sort()
        .join("|"),

    /** Rows for every loaded constituent with something unpushed (see
     *  `ChangeRow`), sorted by origin then name. */
    rows(state): ChangeRow[] {
      const kb = useKBStore();
      const loaded = new Set(
        kb.constituents.map((c) => key(c.name, c.origin.kind)),
      );
      const rows: ChangeRow[] = [];
      for (const rec of Object.values(state.index)) {
        if (!loaded.has(key(rec.name, rec.origin))) continue; // no longer in the KB
        const up = state.upstreamShas[rec.path] ?? null;
        rows.push({
          ...rec,
          upstream: up,
          stale: Boolean(rec.baseBlobSha && up && up !== rec.baseBlobSha),
          state: rec.proposed
            ? rec.savedBlobSha === rec.proposed.blobSha
              ? "review"
              : "amended"
            : rec.origin === "sumo"
              ? "modified"
              : "local",
        });
      }
      for (const c of kb.constituents) {
        if (c.origin.kind !== "file" || state.index[key(c.name, c.origin.kind)])
          continue;
        rows.push({
          name: c.name,
          origin: "file",
          path: c.name,
          state: "local",
          stale: false,
          proposed: null,
          prClosed: null,
          upstream: null,
        });
      }
      return rows.sort(
        (a, b) =>
          a.origin.localeCompare(b.origin) || a.name.localeCompare(b.name),
      );
    },

    actionableCount(): number {
      return this.rows.filter(isActionable).length;
    },
  },
  actions: {
    persist(): void {
      localStorage.setItem(EDITS_KEY, JSON.stringify(this.index));
    },

    /** Drop a record and any text it owns. Does not persist -- callers batch that. */
    async dropRecord(rec: ChangeRecord): Promise<void> {
      if (rec.origin === "sumo") {
        try {
          await (await dir()).removeEntry(opfsSafeName(rec.name));
        } catch {
          /* nothing written yet, or already gone */
        }
      }
      delete this.index[key(rec.name, rec.origin)];
    },

    /**
     * Refresh every tracked path's upstream SHA from one tree read, then drop
     * any record whose saved content is what upstream already holds (a landed
     * pull request, or an edit someone else committed verbatim).
     *
     * Best effort: a failed or rate-limited read leaves the previous SHAs in
     * place and reports nothing, since "we could not check" must not read as
     * "nothing changed upstream" anywhere downstream -- `upstreamSha`
     * returning null is what suppresses the stale flag.
     */
    async refreshUpstreamShas({
      force = false,
    }: { force?: boolean } = {}): Promise<void> {
      if (!Object.keys(this.index).length) return; // nothing tracked, nothing to check
      let tree: any[];
      try {
        tree = await fetchSumoTree({ force });
      } catch {
        return;
      }
      this.upstreamShas = Object.fromEntries(
        tree.filter((e) => e.type === "blob").map((e) => [e.path, e.sha]),
      );
      let dropped = false;
      for (const rec of Object.values<ChangeRecord>(this.index)) {
        const up = this.upstreamSha(rec.path);
        if (up && up === rec.savedBlobSha) {
          await this.dropRecord(rec);
          dropped = true;
        }
      }
      if (dropped) this.persist();
    },

    /**
     * Poll the open pull requests this page opened. Merged ones normally clear
     * via the blob comparison above; this exists for the other endings --
     * closed without merging (the change is still the user's to carry), and
     * merged with modifications (upstream differs from what was pushed, so the
     * file goes back to looking modified and needs the reason shown).
     */
    async refreshProposals(): Promise<void> {
      const numbers = new Set(
        Object.values<ChangeRecord>(this.index)
          .filter((r) => r.proposed)
          .map((r) => r.proposed!.number),
      );
      let changed = false;
      for (const n of numbers) {
        let pr: any;
        try {
          pr = await githubApi(`/repos/${SUMO.owner}/${SUMO.repo}/pulls/${n}`);
        } catch {
          continue;
        }
        if (pr.state !== "closed") continue;
        for (const rec of Object.values<ChangeRecord>(this.index)) {
          if (rec.proposed?.number !== n) continue;
          rec.prClosed = {
            number: n,
            url: rec.proposed.url,
            merged: Boolean(pr.merged),
          };
          rec.proposed = null;
          changed = true;
        }
      }
      if (changed) this.persist();
    },

    /**
     * The unpushed text for a `sumo`-origin constituent, or null if it has
     * none. `fromOrigin` consults this before fetching, which is what keeps
     * local work alive across a boot that re-fetches everything from upstream.
     */
    async readEdit(name: string): Promise<string | null> {
      if (!this.index[key(name, "sumo")]) return null; // untracked: never touch OPFS
      try {
        const handle = await (await dir()).getFileHandle(opfsSafeName(name));
        return await (await handle.getFile()).text();
      } catch {
        return null;
      }
    },

    /** Stop tracking a constituent entirely (it was removed from the KB). */
    async forget(name: string, origin: OriginKind): Promise<void> {
      const rec = this.index[key(name, origin)];
      if (!rec) return;
      await this.dropRecord(rec);
      this.persist();
    },

    /**
     * Record that `text` was saved as `name`/`origin`, storing the text itself
     * for `sumo` origin (local uploads already live in OPFS under their own
     * name).
     *
     * `pristine` is the content the buffer started from, used ONCE to stamp
     * the upstream version this edit descends from; later saves keep the
     * original stamp, so a file edited five times is still compared against
     * what upstream held when the user first touched it.
     *
     * Saving content that matches the recorded base, or that matches what
     * upstream holds now, stops the tracking instead of extending it -- which
     * makes "revert to upstream" and "take upstream's newer copy" the same
     * operation as an ordinary save, with no separate code path to keep
     * consistent.
     *
     * Returns the record, or null if nothing is tracked.
     */
    async recordSave(
      name: string,
      origin: OriginKind,
      text: string,
      pristine?: string,
    ): Promise<ChangeRecord | null> {
      const k = key(name, origin);
      const rec = this.index[k];
      // Local uploads are only tracked once they have been proposed upstream --
      // before that there is nothing to compare them against.
      if (origin !== "sumo" && !rec) return null;

      const path = rec?.path || name;
      const sha = await blobSha(text);
      const base =
        rec?.baseBlobSha ??
        (pristine === undefined ? null : await blobSha(pristine));
      const up = this.upstreamSha(path);
      const backToUpstream =
        sha && ((up && sha === up) || (base && sha === base && !rec?.proposed));
      if (backToUpstream) {
        if (rec) {
          await this.dropRecord(rec);
          this.persist();
        }
        return null;
      }

      if (origin === "sumo") await writeEdit(name, text);
      this.index[k] = {
        name,
        origin,
        path,
        baseBlobSha: base,
        savedBlobSha: sha,
        savedAt: Date.now(),
        proposed: rec?.proposed || null,
        prClosed: rec?.prClosed || null,
      };
      this.persist();
      return this.index[k];
    },

    /** Stamp every file just pushed with the pull request now carrying it. */
    markProposed(
      entries: {
        name: string;
        origin: OriginKind;
        path: string;
        blobSha: string;
      }[],
      pr: { number: number; url: string; branch: string; headOwner: string },
    ): void {
      for (const e of entries) {
        const k = key(e.name, e.origin);
        const rec: ChangeRecord = this.index[k] || {
          name: e.name,
          origin: e.origin,
          path: e.path,
          baseBlobSha: null,
          savedBlobSha: e.blobSha,
          savedAt: Date.now(),
          proposed: null,
          prClosed: null,
        };
        rec.path = e.path;
        rec.savedBlobSha = e.blobSha;
        rec.prClosed = null;
        rec.proposed = {
          number: pr.number,
          url: pr.url,
          branch: pr.branch,
          headOwner: pr.headOwner,
          blobSha: e.blobSha,
        };
        this.index[k] = rec;
      }
      this.persist();
    },

    /**
     * Upstream's current content for a tracked path. Read by blob SHA where
     * one is known, so the text is exactly the version the staleness check
     * compared against rather than whatever the raw CDN is serving this second.
     */
    async fetchUpstreamText(path: string): Promise<string> {
      const sha = this.upstreamSha(path);
      if (sha) {
        const blob = await githubApi(
          `/repos/${SUMO.owner}/${SUMO.repo}/git/blobs/${sha}`,
        );
        if (blob?.encoding === "base64" && blob.content)
          return fromBase64(blob.content);
      }
      const r = await fetch(rawUrl(path));
      if (!r.ok) throw new Error(`${path}: HTTP ${r.status}`);
      return r.text();
    },
  },
});
