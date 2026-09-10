/**
 * GitHub contribution flow for the demo — fork, branch, commit, pull request.
 *
 * Pure API layer: no DOM, no globals, token passed in per call. The token
 * itself comes from the signed-in GitHub session (see auth.ts / the
 * functions/api/github-auth* OAuth flow) — this module just spends it against
 * `api.github.com` over `Authorization: Bearer`, unaware of where it came from.
 *
 * INVARIANT: this module never pushes to a default branch. Every change lands
 * on a freshly created feature branch and is proposed by pull request — there
 * is no direct-commit path, no merge call, and no fork sync (which would write
 * to the fork's own main). `assertFeatureBranch` enforces it at both the
 * branch-creation and commit steps.
 */

import type { ProposedInfo } from './changes.ts';

const API = 'https://api.github.com';

export class GitHubError extends Error {
  status: number;
  /** Set when GitHub's own rate limit (not a scope/permission problem) caused
   *  this failure -- github-api.ts uses it to prompt sign-in only for the
   *  case that's actually fixed by signing in. */
  rateLimited: boolean;
  constructor(message: string, status: number, rateLimited = false) {
    super(message);
    this.name = 'GitHubError';
    this.status = status;
    this.rateLimited = rateLimited;
  }
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export async function api(token: string | null, path: string, opts: RequestInit = {}) {
  const res = await fetch(path.startsWith('http') ? path : API + path, {
    ...opts,
    headers: {
      Accept: 'application/vnd.github+json',
      'X-GitHub-Api-Version': '2022-11-28',
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
      ...(opts.body ? { 'Content-Type': 'application/json' } : {}),
      ...opts.headers,
    },
  });
  if (res.status === 204) return null;
  const data = await res.json().catch(() => null);
  if (!res.ok) {
    const msg = data?.message || `HTTP ${res.status}`;
    const rateLimited = res.status === 403
      && (res.headers.get('x-ratelimit-remaining') === '0' || /rate limit/i.test(msg));
    let detail = msg;
    if (res.status === 401) {
      detail = 'Token rejected by GitHub (check that it has not expired).';
    } else if (rateLimited) {
      detail = token
        ? `${msg} — this token's GitHub API rate limit is exhausted.`
        : `${msg} — log in with GitHub to raise the API limit.`;
    } else if (res.status === 403) {
      detail = `${msg} — the token may lack permission for this operation.`;
    }
    throw new GitHubError(detail, res.status, rateLimited);
  }
  return data;
}

/** UTF-8-safe base64. `btoa` throws on the non-ASCII that appears in SUMO documentation strings. */
export function toBase64(text) {
  const bytes = new TextEncoder().encode(text);
  let bin = '';
  for (let i = 0; i < bytes.length; i += 0x8000) {
    bin += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
  }
  return btoa(bin);
}

/** Inverse of `toBase64`, for blob reads. GitHub wraps its base64 payloads at
 *  60 columns, which `atob` rejects. */
export function fromBase64(b64) {
  const bin = atob(b64.replace(/\s/g, ''));
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return new TextDecoder().decode(bytes);
}

/** Path segments must survive encoding without turning "/" into "%2F". */
const encodePath = (p) => p.split('/').map(encodeURIComponent).join('/');

/**
 * Hard invariant: every write goes to a purpose-made feature branch, never to a
 * default branch. Checked at branch creation and again immediately before the
 * commit, so a future edit that reorders or reuses this code still trips it
 * rather than writing to main.
 */
export function assertFeatureBranch(branch, base) {
  if (!branch || branch === base || /^(main|master)$/i.test(branch)) {
    throw new GitHubError(
      `Refusing to write to "${branch || '(none)'}": contributions must go to a new branch.`, 0);
  }
}

/** Validate a token and return the authenticated user. */
export const whoami = (token) => api(token, '/user');

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
 *
 * `existing` adds this commit to a pull request already under review instead
 * of opening a new one.
 */
interface ContributeFilesOpts {
  token: string;
  owner: string;
  repo: string;
  files: { path: string; content: string; name?: string }[];
  title: string;
  body: string;
  existing?: ProposedInfo | null;
  branchPrefix?: string;
  onStep?: (msg: string) => void;
}

interface ContributeFilesResult {
  url: string;
  number: number;
  branch: string;
  headOwner: string;
  forked: boolean;
  amended: boolean;
  blobShas: string[];
}

export async function contributeFiles({
  token, owner, repo, files, title, body, existing = null,
  branchPrefix = 'sumo-browser', onStep = () => {},
}: ContributeFilesOpts): Promise<ContributeFilesResult> {
  if (!token) throw new GitHubError('No token supplied.', 0);
  if (!files?.length) throw new GitHubError('No files selected.', 0);
  const missing = files.find((f) => !f.path);
  if (missing) throw new GitHubError(`No repository path given for ${missing.name || 'a file'}.`, 0);

  onStep('Checking token…');
  const { login } = await api(token, '/user');

  onStep('Checking repository access…');
  const upstream = await api(token, `/repos/${owner}/${repo}`);
  const base = upstream.default_branch;
  const canPush = Boolean(upstream.permissions?.push);

  let headOwner = existing ? existing.headOwner : owner;
  if (!existing && !canPush) {
    onStep('Forking the repository…');
    await api(token, `/repos/${owner}/${repo}/forks`, { method: 'POST' });
    headOwner = login;
    // Forks are created asynchronously — the repo 404s until it materializes.
    let ready = false;
    for (let i = 0; i < 30 && !ready; i++) {
      try { await api(token, `/repos/${headOwner}/${repo}`); ready = true; }
      catch (e) { if (e.status !== 404) throw e; await sleep(1000); }
    }
    if (!ready) throw new GitHubError('Fork did not become available in time — retry in a moment.', 0);
  }

  // The branch to commit on, and the commit it grows from.
  let branch, parentSha;
  if (existing) {
    branch = existing.branch;
    assertFeatureBranch(branch, base);
    onStep(`Updating branch ${branch}…`);
    // encodePath, not encodeURIComponent: a branch name's own "/" is a literal
    // path separator in this endpoint and must not be escaped.
    const head = await api(token, `/repos/${headOwner}/${repo}/git/ref/heads/${encodePath(branch)}`);
    parentSha = head.object.sha;
  } else {
    onStep('Creating branch…');
    // Branch point is the CURRENT upstream tip. Forks share object storage with
    // the upstream network, so a ref in the fork can point straight at an
    // upstream commit — which means we never have to sync (i.e. write to) the
    // fork's default branch either.
    const upstreamRef = await api(token, `/repos/${owner}/${repo}/git/ref/heads/${base}`);
    const stem = files.length === 1 ? files[0].path.replace(/[^A-Za-z0-9._-]/g, '-') : 'changes';
    branch = `${branchPrefix}/${stem}-${Date.now().toString(36)}`;
    assertFeatureBranch(branch, base);
    parentSha = upstreamRef.object.sha;
    try {
      await api(token, `/repos/${headOwner}/${repo}/git/refs`, {
        method: 'POST',
        body: JSON.stringify({ ref: `refs/heads/${branch}`, sha: parentSha }),
      });
    } catch (e) {
      // Upstream commit not reachable in the fork: branch off the fork's own tip
      // instead. Still a fresh branch, still no write to any default branch.
      const forkRef = await api(token, `/repos/${headOwner}/${repo}/git/ref/heads/${base}`);
      parentSha = forkRef.object.sha;
      await api(token, `/repos/${headOwner}/${repo}/git/refs`, {
        method: 'POST',
        body: JSON.stringify({ ref: `refs/heads/${branch}`, sha: parentSha }),
      });
    }
  }

  assertFeatureBranch(branch, base);   // re-check immediately before the write

  const blobShas = [];
  for (let i = 0; i < files.length; i++) {
    onStep(`Uploading ${files[i].path} (${i + 1}/${files.length})…`);
    const blob = await api(token, `/repos/${headOwner}/${repo}/git/blobs`, {
      method: 'POST',
      body: JSON.stringify({ content: toBase64(files[i].content), encoding: 'base64' }),
    });
    blobShas.push(blob.sha);
  }

  onStep('Committing…');
  const parent = await api(token, `/repos/${headOwner}/${repo}/git/commits/${parentSha}`);
  const tree = await api(token, `/repos/${headOwner}/${repo}/git/trees`, {
    method: 'POST',
    body: JSON.stringify({
      base_tree: parent.tree.sha,
      tree: files.map((f, i) => ({ path: f.path, mode: '100644', type: 'blob', sha: blobShas[i] })),
    }),
  });
  const commit = await api(token, `/repos/${headOwner}/${repo}/git/commits`, {
    method: 'POST',
    body: JSON.stringify({ message: title, tree: tree.sha, parents: [parentSha] }),
  });
  await api(token, `/repos/${headOwner}/${repo}/git/refs/heads/${encodePath(branch)}`, {
    method: 'PATCH',
    body: JSON.stringify({ sha: commit.sha }),
  });

  if (existing) {
    return { url: existing.url, number: existing.number, branch, headOwner,
             forked: headOwner !== owner, amended: true, blobShas };
  }

  onStep('Opening pull request…');
  const pr = await api(token, `/repos/${owner}/${repo}/pulls`, {
    method: 'POST',
    body: JSON.stringify({
      title, body,
      head: headOwner === owner ? branch : `${login}:${branch}`,
      base,
    }),
  });

  return { url: pr.html_url, number: pr.number, branch, headOwner,
           forked: headOwner !== owner, amended: false, blobShas };
}
