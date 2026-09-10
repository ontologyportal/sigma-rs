/**
 * The knowledge base's lifecycle: constituent mutations, and the deferred
 * promote + validate they all funnel through.
 *
 * Mutations INGEST (fast) but do not promote; each one runs `reprocess()` once
 * to promote + validate under the post-processing toast.
 */

import { MERGE, SUMO_FILE_SETTING, PROMOTE_TABS } from "./constants.ts";
import { state } from "./state.ts";
import { call } from "./rpc.ts";
import { $, esc } from "./dom.ts";
import { scheduleKbCacheSave } from "./kb-cache.ts";
import { lspReset, lspSyncDocument } from "./editor/lsp-client.ts";
import { recordSave, forgetChange } from "./changes.ts";
import { refreshChangeUi } from "./tabs/contribute.ts";
import { currentTab, router } from "./router.ts";
import { renderDiagnostics, applyDiagRouteParams } from "./tabs/diagnostics.ts";
import {
  renderConstituents,
  renderPicker,
  renderWordNetPanel,
} from "./tabs/kb-tab.ts";
import { reinstallWordNetIfEnabled } from "./boot.ts";
import { renderTests } from "./tabs/tests.ts";
import { populateEditPicker } from "./tabs/edit.ts";
import {
  refreshHomeStats,
  updateHomeNote,
  markStatsStale,
} from "./tabs/home-stats.ts";
import { openManPage, runSearch } from "./tabs/browse.ts";

// -- KB state mutations -------------------------------------------------------

/**
 * Ingest one constituent's text into the worker session and track it. The
 * constituent is tracked once ingested — ingest still accepts content that
 * carries non-fatal notices (e.g. "duplicate formula ignored").
 * @returns {Promise<{ added: boolean, notices: string[] }>}
 */
export async function ingestConstituent(name, text, origin = "sumo") {
  if (state.constituents.some((c) => c.name === name))
    return { added: false, notices: [`${name}: already loaded`] };
  const { notices } = await call("ingest", { name, text });
  state.constituents.push({ name, text, origin });
  if (
    state.savedConstituents.find(
      (c) => c.name == name && c.origin == origin,
    ) === undefined
  ) {
    state.savedConstituents.push({ name, origin });
    localStorage.setItem(
      SUMO_FILE_SETTING,
      JSON.stringify(state.savedConstituents),
    );
  }
  return { added: true, notices };
}

/** Rebuild the worker session from the current (cached) constituents — used by remove/reset/edit. */
async function rebuildSession() {
  await call("newSession");
  lspReset(); // the worker dropped its WasmLsp with the session
  for (const c of state.constituents)
    await call("ingest", { name: c.name, text: c.text });
  // newSession() also drops any installed WordNet lexicon; reinstall it (from
  // the cached fetch, so this never re-downloads) so search keeps its
  // synonym expansion across a remove/reset/edit rebuild.
  await reinstallWordNetIfEnabled();
}

/**
 * Save `text` as constituent `name`/`origin` — updates it in place if already
 * loaded, else adds it. Used by the Edit tab's Save button. For `file`-origin
 * constituents, persists to OPFS FIRST (awaited), mirroring the KB tab's upload
 * flow — a `file` entry with no OPFS handle would throw on next boot and abort
 * loading every OTHER constituent too. A `sumo`-origin save is persisted the
 * same way, into the separate edit store, and stays local until pushed.
 * @returns {Promise<{ added: boolean, notices: string[] }>}
 */
export async function updateConstituentText(name, text, origin = "file") {
  if (origin === "file") {
    if (!state.opfsRoot) throw new Error("File system not initialized yet");
    const handle = await state.opfsRoot.getFileHandle(name, { create: true });
    const stream = await handle.createWritable();
    await stream.write(text);
    await stream.close();
  }
  const idx = state.constituents.findIndex(
    (c) => c.name === name && c.origin === origin,
  );
  // The text this edit descends from, which only the FIRST save of a file can
  // observe — by the second, the constituent already holds the first save.
  const pristine = idx === -1 ? undefined : state.constituents[idx].text;
  await recordSave(name, origin, text, pristine);
  if (idx === -1) {
    const r = await ingestConstituent(name, text, origin);
    await reprocess();
    return r;
  }
  state.constituents[idx] = { ...state.constituents[idx], text };
  // In-place diff-commit instead of rebuildSession(): the LSP didChange lane
  // reconciles the buffer under the file's own name, so only the changed
  // sentences are processed — a one-formula edit costs a diff, not a full
  // re-ingest of every constituent. reprocess() then re-promotes (no-op for
  // untouched files) and re-validates for correct whole-KB diagnostics.
  // Remove/reset below genuinely retract whole files, so they keep the rebuild.
  await lspSyncDocument(name, text);
  await reprocess();
  return { added: false, notices: [] };
}

export async function removeConstituent(name, origin = "sumo") {
  state.constituents = state.constituents.filter(
    (c) => c.name !== name || c.origin !== origin,
  );
  state.savedConstituents = state.savedConstituents.filter(
    (c) => c.name !== name || c.origin !== origin,
  );
  localStorage.setItem(
    SUMO_FILE_SETTING,
    JSON.stringify(state.savedConstituents),
  );
  if (origin === "file") {
    try {
      const h = await state.opfsRoot.getFileHandle(name);
      await h.remove();
    } catch {
      /* already gone */
    }
  }
  await forgetChange(name, origin);
  await rebuildSession();
  await reprocess();
}

export async function resetToMerge() {
  const merge = state.constituents.find((c) => c.name === MERGE);
  const dropped = state.constituents.filter((c) => c !== merge);
  state.constituents = merge ? [merge] : [];
  // A record left behind for a constituent that is gone would resurrect with a
  // long-stale base the next time that file is loaded.
  for (const c of dropped) await forgetChange(c.name, c.origin);
  await rebuildSession();
  await reprocess();
}

// -- Deferred promote + post-processing UI ------------------------------------

// Keep the toast up at least this long so the post-processing state is
// perceptible even when promote+validate finish in well under one paint frame.
const MIN_TOAST_MS = 650;

/**
 * Run `fn` (promote → validate → render) under the "post-processing" UI: grey
 * the promote-dependent tabs and show the toast until it finishes. Ingest
 * happens BEFORE this (under the loading screen on boot / the busy button on
 * adds). Re-entrant: a nested call runs inside the outer window.
 */
async function withPostProcessing(fn) {
  const outer = !state.promoting;
  if (outer) {
    state.promoting = true;
    setPromoteTabsEnabled(false);
    showToast(true);
  }
  const shownAt = performance.now();
  try {
    await fn();
  } finally {
    if (outer) {
      const held = performance.now() - shownAt;
      if (held < MIN_TOAST_MS)
        await new Promise((r) => setTimeout(r, MIN_TOAST_MS - held));
      state.promoting = false;
      setPromoteTabsEnabled(true);
      showToast(false);
      // A deep link into (or a live visit to) a promote tab got evicted to
      // Browse while promoting -- `setPromoteTabsEnabled` remembered where
      // from; return there now that it's usable (no history push: the
      // eviction never pushed one either, so there's nothing to keep in
      // sync -- see `evictedFrom` below).
      if (evictedFrom && currentTab() === "browse") {
        router.replace({ name: evictedFrom, query: evictedQuery });
        evictedFrom = null;
      }
      // Anything that renders the `promoting` flag has to be redrawn HERE.
      // Views refreshed inside the window (renderAll → refreshHomeStats) ran
      // while the flag was still set, so their "post-processing" wording is
      // stale the moment it clears.
      if (currentTab() === "browse") updateHomeNote();
    }
  }
}

// Promote every ingested constituent into the axiom base, THEN validate once,
// THEN refresh every view. Promote and validate are the KB-size-bound steps —
// validation runs exactly once here, not per constituent.
async function promoteAndValidate() {
  await call("promoteAll", { names: state.constituents.map((c) => c.name) });
  state.diagnostics = (await call("validate")).diagnostics;
  markStatsStale();
  renderAll();
  // The route was applied before any of this existed; re-honour ?file/?sev/?l
  // now that there is something to filter and scroll to -- only meaningful
  // while Diagnostics is actually showing (this can run while another tab is).
  applyDiagRouteParams(currentTab() === "diagnostics" ? router.currentRoute.value.query : null);
  // Queued, not awaited: every mutation path (ingest/edit/remove/reset)
  // funnels through here, so the cache tracks the last successful promote —
  // no separate invalidation step (see scheduleKbCacheSave).
  scheduleKbCacheSave();
}

export function reprocess() {
  return withPostProcessing(promoteAndValidate);
}

/** Refresh every view that reflects KB contents (after promote+validate). */
export function renderAll() {
  renderDiagnostics();
  renderConstituents();
  renderWordNetPanel();
  renderTests();
  refreshLangSelect();
  if (state.sumoCatalog) renderPicker();
  populateEditPicker();
  refreshChangeUi();
  if (currentTab() === "browse") refreshHomeStats(); // counts moved
}

// Remembers what tab (and its query) `setPromoteTabsEnabled` evicted the user
// FROM, e.g. a deep link into /prover opened cold before the boot promote
// finishes, or a live visit to a promote tab when a fresh add re-triggers
// promotion -- so `withPostProcessing`'s cleanup can send them back once it's
// usable again. `router.replace` (not `push`), same as the original
// `pushState`-free deflection: an automatic eviction is not a navigation the
// user should have to hit Back through.
let evictedFrom: ReturnType<typeof currentTab> | null = null;
let evictedQuery: typeof router.currentRoute.value.query | undefined;

function setPromoteTabsEnabled(on) {
  for (const t of PROMOTE_TABS) {
    const btn = document.querySelector(`nav.tabs [data-tab=${t}]`);
    if (btn) {
      btn.classList.toggle("disabled", !on);
      btn.setAttribute("aria-disabled", String(!on));
    }
  }
  if (!on && PROMOTE_TABS.includes(currentTab())) {
    evictedFrom = currentTab();
    evictedQuery = router.currentRoute.value.query;
    router.replace({ name: "browse" });
  }
}

function showToast(on) {
  const t = $("toast");
  if (t) t.hidden = !on;
}

/**
 * Populate the header language selector from the KB's `NaturalLanguage`
 * instances, preserving the current choice. Fire-and-forget: the selected
 * symbol lands in `state.uiLanguage`, consumed by search, man-page rendering,
 * and the NL paraphrases. Only call where the language list can actually have
 * changed (boot, promote) — the worker round-trip is KB-bound.
 */
export async function refreshLangSelect() {
  const sel = $("langSelect");
  if (!sel) return;
  let languages;
  try {
    languages = (await call("naturalLanguages")).languages;
  } catch {
    return;
  }
  if (!languages || !languages.length) return;
  const has = (v) => languages.some((l) => l.symbol === v);
  const options = languages
    .map((l) => `<option value="${esc(l.symbol)}">${esc(l.label)}</option>`)
    .join("");
  // Unchanged list → leave the DOM alone (a rebuild would collapse the
  // dropdown under the user's pointer and reset the selection).
  if (sel.dataset.options !== options) {
    sel.dataset.options = options;
    sel.innerHTML = options;
  }
  sel.value = has(state.uiLanguage)
    ? state.uiLanguage
    : has("EnglishLanguage")
      ? "EnglishLanguage"
      : languages[0].symbol;
  state.uiLanguage = sel.value;
}

/** React to a language change by re-rendering whatever the Browse tab is
 *  showing so its documentation follows the selection -- called once from
 *  `vue/main.ts` AFTER the single root app mounts (this module is a
 *  transitive dependency of the app's own component tree, so its top-level
 *  code runs before the header's language selector exists in the DOM). */
export function initLangSelect() {
  $("langSelect")?.addEventListener("change", () => {
    const sel = $("langSelect");
    state.uiLanguage = sel.value;
    if (currentTab() !== "browse") return;
    const params = new URLSearchParams(location.search);
    const sym = params.get("sym");
    const q = params.get("q") || $("q").value.trim();
    if (sym) openManPage(sym);
    else if (q) runSearch(q);
  });
}
