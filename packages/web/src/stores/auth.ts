/**
 * One active repository account: GitHub OAuth/token or a GitLab session.
 * Tokens and profiles stay in memory; OAuth is restored from /api/me.
 * Also owns the shared login and logout dialogs.
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
    loginProvider: "github" as "github" | "gitlab",
    loginServer: "",
    loginDialogOpen: false,
    logoutDialogOpen: false,
  }),
  getters: {
    signedIn: (state) => state.user !== null || state.gitlabUser !== null,
    githubSignedIn: (state) => state.user !== null,
    provider: (state): "github" | "gitlab" | null =>
      state.user ? "github" : state.gitlabUser ? "gitlab" : null,
    accountName: (state) => state.user?.name || state.gitlabUser?.login || "",
  },
  actions: {
    /** Validate a personal token with GitHub and use it for this page only. */
    async loginWithToken(value: string, signal?: AbortSignal) {
      const token = value.trim();
      if (!token) throw new Error("Enter a GitHub access token.");
      const user = await whoami(token, signal);
      signal?.throwIfAborted();
      if (this.gitlabUser) await gitlabRequest("logout");
      signal?.throwIfAborted();
      this.gitlabUser = null;
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
      await this.initGithub();
      await this.initGitlab();
    },

    /** Restore only GitLab profile metadata; credentials remain on the server. */
    async initGitlab() {
      const initialUser = this.gitlabUser;
      try {
        const result = await gitlabRequest<{
          user: { login: string; server: string } | null;
        }>("session");
        if (this.user || this.token) {
          if (result.user) await gitlabRequest("logout");
          this.gitlabUser = null;
        } else if (this.gitlabUser === initialUser)
          this.gitlabUser = result.user;
      } catch {
        // Preserve the displayed account if a network interruption prevents checking it.
      }
    },

    async initGithub() {
      const initialGitlab = this.gitlabUser;
      try {
        const res = await fetch("/api/me");
        if (res.ok) {
          const me = await res.json();
          if (this.token || this.gitlabUser !== initialGitlab) return;
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
      this.token = null;
      this.user = null;
      this.gitlabUser = account;
      this.loginDialogOpen = false;
      return account;
    },

    /** Clear both legacy session types, leaving one signed-out state. */
    async logout() {
      await Promise.allSettled([
        fetch("/api/logout", { method: "POST" }),
        gitlabRequest("logout"),
      ]);
      this.token = null;
      this.user = null;
      this.gitlabUser = null;
      this.logoutDialogOpen = false;
      this.loginDialogOpen = false;
    },

    /** Offer OAuth or a personal token, including after anonymous rate limits. */
    openLoginDialog(provider: "github" | "gitlab" = "github", server?: string) {
      this.loginProvider = provider;
      this.loginServer = server ?? this.gitlabUser?.server ?? "";
      this.loginDialogOpen = true;
    },
  },
});
