/**
 * The constituent library: every file the app knows about, whether or not
 * it is loaded into the KB. Three sources feed it -- registered git repos
 * (listed lazily from one tree read each), local uploads (text kept in the
 * OPFS `library/` directory), and single-file URLs (fetched on load, never
 * stored). Repos and local/URL entries persist to localStorage; repo
 * catalogs are in-memory only.
 */

import { defineStore } from "pinia";
import { LIBRARY_KEY, SUMO } from "../constants";
import {
  GitOrigin,
  LocalOrigin,
  type Origin,
  type OriginKind,
  type RemoteOrigin,
} from "../models/Origin";
import { fetchRepoTree } from "../api/github";
import { fetchText } from "../services/sources";
import { errMsg } from "../utils/format";
import { useBootStore } from "./boot";
import { opfsSafeName } from "./changes";
import { isTestFile } from "./tests";

/** A registered git repository + branch (GitHub only for now). */
export interface RepoRef {
  owner: string;
  repo: string;
  branch: string;
}

export interface LocalEntry {
  kind: "file";
  name: string;
  size: number;
  added: number;
}

export interface UrlEntry {
  kind: "url";
  name: string;
  url: string;
  size: number;
  added: number;
}

export type LibraryEntry = LocalEntry | UrlEntry;

/** One `.kif` / `.kif.tq` / `.p` / `.tptp` blob in a repo, with its byte size. */
export interface CatalogEntry {
  path: string;
  size: number;
}

const LIBRARY_DIR = "library";
export const isKif = (name: string) => /\.kif$/i.test(name);
/** What the library lists: KIF constituents and test files. */
const isLibraryFile = (name: string) => isKif(name) || isTestFile(name);

/** The extension filter for an import: `kif` constituents or `test` files. */
export type ImportAccept = "kif" | "test";
export const acceptsFile = (accept: ImportAccept, name: string) =>
  accept === "kif" ? isKif(name) : isTestFile(name);

export const DEFAULT_REPO: RepoRef = {
  owner: SUMO.owner,
  repo: SUMO.repo,
  branch: SUMO.branch,
};

export const repoId = (r: RepoRef) => `github:${r.owner}/${r.repo}@${r.branch}`;
export const sameRepo = (a: RepoRef, b: RepoRef) => repoId(a) === repoId(b);
export const isDefaultRepo = (r: RepoRef) => sameRepo(r, DEFAULT_REPO);
export const originForRepo = (r: RepoRef) =>
  new GitOrigin("github", r.owner, r.repo, r.branch);

/** localStorage holds whatever an older build wrote; these two guards are
 *  the only gate between that and the store's state. */
function isRepoRef(v: unknown): v is RepoRef {
  const r = v as RepoRef | null;
  return !!r && !!r.owner && !!r.repo && !!r.branch;
}

function isLibraryEntry(v: unknown): v is LibraryEntry {
  const e = v as LibraryEntry | null;
  return !!e && (e.kind === "file" || e.kind === "url") && !!e.name;
}

function loadPersisted(): { repos: RepoRef[]; entries: LibraryEntry[] } {
  let repos: RepoRef[] = [];
  let entries: LibraryEntry[] = [];
  try {
    const raw: { repos?: unknown; entries?: unknown } | null = JSON.parse(
      localStorage.getItem(LIBRARY_KEY) || "null",
    );
    if (Array.isArray(raw?.repos)) repos = raw.repos.filter(isRepoRef);
    if (Array.isArray(raw?.entries))
      entries = raw.entries.filter(isLibraryEntry);
  } catch {
    /* corrupt value */
  }
  // The upstream repo is always registered, always first.
  repos = [DEFAULT_REPO, ...repos.filter((r) => !isDefaultRepo(r))];
  return { repos, entries };
}

let libraryDirHandle: FileSystemDirectoryHandle | null = null;

async function opfsRoot(): Promise<FileSystemDirectoryHandle> {
  const boot = useBootStore();
  if (!boot.opfsRoot) throw new Error("File system not initialized yet");
  return boot.opfsRoot;
}

async function libraryDir(): Promise<FileSystemDirectoryHandle> {
  if (libraryDirHandle) return libraryDirHandle;
  libraryDirHandle = await (
    await opfsRoot()
  ).getDirectoryHandle(LIBRARY_DIR, { create: true });
  return libraryDirHandle;
}

async function readOpfsText(
  dir: FileSystemDirectoryHandle,
  name: string,
): Promise<string | null> {
  try {
    return await (await (await dir.getFileHandle(name)).getFile()).text();
  } catch {
    return null;
  }
}

async function writeOpfsText(
  dir: FileSystemDirectoryHandle,
  name: string,
  text: string,
): Promise<void> {
  const handle = await dir.getFileHandle(name, { create: true });
  const w = await handle.createWritable();
  await w.write(text);
  await w.close();
}

/** A URL's file name, or the whole URL when it has no usable last segment. */
function urlBaseName(url: string): string {
  try {
    const last = new URL(url).pathname.split("/").filter(Boolean).pop();
    return last ? decodeURIComponent(last) : url;
  } catch {
    return url;
  }
}

export const useLibraryStore = defineStore("library", {
  state: () => ({
    ...loadPersisted(),
    /** Per repo id: its KIF and test-file blobs, or null until listed. */
    catalogs: {} as Record<string, CatalogEntry[] | null>,
    catalogErrors: {} as Record<string, string>,
  }),
  getters: {
    defaultRepo: (state) => state.repos.find(isDefaultRepo) ?? DEFAULT_REPO,
    repoId: () => repoId,
    entry:
      (state) =>
      (name: string, kind: OriginKind): LibraryEntry | undefined =>
        state.entries.find((e) => e.name === name && e.kind === kind),
    /** Every non-test file the library knows about. */
    size: (state) =>
      state.entries.filter((e) => !isTestFile(e.name)).length +
      Object.values(state.catalogs).reduce(
        (n, c) => n + (c ?? []).filter((e) => !isTestFile(e.path)).length,
        0,
      ),
    /** One line for the tables' hint: the catalogs still listing, or the
     *  repos whose listing failed. */
    catalogNote: (state): { text: string; error: boolean } => {
      const errors = state.repos
        .map((r) => {
          const err = state.catalogErrors[repoId(r)];
          return err ? `${originForRepo(r).label}: ${err}` : "";
        })
        .filter(Boolean);
      if (errors.length)
        return {
          text: `could not load file list — ${errors.join("; ")}`,
          error: true,
        };
      const pending = state.repos.some((r) => !state.catalogs[repoId(r)]);
      return { text: pending ? "loading file lists…" : "", error: false };
    },
  },
  actions: {
    persist() {
      localStorage.setItem(
        LIBRARY_KEY,
        JSON.stringify({ repos: this.repos, entries: this.entries }),
      );
    },

    /** List a repo's KIF and test files -- one tree read per repo, shared
     *  with the change tracker for the default repo (which reads `SUMO.ref`). */
    async loadCatalog(repo: RepoRef, { force = false } = {}): Promise<void> {
      const id = repoId(repo);
      if (this.catalogs[id] && !force) return;
      delete this.catalogErrors[id];
      try {
        const tree = await fetchRepoTree(
          repo.owner,
          repo.repo,
          isDefaultRepo(repo) ? SUMO.ref : repo.branch,
          { force },
        );
        this.catalogs[id] = tree
          .filter((e) => e.type === "blob" && isLibraryFile(e.path))
          .map((e) => ({
            path: e.path as string,
            size: Number(e.size) || 0,
          }))
          .sort((a, b) => a.path.localeCompare(b.path));
      } catch (e) {
        this.catalogErrors[id] = errMsg(e);
        throw e;
      }
    },

    /** Best-effort listing of every registered repo (errors land in
     *  `catalogErrors`, not thrown). */
    async loadCatalogs(): Promise<void> {
      await Promise.all(
        this.repos.map((r) =>
          this.loadCatalog(r).catch(() => {
            /* recorded in catalogErrors */
          }),
        ),
      );
    },

    /** Register a repo, validating it by listing its tree. Rejects
     *  duplicates and repos with no KIF or test files. */
    async addRepo(r: RepoRef): Promise<void> {
      const repo: RepoRef = {
        owner: r.owner.trim(),
        repo: r.repo.trim(),
        branch: r.branch.trim() || "main",
      };
      if (!repo.owner || !repo.repo)
        throw new Error("Enter a GitHub user and repository name.");
      if (this.repos.some((x) => sameRepo(x, repo)))
        throw new Error(
          `${repo.owner}/${repo.repo}@${repo.branch} is already registered.`,
        );
      await this.loadCatalog(repo);
      if (!this.catalogs[repoId(repo)]?.length)
        throw new Error(
          `${repo.owner}/${repo.repo}@${repo.branch} has no .kif or test files.`,
        );
      this.repos.push(repo);
      this.persist();
    },

    /** Forget a non-default repo. Callers ensure none of its files is loaded. */
    removeRepo(r: RepoRef): void {
      if (isDefaultRepo(r)) return;
      this.repos = this.repos.filter((x) => !sameRepo(x, r));
      delete this.catalogs[repoId(r)];
      delete this.catalogErrors[repoId(r)];
      this.persist();
    },

    /** Store `text` as local entry `name` (created or replaced) -- shared by
     *  upload and the editor's save of a `file`-origin constituent. */
    async writeLocal(name: string, text: string): Promise<LocalEntry> {
      await writeOpfsText(await libraryDir(), opfsSafeName(name), text);
      const entry: LocalEntry = {
        kind: "file",
        name,
        size: text.length,
        added: Date.now(),
      };
      const idx = this.entries.findIndex(
        (e) => e.kind === "file" && e.name === name,
      );
      if (idx === -1) this.entries.push(entry);
      else this.entries[idx] = entry;
      this.persist();
      return entry;
    },

    /** Make sure a loaded `file`/`url` constituent is listed, so unloading
     *  it later leaves it in the library rather than dropping it. Covers
     *  constituents saved before the library existed (a `url` whose name
     *  is the URL, a `file` restored from OPFS) and is a no-op for `sumo`
     *  origin and for anything already listed. Never touches OPFS: a
     *  `file` constituent's text is already there, or it could not have
     *  been loaded. */
    ensureEntry(name: string, origin: Origin, size: number): void {
      if (origin.kind === "sumo") return;
      if (this.entries.some((e) => e.kind === origin.kind && e.name === name))
        return;
      const added = Date.now();
      this.entries.push(
        origin.kind === "file"
          ? { kind: "file", name, size, added }
          : {
              kind: "url",
              name,
              url: (origin as RemoteOrigin).url || name,
              size,
              added,
            },
      );
      this.persist();
    },

    /** Move uploads from before the library existed -- loose files at the
     *  OPFS root under their bare name -- into the library directory and
     *  list them, whether or not they are currently loaded. Runs once per
     *  boot, before anything reads the library; a file that fails to move
     *  is left in place for the next boot. */
    async adoptLegacy(): Promise<string[]> {
      const root = await opfsRoot();
      const dir = await libraryDir();
      // Collected first: removing entries while iterating a directory is
      // not guaranteed to visit the rest.
      const loose: string[] = [];
      for await (const [name, handle] of root.entries())
        if (handle.kind === "file" && isLibraryFile(name)) loose.push(name);
      const adopted: string[] = [];
      for (const name of loose) {
        try {
          // A library copy already there was written by this version and is
          // the newer one; the root copy is just a leftover.
          const kept = await readOpfsText(dir, opfsSafeName(name));
          if (kept === null) {
            const text = await readOpfsText(root, name);
            if (text === null) continue;
            await this.writeLocal(name, text);
          } else this.ensureEntry(name, new LocalOrigin(), kept.length);
          await root.removeEntry(name);
          adopted.push(name);
        } catch {
          /* leave it for the next boot */
        }
      }
      return adopted;
    },

    /** Import picked files of the `accept`ed kind (`.kif`, or the test
     *  extensions); a file's name is its path relative to the picked folder
     *  (or its bare name for a single pick). */
    async importFiles(
      files: File[],
      accept: ImportAccept = "kif",
    ): Promise<{ added: string[]; skipped: string[] }> {
      const added: string[] = [];
      const skipped: string[] = [];
      for (const file of files) {
        const name = file.webkitRelativePath || file.name;
        if (!acceptsFile(accept, name)) {
          skipped.push(name);
          continue;
        }
        await this.writeLocal(name, await file.text());
        added.push(name);
      }
      return { added, skipped };
    },

    /** Register a URL (a `.kif` constituent or a test file) after fetching
     *  it once to validate it and record its size. The entry's name is the
     *  URL's file name unless that would collide with a different URL
     *  already in the library. */
    async importUrl(url: string): Promise<UrlEntry> {
      url = url.trim();
      if (!/^https?:\/\//i.test(url))
        throw new Error("Enter an http(s) URL of a .kif or test file.");
      const text = await fetchText(url);
      let name = urlBaseName(url);
      const clash = this.entries.find(
        (e) => e.kind === "url" && e.name === name && e.url !== url,
      );
      if (clash) name = url;
      const entry: UrlEntry = {
        kind: "url",
        name,
        url,
        size: text.length,
        added: Date.now(),
      };
      const idx = this.entries.findIndex(
        (e) => e.kind === "url" && e.url === url,
      );
      if (idx === -1) this.entries.push(entry);
      else this.entries[idx] = entry;
      this.persist();
      return entry;
    },

    /** Remove a local/URL entry from the library (and OPFS, for a file).
     *  Callers ensure it is not loaded. */
    async deleteEntry(name: string, kind: OriginKind): Promise<void> {
      this.entries = this.entries.filter(
        (e) => !(e.name === name && e.kind === kind),
      );
      this.persist();
      if (kind !== "file") return;
      try {
        await (await libraryDir()).removeEntry(opfsSafeName(name));
      } catch {
        /* already gone */
      }
    },

    /** A local entry's text. */
    async readLocal(name: string): Promise<string> {
      const text = await readOpfsText(await libraryDir(), opfsSafeName(name));
      if (text === null) throw new Error(`${name}: not in the library`);
      return text;
    },
  },
});
