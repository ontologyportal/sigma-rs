import { createRouter, createWebHistory } from "vue-router";

// Lazy: each view is its own chunk, resolved only on navigation -- so a tab
// still mid-migration (stale imports, not yet wired to the Pinia stores)
// only breaks when a user actually visits it, not for every other route.
export const router = createRouter({
    routes: [
        {
        name: "home", path: "/", component: () => import("../views/BrowseTab.vue")
    }, {
        name: "browse", path: "/browse", component: () => import("../views/BrowseTab.vue"),
    }, {
        name: "prove", path: "/prove", component: () => import("../views/AskTellTab.vue"),
    }, {
        name: "edit", path: "/edit", component: () => import("../views/EditTab.vue"),
    }, {
        name: "diagnostics", path: "/diagnostics", component: () => import("../views/DiagnosticsTab.vue"),
    }, {
        name: "history", path: "/history", component: () => import("../views/HistoryTab.vue"),
    }, {
        name: "audit", path: "/audit", component: () => import("../views/AuditTab.vue"),
    }, {
        name: "kb", path: "/kb", component: () => import("../views/KbTab.vue")
    }
],
    history: createWebHistory()
})