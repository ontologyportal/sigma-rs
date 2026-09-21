/**
 * GitHub authentication: an OAuth session or a manually supplied token.
 * Tokens and profiles stay in memory; OAuth is restored from /api/me.
 * Also owns the shared login and logout dialogs.
 */

import { defineStore } from "pinia";
import { whoami } from "../api/github";

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
    /** Validate a personal token with GitHub and use it for this page only. */
    async loginWithToken(value: string, signal?: AbortSignal) {
      const token = value.trim();
      if (!token) throw new Error("Enter a GitHub access token.");
      const user = await whoami(token, signal);
      signal?.throwIfAborted();
      this.token = token;
      this.user = {
        name: user.name || user.login,
        login: user.login,
        avatarUrl: user.avatar_url,
      };
      this.loginDialogOpen = false;
    },

    /** Fetch the session's profile + token once at boot, in one call --
     *  /api/me returns both together (see functions/api/me/index.ts).
     *  Best-effort: a 401 (not logged in) or a network failure both just
     *  leave the page signed out. */
    async init() {
      try {
        const res = await fetch("/api/me");
        if (res.ok) {
          const me = await res.json();
          if (this.token) return;
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
      if (this.token) return;
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

    /** Offer OAuth or a personal token, including after anonymous rate limits. */
    openLoginDialog() {
      this.loginDialogOpen = true;
    },
  },
});
