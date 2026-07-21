/**
 * GitHub contribution flow for the demo — fork, branch, commit, pull request.
 *
 * Pure API layer: no DOM, no globals, token passed in per call. `api.github.com`
 * is CORS-enabled and accepts `Authorization: Bearer`, so this whole flow runs
 * from the static page with a user-supplied token; nothing is proxied and there
 * is no server that could see the token.
 *
 * A classic PAT with `public_repo` is sufficient (fine-grained tokens are
 * scoped per-repository, which makes fork-then-PR-upstream awkward).
 *
 * INVARIANT: this module never pushes to a default branch. Every change lands
 * on a freshly created feature branch and is proposed by pull request — there
 * is no direct-commit path, no merge call, and no fork sync (which would write
 * to the fork's own main). `assertFeatureBranch` enforces it at both the
 * branch-creation and commit steps.
 */

const API = 'https://api.github.com';

export class GitHubError extends Error {
  constructor(message, status) { super(message); this.name = 'GitHubError'; this.status = status; }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function api(token, path, opts = {}) {
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
    throw new GitHubError(
      res.status === 401 ? 'Token rejected by GitHub (check it has not expired).'
      : res.status === 403 ? `${msg} — the token may lack the "public_repo" scope, or you hit a rate limit.`
      : msg, res.status);
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
 * Propose a single-file change upstream.
 *
 * Branches directly when the user can push to `owner/repo`, otherwise forks
 * and opens the PR cross-repo. Reports progress through `onStep`.
 *
 * @returns {Promise<{url:string, number:number, branch:string, forked:boolean}>}
 */
export async function contribute({
  token, owner, repo, path, content, title, body,
  branchPrefix = 'sumo-browser', onStep = () => {},
}) {
  if (!token) throw new GitHubError('No token supplied.', 0);
  if (!path) throw new GitHubError('No file selected.', 0);

  onStep('Checking token…');
  const { login } = await api(token, '/user');

  onStep('Checking repository access…');
  const upstream = await api(token, `/repos/${owner}/${repo}`);
  const base = upstream.default_branch;
  const canPush = Boolean(upstream.permissions?.push);

  let headOwner = owner;
  if (!canPush) {
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

  onStep('Creating branch…');
  // Branch point is the CURRENT upstream tip. Forks share object storage with
  // the upstream network, so a ref in the fork can point straight at an
  // upstream commit — which means we never have to sync (i.e. write to) the
  // fork's default branch either.
  const upstreamRef = await api(token, `/repos/${owner}/${repo}/git/ref/heads/${base}`);
  const branch = `${branchPrefix}/${path.replace(/[^A-Za-z0-9._-]/g, '-')}-${Date.now().toString(36)}`;
  assertFeatureBranch(branch, base);
  try {
    await api(token, `/repos/${headOwner}/${repo}/git/refs`, {
      method: 'POST',
      body: JSON.stringify({ ref: `refs/heads/${branch}`, sha: upstreamRef.object.sha }),
    });
  } catch (e) {
    // Upstream commit not reachable in the fork: branch off the fork's own tip
    // instead. Still a fresh branch, still no write to any default branch.
    const forkRef = await api(token, `/repos/${headOwner}/${repo}/git/ref/heads/${base}`);
    await api(token, `/repos/${headOwner}/${repo}/git/refs`, {
      method: 'POST',
      body: JSON.stringify({ ref: `refs/heads/${branch}`, sha: forkRef.object.sha }),
    });
  }

  onStep('Committing…');
  assertFeatureBranch(branch, base);   // re-check immediately before the write
  // Updating requires the blob sha it replaces; its absence (404) means the
  // path is new on this branch, which the same endpoint handles as a create.
  let sha;
  try {
    const existing = await api(token, `/repos/${headOwner}/${repo}/contents/${encodePath(path)}?ref=${encodeURIComponent(branch)}`);
    sha = existing?.sha;
  } catch (e) { if (e.status !== 404) throw e; }
  await api(token, `/repos/${headOwner}/${repo}/contents/${encodePath(path)}`, {
    method: 'PUT',
    body: JSON.stringify({ message: title, content: toBase64(content), branch, ...(sha ? { sha } : {}) }),
  });

  onStep('Opening pull request…');
  const pr = await api(token, `/repos/${owner}/${repo}/pulls`, {
    method: 'POST',
    body: JSON.stringify({
      title, body,
      head: headOwner === owner ? branch : `${login}:${branch}`,
      base,
    }),
  });

  return { url: pr.html_url, number: pr.number, branch, forked: headOwner !== owner };
}
