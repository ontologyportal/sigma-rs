/**
 * Tabs + URL routing. Path-based (`/edit?file=Merge.kif&l=100`), served by
 * the SPA fallback (Cloudflare `_redirects`, Vite dev server). Legacy
 * `?tab=` bookmarks redirect onto the real path.
 *
 * The route query is the single source of truth for everything carried in
 * the URL: views derive their state from it and change it only through
 * `navigate`/`updateParams`.
 */

import {
  createRouter,
  createWebHistory,
  type LocationQueryRaw,
  type RouteLocationRaw,
} from "vue-router";
import { BASE, PROMOTE_TABS, TABS, type TabName } from "../constants";
import { useKBStore } from "../stores/kb";

export type { TabName };

export const router = createRouter({
  history: createWebHistory(BASE),
  routes: [
    {
      name: "home",
      path: "/",
      component: () => import("../views/BrowseTab.vue"),
    },
    {
      name: "browse",
      path: "/browse",
      component: () => import("../views/BrowseTab.vue"),
    },
    {
      name: "prover",
      path: "/prover",
      component: () => import("../views/AskTellTab.vue"),
    },
    {
      name: "audit",
      path: "/audit",
      component: () => import("../views/AuditTab.vue"),
    },
    {
      name: "edit",
      path: "/edit",
      component: () => import("../views/EditTab.vue"),
    },
    {
      name: "diagnostics",
      path: "/diagnostics",
      component: () => import("../views/DiagnosticsTab.vue"),
    },
    { name: "kb", path: "/kb", component: () => import("../views/KbTab.vue") },
    {
      name: "history",
      path: "/history",
      component: () => import("../views/HistoryTab.vue"),
    },
    { path: "/:pathMatch(.*)*", redirect: { name: "home" } },
  ],
});

// Legacy `?tab=` bookmarks (the old query-string scheme) resolve onto the
// real path, dropping the query param.
router.beforeEach((to) => {
  if (to.name && to.name !== "home" && to.name !== "browse") return true;
  const legacy = to.query.tab;
  if (
    typeof legacy !== "string" ||
    !(TABS as readonly string[]).includes(legacy)
  )
    return true;
  const query = { ...to.query };
  delete query.tab;
  if (legacy === "browse") return { name: "browse", query, replace: true };
  return { name: legacy, query, replace: true };
});

// Promote-gated tabs are unusable while the KB is being axiomatized: bounce
// a navigation there back to Browse. App.vue handles evicting a tab that is
// already showing when promotion starts, and returning to it afterwards.
router.beforeEach((to) => {
  const kb = useKBStore();
  if (kb.promoting && PROMOTE_TABS.includes(String(to.name)))
    return { name: "browse" };
  return true;
});

/** The active tab, `'browse'` for the home route. */
export function currentTab(): TabName {
  const name = router.currentRoute.value.name;
  return name === "home" || !name ? "browse" : (name as TabName);
}

function toQuery(obj: Record<string, unknown> | undefined): LocationQueryRaw {
  const query: LocationQueryRaw = {};
  for (const [k, v] of Object.entries(obj || {})) {
    if (v != null && v !== "") query[k] = String(v);
  }
  return query;
}

/** Push a history entry for `tab` with `query`. Every cross-tab jump goes
 *  through here so param naming stays consistent. */
export function navigate(tab: TabName, query?: Record<string, unknown>) {
  const to: RouteLocationRaw = { name: tab, query: toQuery(query) };
  return router.push(to);
}

/** Replace the query on the current route -- keeps the address bar
 *  shareable without a history entry per keystroke/filter change. */
export function updateParams(query: Record<string, unknown>) {
  return router.replace({
    name: router.currentRoute.value.name ?? "home",
    query: toQuery(query),
  });
}
