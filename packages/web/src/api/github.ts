/**
 * GitHub REST client for the demo: the pure, token-per-call layer (fork,
 * branch, commit, pull request) plus the page's authenticated entry points
 * (`fetchFileCommits`, `fetchPullRequest`, `fetchBlobText`, `fetchSumoTree`,
 * `fetchLastCommitInfo`) that read the token from the auth store, so catalog
 * reads, commit reads, and contribution writes all authenticate the same way
 * and share one set of rate-limit wording.
 *
 * Every request to `api.github.com` lives here: `githubApi` is deliberately
 * module-private, so a new endpoint becomes a named, typed function in this
 * file rather than an interpolated path in a store or a component. Response
 * shapes come from `@octokit/types` (the generated OpenAPI types -- a
 * devDependency with no runtime, so nothing here reaches the bundle).
 *
 * The token comes from the signed-in GitHub session (the functions/api/
 * github-auth* OAuth flow); this module just spends it against
 * `api.github.com` over `Authorization: Bearer`.
 *
 * INVARIANT: this module never pushes to a default branch. Every change lands
 * on a freshly created feature branch and is proposed by pull request -- there
 * is no direct-commit path, no merge call, and no fork sync (which would write
 * to the fork's own main). `assertFeatureBranch` enforces it at both the
 * branch-creation and commit steps.
 */

import type { Endpoints } from "@octokit/types";

import { APP_REPO, SUMO } from "../constants";
import type { ProposedInfo } from "../stores/changes";
import { useAuthStore } from "../stores/auth";

const API = "https://api.github.com";

/** The response body of one GitHub REST route, keyed the way
 *  `@octokit/types` keys them: `"GET /repos/{owner}/{repo}/commits"`. Types
 *  only -- `@octokit/types` is a devDependency with no runtime, so nothing
 *  here reaches the bundle. */
type Res<R extends keyof Endpoints> = Endpoints[R]["response"]["data"];

/** One commit in a file's history (`fetchFileCommits`). */
export type RepoCommit = Res<"GET /repos/{owner}/{repo}/commits">[number];
/** One entry of a recursive tree listing (`fetchRepoTree`). */
export type TreeEntry =
  Res<"GET /repos/{owner}/{repo}/git/trees/{tree_sha}">["tree"][number];
/** A pull request, as `fetchPullRequest` reports it. */
export type PullRequest = Res<"GET /repos/{owner}/{repo}/pulls/{pull_number}">;
/** A single release, as `fetchAppRelease` reports it. */
export type Release = Res<"GET /repos/{owner}/{repo}/releases/tags/{tag}">;
/** A single git ref -- the branch tips `contributeFiles` commits onto. */
type GitRef = Res<"GET /repos/{owner}/{repo}/git/ref/{ref}">;

export class GitHubError extends Error {
  status: number;
  /** Set when GitHub's own rate limit (not a scope/permission problem) caused
   *  this failure -- `githubApi` uses it to prompt sign-in only for the case
   *  that's actually fixed by signing in. */
  rateLimited: boolean;
  constructor(message: string, status: number, rateLimited = false) {
    super(message);
    this.name = "GitHubError";
    this.status = status;
    this.rateLimited = rateLimited;
  }
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** One GitHub REST request. `path` is API-relative unless it is already an
 *  absolute URL. Resolves to the parsed JSON body (null for 204); rejects
 *  with a `GitHubError` carrying user-facing detail for auth/limit failures. */
export async function api<T = unknown>(
  token: string | null,
  path: string,
  opts: RequestInit = {},
): Promise<T> {
  const res = await fetch(path.startsWith("http") ? path : API + path, {
    ...opts,
    headers: {
      Accept: "application/vnd.github+json",
      "X-GitHub-Api-Version": "2022-11-28",
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
      ...(opts.body ? { "Content-Type": "application/json" } : {}),
      ...(opts.headers as Record<string, string> | undefined),
    },
  });
  // 204 has no body; the callers that can receive one treat it as empty.
  if (res.status === 204) return null as T;
  const data: unknown = await res.json().catch(() => null);
  if (!res.ok) {
    const msg =
      (data as { message?: string } | null)?.message || `HTTP ${res.status}`;
    const rateLimited =
      res.status === 403 &&
      (res.headers.get("x-ratelimit-remaining") === "0" ||
        /rate limit/i.test(msg));
    let detail = msg;
    if (res.status === 401) {
      detail = "Token rejected by GitHub (check that it has not expired).";
    } else if (rateLimited) {
      detail = token
        ? `${msg} - this token's GitHub API rate limit is exhausted.`
        : `${msg} - log in with GitHub to raise the API limit.`;
    } else if (res.status === 403) {
      detail = `${msg} - the token may lack permission for this operation.`;
    }
    throw new GitHubError(detail, res.status, rateLimited);
  }
  return data as T;
}

/** UTF-8-safe base64. `btoa` throws on the non-ASCII that appears in SUMO documentation strings. */
export function toBase64(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    bin += String.fromCharCode.apply(
      null,
      Array.from(bytes.subarray(i, i + 0x8000)),
    );
  }
  return btoa(bin);
}

/** Inverse of `toBase64`, for blob reads. GitHub wraps its base64 payloads at
 *  60 columns, which `atob` rejects. */
export function fromBase64(b64: string): string {
  const bin = atob(b64.replace(/\s/g, ""));
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return new TextDecoder().decode(bytes);
}

/** Path segments must survive encoding without turning "/" into "%2F". */
const encodePath = (p: string) =>
  p.split("/").map(encodeURIComponent).join("/");

/**
 * Hard invariant: every write goes to a purpose-made feature branch, never to a
 * default branch. Checked at branch creation and again immediately before the
 * commit, so a future edit that reorders or reuses this code still trips it
 * rather than writing to main.
 */
export function assertFeatureBranch(branch: string, base: string): void {
  if (!branch || branch === base || /^(main|master)$/i.test(branch)) {
    throw new GitHubError(
      `Refusing to write to "${branch || "(none)"}": contributions must go to a new branch.`,
      0,
    );
  }
}

/** Validate a token and return the authenticated user. */
export const whoami = (token: string | null) =>
  api<Res<"GET /user">>(token, "/user");

export interface ContributeFilesOpts {
  token: string;
  owner: string;
  repo: string;
  files: { path: string; content: string; name?: string }[];
  title: string;
  body: string;
  /** Adds this commit to a pull request already under review instead of opening a new one. */
  existing?: ProposedInfo | null;
  branchPrefix?: string;
  onStep?: (msg: string) => void;
}

export interface ContributeFilesResult {
  url: string;
  number: number;
  branch: string;
  headOwner: string;
  forked: boolean;
  amended: boolean;
  blobShas: string[];
}

/**
 * Propose a set of files upstream as ONE commit, either on a fresh branch with
 * a new pull request or as a follow-up commit on a branch already under review.
 *
 * Branches directly when the user can push to `owner/repo`, otherwise forks and
 * opens the PR cross-repo. Reports progress through `onStep`.
 *
 * The commit is built through the git data API (blobs -> tree -> commit -> ref)
 * rather than one contents-API PUT per file: a five-file change is one reviewable
 * commit instead of five, and the tree is layered onto upstream's own tree, so
 * paths nobody touched are carried over untouched.
 */
export async function contributeFiles({
  token,
  owner,
  repo,
  files,
  title,
  body,
  existing = null,
  branchPrefix = "sumo-browser",
  onStep = () => {},
}: ContributeFilesOpts): Promise<ContributeFilesResult> {
  if (!token) throw new GitHubError("No token supplied.", 0);
  if (!files?.length) throw new GitHubError("No files selected.", 0);
  const missing = files.find((f) => !f.path);
  if (missing)
    throw new GitHubError(
      `No repository path given for ${missing.name || "a file"}.`,
      0,
    );

  onStep("Checking token...");
  const { login } = await api<Res<"GET /user">>(token, "/user");

  onStep("Checking repository access...");
  const upstream = await api<Res<"GET /repos/{owner}/{repo}">>(
    token,
    `/repos/${owner}/${repo}`,
  );
  const base: string = upstream.default_branch;
  const canPush = Boolean(upstream.permissions?.push);

  let headOwner = existing ? existing.headOwner : owner;
  if (!existing && !canPush) {
    onStep("Forking the repository...");
    await api(token, `/repos/${owner}/${repo}/forks`, { method: "POST" });
    headOwner = login;
    // Forks are created asynchronously -- the repo 404s until it materializes.
    let ready = false;
    for (let i = 0; i < 30 && !ready; i++) {
      try {
        await api(token, `/repos/${headOwner}/${repo}`);
        ready = true;
      } catch (e) {
        if ((e as GitHubError).status !== 404) throw e;
        await sleep(1000);
      }
    }
    if (!ready) {
      throw new GitHubError(
        "Fork did not become available in time - retry in a moment.",
        0,
      );
    }
  }

  // The branch to commit on, and the commit it grows from.
  let branch: string;
  let parentSha: string;
  if (existing) {
    branch = existing.branch;
    assertFeatureBranch(branch, base);
    onStep(`Updating branch ${branch}...`);
    // encodePath, not encodeURIComponent: a branch name's own "/" is a literal
    // path separator in this endpoint and must not be escaped.
    const head = await api<GitRef>(
      token,
      `/repos/${headOwner}/${repo}/git/ref/heads/${encodePath(branch)}`,
    );
    parentSha = head.object.sha;
  } else {
    onStep("Creating branch...");
    // Branch point is the CURRENT upstream tip. Forks share object storage with
    // the upstream network, so a ref in the fork can point straight at an
    // upstream commit -- which means we never have to sync (i.e. write to) the
    // fork's default branch either.
    const upstreamRef = await api<GitRef>(
      token,
      `/repos/${owner}/${repo}/git/ref/heads/${base}`,
    );
    const stem =
      files.length === 1
        ? files[0].path.replace(/[^A-Za-z0-9._-]/g, "-")
        : "changes";
    branch = `${branchPrefix}/${stem}-${Date.now().toString(36)}`;
    assertFeatureBranch(branch, base);
    parentSha = upstreamRef.object.sha;
    try {
      await api(token, `/repos/${headOwner}/${repo}/git/refs`, {
        method: "POST",
        body: JSON.stringify({ ref: `refs/heads/${branch}`, sha: parentSha }),
      });
    } catch {
      // Upstream commit not reachable in the fork: branch off the fork's own tip
      // instead. Still a fresh branch, still no write to any default branch.
      const forkRef = await api<GitRef>(
        token,
        `/repos/${headOwner}/${repo}/git/ref/heads/${base}`,
      );
      parentSha = forkRef.object.sha;
      await api(token, `/repos/${headOwner}/${repo}/git/refs`, {
        method: "POST",
        body: JSON.stringify({ ref: `refs/heads/${branch}`, sha: parentSha }),
      });
    }
  }

  assertFeatureBranch(branch, base); // re-check immediately before the write

  const blobShas: string[] = [];
  for (let i = 0; i < files.length; i++) {
    onStep(`Uploading ${files[i].path} (${i + 1}/${files.length})...`);
    const blob = await api<Res<"POST /repos/{owner}/{repo}/git/blobs">>(
      token,
      `/repos/${headOwner}/${repo}/git/blobs`,
      {
        method: "POST",
        body: JSON.stringify({
          content: toBase64(files[i].content),
          encoding: "base64",
        }),
      },
    );
    blobShas.push(blob.sha);
  }

  onStep("Committing...");
  const parent = await api<
    Res<"GET /repos/{owner}/{repo}/git/commits/{commit_sha}">
  >(token, `/repos/${headOwner}/${repo}/git/commits/${parentSha}`);
  const tree = await api<Res<"POST /repos/{owner}/{repo}/git/trees">>(
    token,
    `/repos/${headOwner}/${repo}/git/trees`,
    {
      method: "POST",
      body: JSON.stringify({
        base_tree: parent.tree.sha,
        tree: files.map((f, i) => ({
          path: f.path,
          mode: "100644",
          type: "blob",
          sha: blobShas[i],
        })),
      }),
    },
  );
  const commit = await api<Res<"POST /repos/{owner}/{repo}/git/commits">>(
    token,
    `/repos/${headOwner}/${repo}/git/commits`,
    {
      method: "POST",
      body: JSON.stringify({
        message: title,
        tree: tree.sha,
        parents: [parentSha],
      }),
    },
  );
  await api(
    token,
    `/repos/${headOwner}/${repo}/git/refs/heads/${encodePath(branch)}`,
    {
      method: "PATCH",
      body: JSON.stringify({ sha: commit.sha }),
    },
  );

  if (existing) {
    return {
      url: existing.url,
      number: existing.number,
      branch,
      headOwner,
      forked: headOwner !== owner,
      amended: true,
      blobShas,
    };
  }

  onStep("Opening pull request...");
  const pr = await api<Res<"POST /repos/{owner}/{repo}/pulls">>(
    token,
    `/repos/${owner}/${repo}/pulls`,
    {
      method: "POST",
      body: JSON.stringify({
        title,
        body,
        head: headOwner === owner ? branch : `${login}:${branch}`,
        base,
      }),
    },
  );

  return {
    url: pr.html_url,
    number: pr.number,
    branch,
    headOwner,
    forked: headOwner !== owner,
    amended: false,
    blobShas,
  };
}

// -- Authenticated reads ------------------------------------------------------

/** GitHub REST GET, authenticated with the user's optional token. Prompts
 *  sign-in the moment an ANONYMOUS request hits GitHub's rate limit -- a
 *  signed-in user's own limit being exhausted (rare: 5000/hour) is a
 *  different problem logging in again can't fix, so that case is left as a
 *  plain error instead. */
function githubApi<T = unknown>(path: string): Promise<T> {
  const auth = useAuthStore();
  return api<T>(auth.token, path).catch((e) => {
    if (e instanceof GitHubError && e.rateLimited && !auth.token)
      auth.openLoginDialog();
    throw e;
  });
}

/**
 * One `sumo`-origin file's commit history upstream, newest first. Public
 * data, so no token is required -- which caps an anonymous visitor at
 * GitHub's 60 requests/hour per IP, hence the History tab's own per-file
 * session cache on top of this.
 */
export function fetchFileCommits(
  path: string,
  { perPage = 30 }: { perPage?: number } = {},
): Promise<RepoCommit[]> {
  return githubApi<RepoCommit[]>(
    `/repos/${SUMO.owner}/${SUMO.repo}/commits?path=${encodeURIComponent(path)}&per_page=${perPage}`,
  );
}

/** One upstream pull request by number -- how the change tracker learns that
 *  a proposal it opened was merged or closed. */
export function fetchPullRequest(number: number): Promise<PullRequest> {
  return githubApi<PullRequest>(
    `/repos/${SUMO.owner}/${SUMO.repo}/pulls/${number}`,
  );
}

/**
 * Upstream blob text by SHA, or null when GitHub did not return decodable
 * base64 content (it omits the body for blobs over 1 MB). Reading by SHA
 * pins the text to one exact version, unlike the raw CDN.
 */
export async function fetchBlobText(sha: string): Promise<string | null> {
  const blob = await githubApi<
    Res<"GET /repos/{owner}/{repo}/git/blobs/{file_sha}">
  >(`/repos/${SUMO.owner}/${SUMO.repo}/git/blobs/${sha}`);
  return blob?.encoding === "base64" && blob.content
    ? fromBase64(blob.content)
    : null;
}

// Cache the promise, not the resolved value: the file picker and the change
// tracker both want the upstream tree, and two overlapping callers would
// otherwise spend two of the 60/hour unauthenticated budget on the same read.
const treePromises = new Map<string, Promise<TreeEntry[]>>();

/**
 * Every blob in `owner/repo` at `ref`, as `[{ path, type, sha, size }]`, one
 * recursive tree read per repo+ref, memoized.
 *
 * `force` re-reads instead of reusing the memoized tree -- the check
 * immediately before a pull request must not be answered from a tree fetched
 * minutes ago.
 */
export function fetchRepoTree(
  owner: string,
  repo: string,
  ref: string,
  { force = false }: { force?: boolean } = {},
): Promise<TreeEntry[]> {
  const key = `${owner}/${repo}@${ref}`;
  if (force) treePromises.delete(key);
  let p = treePromises.get(key);
  if (!p) {
    p = githubApi<Res<"GET /repos/{owner}/{repo}/git/trees/{tree_sha}">>(
      `/repos/${owner}/${repo}/git/trees/${ref}?recursive=1`,
    )
      .then((t) => {
        if (t?.truncated)
          console.warn(`${key}: tree listing truncated by GitHub`);
        return t.tree || [];
      })
      .catch((e) => {
        treePromises.delete(key);
        throw e;
      });
    treePromises.set(key, p);
  }
  return p;
}

/**
 * The upstream repository's tree at `SUMO.ref`. One request answers both
 * "which files exist" (the KB tab's library) and "what does upstream hold
 * right now" (the change tracker's staleness check, which needs a current
 * blob SHA per tracked path).
 */
export function fetchSumoTree({
  force = false,
}: { force?: boolean } = {}): Promise<TreeEntry[]> {
  return fetchRepoTree(SUMO.owner, SUMO.repo, SUMO.ref, { force });
}

// Cache the promise, not the resolved value: two overlapping callers would
// both see a null value and each fire a request, spending two of the 60/hour
// unauthenticated budget on one page load.
let lastCommitPromise: Promise<{
  sha: string | null;
  date: Date | null;
}> | null = null;

/** `{ sha, date }` of the latest commit on `SUMO.ref`. Shared by the Browse
 *  stats tile (date) and the KB snapshot cache (sha, the version signal
 *  'sumo'-origin constituents are pinned to). A failed read rejects and
 *  clears the memo so the next caller retries. `force` re-reads instead of
 *  reusing the memo -- an explicit "check again" (the Sources card's
 *  "Update now") must not just replay whatever was cached at page load. */
export function fetchLastCommitInfo({
  force = false,
}: { force?: boolean } = {}): Promise<{
  sha: string | null;
  date: Date | null;
}> {
  if (force) lastCommitPromise = null;
  if (!lastCommitPromise) {
    lastCommitPromise = (async () => {
      const [c] = await githubApi<RepoCommit[]>(
        `/repos/${SUMO.owner}/${SUMO.repo}/commits?per_page=1`,
      );
      const iso = c?.commit?.author?.date;
      return { sha: c?.sha ?? null, date: iso ? new Date(iso) : null };
    })().catch((e) => {
      lastCommitPromise = null;
      throw e;
    });
  }
  return lastCommitPromise;
}

// Cache the promise, not the resolved value -- same reasoning as
// `lastCommitPromise`, generalized to any repo (the Sources card's details
// dialog can be opened for the default SUMO repo or a custom one).
const repoCommitPromises = new Map<
  string,
  Promise<{ sha: string | null; date: Date | null }>
>();

/** `{ sha, date }` of the latest commit on `owner/repo` at `branch` -- the
 *  general form of `fetchLastCommitInfo`, for a Sources card entry that
 *  isn't the default SUMO repo. `GitOrigin.branch` is always a concrete
 *  branch name (never `SUMO.ref`'s bare `"HEAD"`), so the default-repo case
 *  is matched against `SUMO.branch`. `force` -- see `fetchLastCommitInfo`. */
export function fetchRepoLastCommit(
  owner: string,
  repo: string,
  branch: string,
  { force = false }: { force?: boolean } = {},
): Promise<{ sha: string | null; date: Date | null }> {
  if (owner === SUMO.owner && repo === SUMO.repo && branch === SUMO.branch)
    return fetchLastCommitInfo({ force });
  const key = `${owner}/${repo}@${branch}`;
  if (force) repoCommitPromises.delete(key);
  let p = repoCommitPromises.get(key);
  if (!p) {
    p = (async () => {
      const [c] = await githubApi<RepoCommit[]>(
        `/repos/${owner}/${repo}/commits?sha=${encodeURIComponent(branch)}&per_page=1`,
      );
      const iso = c?.commit?.author?.date;
      return { sha: c?.sha ?? null, date: iso ? new Date(iso) : null };
    })().catch((e) => {
      repoCommitPromises.delete(key);
      throw e;
    });
    repoCommitPromises.set(key, p);
  }
  return p;
}

/**
 * This app's own release by tag (e.g. `sigmakee-v2.2.0`) -- the source of the
 * version dialog's "what's new" notes. Public data on `APP_REPO`, so no
 * token is required; resolves to null rather than throwing when the tag has
 * no release yet (a build newer than the last published release).
 */
export async function fetchAppRelease(tag: string): Promise<Release | null> {
  try {
    return await githubApi<Release>(
      `/repos/${APP_REPO.owner}/${APP_REPO.repo}/releases/tags/${encodeURIComponent(tag)}`,
    );
  } catch (e) {
    if (e instanceof GitHubError && e.status === 404) return null;
    throw e;
  }
}
