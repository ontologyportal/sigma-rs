/**
 * The page's authenticated entry point to the pure client in `github.ts`: one
 * place that knows where the user's token lives, so catalog reads, commit
 * reads, and contribution writes all authenticate the same way (and share one
 * set of rate-limit wording).
 */

import { SUMO } from './constants.ts';
import { api, GitHubError } from './github.ts';
import { currentAuthToken, openLoginDialog } from './auth.ts';

/** The token to send with GitHub API calls, or null when signed out. */
export function currentGithubToken() {
  return currentAuthToken();
}

/** GitHub REST GET, authenticated with the user's optional token. Prompts
 *  sign-in the moment an ANONYMOUS request hits GitHub's rate limit -- a
 *  signed-in user's own limit being exhausted (rare: 5000/hour) is a
 *  different problem logging in again can't fix, so that case is left as a
 *  plain error instead. */
export function githubApi(path) {
  return api(currentGithubToken(), path).catch((e) => {
    if (e instanceof GitHubError && e.rateLimited && !currentGithubToken()) openLoginDialog();
    throw e;
  });
}

// Cache the promise, not the resolved value: the file picker and the change
// tracker both want this tree, and two overlapping callers would otherwise
// spend two of the 60/hour unauthenticated budget on the same read.
let sumoTreePromise = null;

/**
 * Every blob in the upstream repository at `SUMO.ref`, as
 * `[{ path, type, sha }]`. One request answers both "which files exist"
 * (the KB tab's picker) and "what does upstream hold right now" (the change
 * tracker's staleness check, which needs a current blob SHA per tracked path).
 *
 * @param {{force?: boolean}} opts `force` re-reads instead of reusing the
 *   memoized tree — the check immediately before a pull request must not be
 *   answered from a tree fetched minutes ago.
 */
export function fetchSumoTree({ force = false } = {}) {
  if (force) sumoTreePromise = null;
  if (!sumoTreePromise) {
    sumoTreePromise = githubApi(`/repos/${SUMO.owner}/${SUMO.repo}/git/trees/${SUMO.ref}?recursive=1`)
      .then((t) => t.tree || [])
      .catch((e) => { sumoTreePromise = null; throw e; });
  }
  return sumoTreePromise;
}
