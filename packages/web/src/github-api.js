/**
 * The page's authenticated entry point to the pure client in `github.js`: one
 * place that knows where the user's token lives, so catalog reads, commit
 * reads, and contribution writes all authenticate the same way (and share one
 * set of rate-limit wording).
 */

import { api } from './github.js';
import { $ } from './dom.js';
import { state } from './state.js';

/** The token to send: whatever is in the Contribute panel's field right now,
 *  else the remembered/session one. */
export function currentGithubToken() {
  return $('ghToken')?.value.trim() || state.ghToken;
}

/** GitHub REST GET, authenticated with the user's optional token. */
export function githubApi(path) {
  return api(currentGithubToken(), path);
}
