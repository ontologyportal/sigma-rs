/**
 * Page chrome that belongs to no tab: the theme, the bug-report link, the
 * settings modal, the version-change notice, and the "/" search shortcut.
 */

import { defineStore } from "pinia";
import {
  APP_REPO,
  SEEN_VERSION_KEY,
  THEME_KEY,
  LAYOUT_KEY,
  AVAILABLE_LAYOUTS,
  NARROW_LAYOUT_QUERY,
} from "../constants";

/** The deployed build's identity, from `version.json`. */
export interface AppVersion {
  version: string;
  build: string;
  commit: string;
}

export type Theme = "light" | "dark";
export type Layout = (typeof AVAILABLE_LAYOUTS)[number];

const DARK_QUERY = "(prefers-color-scheme: dark)";

function applyTheme(theme: Theme | null) {
  if (theme) document.documentElement.dataset.theme = theme;
  else delete document.documentElement.dataset.theme;
}

function applyLayout(layout: Layout) {
  document.documentElement.dataset.layout = layout;
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
    /** The saved layout preference (SettingsDialog); may not be what's
     *  actually applied -- see `effectiveLayout`. */
    layout: "comfortable" as Layout,
    /** True while the viewport is too narrow for "classic" mode, kept live
     *  via matchMedia. Drives `effectiveLayout`; never itself written to
     *  localStorage. */
    layoutNarrow: false,
  }),
  getters: {
    /** Whether the page currently renders dark: the explicit choice wins,
     *  the OS preference is the fallback. */
    isDark: (state): boolean =>
      state.theme ? state.theme === "dark" : state.systemDark,
    /** The layout actually applied: `layout` (the saved preference),
     *  unless the viewport is currently too narrow for "classic" to render
     *  usably, in which case "comfortable" is forced without touching the
     *  saved preference -- widening the viewport again reverts to it with
     *  no further action. Every view branching on comfortable-vs-classic
     *  reads this, never `layout` directly. */
    effectiveLayout: (state): Layout =>
      state.layoutNarrow ? "comfortable" : state.layout,
  },
  actions: {
    /** Read the persisted theme, apply it, and follow the OS preference. */
    init() {
      let savedTheme: string | null = null;
      let savedLayout: string | null = null;
      try {
        savedTheme = localStorage.getItem(THEME_KEY);
        savedLayout = localStorage.getItem(LAYOUT_KEY);
      } catch {
        /* private mode */
      }
      this.theme =
        savedTheme === "dark" || savedTheme === "light" ? savedTheme : null;
      this.layout = AVAILABLE_LAYOUTS.includes(savedLayout as Layout)
        ? (savedLayout as Layout)
        : "comfortable";
      applyTheme(this.theme);

      // Set before the first changeLayout() call so its applyLayout() sees
      // the right effectiveLayout from the start, not just on the next
      // resize.
      const layoutMq = window.matchMedia?.(NARROW_LAYOUT_QUERY);
      if (layoutMq) {
        this.layoutNarrow = layoutMq.matches;
        layoutMq.addEventListener("change", (e) => {
          this.layoutNarrow = e.matches;
          applyLayout(this.effectiveLayout);
        });
      }
      this.changeLayout(this.layout);

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

    changeLayout(layout: string | null) {
      const newLayout =
        layout && AVAILABLE_LAYOUTS.includes(layout as Layout)
          ? (layout as Layout)
          : "comfortable";
      this.layout = newLayout;
      try {
        localStorage.setItem(LAYOUT_KEY, this.layout);
      } catch {
        /* private mode */
      }
      applyLayout(this.effectiveLayout);
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
