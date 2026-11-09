/**
 * Independent GitHub and GitLab sessions. GitHub OAuth is restored from
 * /api/me; GitLab credentials stay on the server.
 */

import { defineStore } from "pinia";
import { gitlabRequest } from "../api/gitlab";
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
    gitlabUser: null as { login: string; server: string } | null,
    loginDialogOpen: false,
    logoutDialogOpen: false,
  }),
  getters: {
    githubSignedIn: (state) => state.user !== null,
    accountName: (state) => state.user?.name || state.user?.login || "",
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
      await Promise.all([this.initGithub(), this.initGitlab()]);
    },

    /** Restore only GitLab profile metadata; credentials remain on the server. */
    async initGitlab() {
      const initialUser = this.gitlabUser;
      try {
        const result = await gitlabRequest<{
          user: { login: string; server: string } | null;
        }>("session");
        if (this.gitlabUser === initialUser) this.gitlabUser = result.user;
      } catch {
        // Preserve the displayed account if a network interruption prevents checking it.
      }
    },

    async initGithub() {
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

    /** Accept a GitLab connection only after server-side validation succeeds. */
    async loginWithGitlabToken(
      server: string,
      token: string,
      signal?: AbortSignal,
    ) {
      const account = await gitlabRequest<{ login: string; server: string }>(
        "login",
        { server, token },
        signal,
      );
      signal?.throwIfAborted();
      this.gitlabUser = account;
      return account;
    },

    /** Sign out of GitHub from the header account dialog. */
    async logout() {
      await fetch("/api/logout", { method: "POST" }).catch(() => {});
      this.token = null;
      this.user = null;
      this.logoutDialogOpen = false;
      this.loginDialogOpen = false;
    },

    async logoutGitlab() {
      const user = this.gitlabUser;
      await gitlabRequest("logout");
      if (this.gitlabUser === user) this.gitlabUser = null;
    },

    /** Offer OAuth or a personal token, including after anonymous rate limits. */
    openLoginDialog() {
      this.loginDialogOpen = true;
    },
  },
});
