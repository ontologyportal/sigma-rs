/**
 * GitHub OAuth session: the in-memory access token and the signed-in user's
 * profile, both fetched fresh from /api/me on every page load -- never
 * persisted. Also owns the open/closed state of the login and logout
 * dialogs, so a rate-limited anonymous API call, the Contribute panel, and
 * the header all prompt for an identity the same way.
 */

import { defineStore } from "pinia";

export interface GithubUser {
  name: string;
  login: string;
  avatarUrl: string;
}

export const useAuthStore = defineStore("auth", {
  state: () => ({
    /** The token to send with GitHub API calls, or null when signed out. */
    token: null as string | null,
    user: null as GithubUser | null,
    loginDialogOpen: false,
    logoutDialogOpen: false,
  }),
  getters: {
    signedIn: (state) => state.user !== null,
  },
  actions: {
    /** Fetch the session's profile + token once at boot, in one call --
     *  /api/me returns both together (see functions/api/me/index.ts).
     *  Best-effort: a 401 (not logged in) or a network failure both just
     *  leave the page signed out. */
    async init() {
      try {
        const res = await fetch("/api/me");
        if (res.ok) {
          const me = await res.json();
          this.token = me.token ?? null;
          this.user = {
            name: me.name,
            login: me.login,
            avatarUrl: me.avatarUrl,
          };
          return;
        }
      } catch {
        /* offline, or the API is not deployed */
      }
      this.token = null;
      this.user = null;
    },

    /** End the session. Best-effort: GitHub or the network may be
     *  unreachable -- the local, in-memory state is cleared regardless
     *  rather than leaving the UI stuck showing a session the user just
     *  asked to end. */
    async logout() {
      try {
        await fetch("/api/logout", { method: "POST" });
      } catch {
        /* see above */
      }
      this.token = null;
      this.user = null;
      this.logoutDialogOpen = false;
    },

    /** Explain why a login prompt appeared. The dialog's own confirm control
     *  is a link to /api/github-auth (a full-page hand-off to GitHub's consent
     *  screen). Idempotent: several anonymous GitHub reads can hit the rate
     *  limit around the same time (e.g. on page load). */
    openLoginDialog() {
      this.loginDialogOpen = true;
    },
  },
});
