/**
 * GitHub OAuth session: the in-memory access token and the signed-in user's
 * profile, both fetched fresh from /api/me on every page load -- never
 * persisted, replacing the old localStorage-backed personal access token.
 *
 * Owns the header's login/user-menu buttons and the login/logout
 * confirmation dialogs shared by every entry point that needs a GitHub
 * identity: the header itself, a rate-limited anonymous API call
 * (github-api.ts), and the Contribute panel (tabs/contribute.ts).
 */

import { $ } from './dom.ts';

interface GithubUser {
  name: string;
  login: string;
  avatarUrl: string;
}

let token: string | null = null;
let user: GithubUser | null = null;

/** The token to send with GitHub API calls, or null when signed out. */
export function currentAuthToken(): string | null {
  return token;
}

export function currentUser(): GithubUser | null {
  return user;
}

function renderAuthUI() {
  const loginBtn = $('githubLogin');
  const userBtn = $('userMenu');
  if (!loginBtn || !userBtn) return;
  loginBtn.hidden = Boolean(user);
  userBtn.hidden = !user;
  if (user) {
    const avatar = $('userAvatar');
    if (avatar) { avatar.src = user.avatarUrl; avatar.alt = user.name; }
    const name = $('userName');
    if (name) name.textContent = user.name;
  }
}

/** Fetch the session's profile + token once at boot, in one call -- /api/me
 *  returns both together (see functions/api/me/index.ts). Best-effort: a 401
 *  (not logged in) or a network failure both just leave the page signed out
 *  -- the header falls back to the "Log in" button either way. */
export async function initAuth() {
  try {
    const res = await fetch('/api/me');
    if (res.ok) {
      const me = await res.json();
      token = me.token;
      user = { name: me.name, login: me.login, avatarUrl: me.avatarUrl };
    } else {
      token = null;
      user = null;
    }
  } catch {
    token = null;
    user = null;
  }
  renderAuthUI();
}

/** Explain why a login prompt appeared. The dialog's own confirm control is a
 *  real link to /api/github-auth (a full-page hand-off to GitHub's consent
 *  screen), not a click handler here. Guarded against a dialog that's
 *  already open: several anonymous GitHub reads can hit the rate limit
 *  around the same time (e.g. on page load), and showModal() throws
 *  InvalidStateError on an already-open <dialog>. */
export function openLoginDialog() {
  const dialog = $('loginDialog');
  if (dialog && !dialog.open) dialog.showModal();
}

$('loginDialogCancel')?.addEventListener('click', () => $('loginDialog').close());

$('userMenu')?.addEventListener('click', () => $('logoutDialog').showModal());
$('logoutDialogCancel')?.addEventListener('click', () => $('logoutDialog').close());
$('logoutDialogConfirm')?.addEventListener('click', async () => {
  const btn = $('logoutDialogConfirm');
  btn.disabled = true;
  try {
    await fetch('/api/logout', { method: 'POST' });
  } catch {
    // Best-effort: GitHub or the network may be unreachable -- still clear
    // the local, in-memory state below rather than leaving the UI stuck
    // showing a session the user just asked to end.
  }
  token = null;
  user = null;
  renderAuthUI();
  $('logoutDialog').close();
  btn.disabled = false;
});

// Kick off the session check as soon as this module loads; nothing here
// blocks the KB boot sequence (see boot.ts), so the app is usable before
// this resolves.
initAuth();
