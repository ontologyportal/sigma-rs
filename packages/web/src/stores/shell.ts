/**
 * Page chrome that belongs to no tab: the theme, the bug-report link, the
 * settings modal, the version-change notice, and the "/" search shortcut.
 */

import { defineStore } from "pinia";
import { APP_REPO, SEEN_VERSION_KEY, THEME_KEY } from "../constants";

/** The deployed build's identity, from `version.json`. */
export interface AppVersion {
  version: string;
  build: string;
  commit: string;
}

export type Theme = "light" | "dark";

const DARK_QUERY = "(prefers-color-scheme: dark)";

function applyTheme(theme: Theme | null) {
  if (theme) document.documentElement.dataset.theme = theme;
  else delete document.documentElement.dataset.theme;
}

export const useShellStore = defineStore("shell", {
  state: () => ({
    /** The explicit header choice (localStorage THEME_KEY); null follows the OS. */
    theme: null as Theme | null,
    /** The OS preference, kept current via matchMedia. */
    systemDark: false,
    version: null as AppVersion | null,
    settingsOpen: false,
    /** Welcome on first visit, notice on upgrade. */
    versionDialog: { open: false, title: "", body: "" },
    /** Bumped by `requestSearchFocus`; the Browse view watches it. */
    searchFocusRequest: 0,
  }),
  getters: {
    /** Whether the page currently renders dark: the explicit choice wins,
     *  the OS preference is the fallback. */
    isDark: (state): boolean =>
      state.theme ? state.theme === "dark" : state.systemDark,
  },
  actions: {
    /** Read the persisted theme, apply it, and follow the OS preference. */
    init() {
      let saved: string | null = null;
      try {
        saved = localStorage.getItem(THEME_KEY);
      } catch {
        /* private mode */
      }
      this.theme = saved === "dark" || saved === "light" ? saved : null;
      applyTheme(this.theme);
      const mq = window.matchMedia?.(DARK_QUERY);
      if (!mq) return;
      this.systemDark = mq.matches;
      mq.addEventListener("change", (e) => {
        this.systemDark = e.matches;
      });
    },

    toggleTheme() {
      const next: Theme = this.isDark ? "light" : "dark";
      this.theme = next;
      try {
        localStorage.setItem(THEME_KEY, next);
      } catch {
        /* private mode */
      }
      applyTheme(next);
    },

    /** Fetch `version.json` (absent in local dev) and, when the version
     *  differs from the one last seen, open the version dialog. */
    async loadVersion() {
      let v: AppVersion | null = null;
      try {
        const r = await fetch("./version.json");
        v = r.ok ? await r.json() : null;
      } catch {
        /* local dev / no version.json published yet */
      }
      if (!v) return;
      this.version = v;
      this.checkVersionChange(v.version);
    },

    checkVersionChange(version: string) {
      let seen: string | null;
      try {
        seen = localStorage.getItem(SEEN_VERSION_KEY);
      } catch {
        return;
      }
      if (seen === version) return;
      if (seen === null) {
        this.versionDialog.title = "Welcome to SigmaKEE";
        this.versionDialog.body = "";
      } else {
        this.versionDialog.title = "New version available";
        this.versionDialog.body = `You are using a new version (v${version}).`;
      }
      this.versionDialog.open = true;
      try {
        localStorage.setItem(SEEN_VERSION_KEY, version);
      } catch {
        /* private mode */
      }
    },

    /** No anonymous issue creation via the GitHub API, so this opens a
     *  prefilled "new issue" page against this app's own repo. */
    openBugReport() {
      const v = this.version;
      const body = [
        "",
        "",
        "---",
        `Version: ${v?.version ?? "unknown"} (build ${v?.build ?? "?"}, ${v?.commit ?? "?"})`,
        `URL: ${location.href}`,
        `User agent: ${navigator.userAgent}`,
      ].join("\n");
      const url =
        `https://github.com/${APP_REPO.owner}/${APP_REPO.repo}/issues/new?` +
        new URLSearchParams({ title: "", body, labels: "bug,web-ui" });
      window.open(url, "_blank", "noopener");
    },

    requestSearchFocus() {
      this.searchFocusRequest += 1;
    },
  },
});
