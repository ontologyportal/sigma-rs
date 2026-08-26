/**
 * The knowledge base's lifecycle: constituent mutations, and the deferred
 * promote + validate they all funnel through.
 *
 * Mutations INGEST (fast) but do not promote; each one runs `reprocess()` once
 * to promote + validate under the post-processing toast.
 */

import { MERGE, SUMO_FILE_SETTING, PROMOTE_TABS } from './constants.ts';
import { state } from './state.ts';
import { call } from './rpc.ts';
import { $, esc } from './dom.ts';
import { scheduleKbCacheSave } from './kb-cache.ts';
import { recordSave, forgetChange } from './changes.ts';
import { refreshChangeUi } from './tabs/contribute.ts';
import { currentTab, routeFromLocation, showTab } from './router.ts';
import { renderDiagnostics, applyDiagRouteParams } from './tabs/diagnostics.ts';
import { renderConstituents, renderPicker } from './tabs/kb-tab.ts';
import { renderTests } from './tabs/tests.ts';
import { populateEditPicker } from './tabs/edit.ts';
import { refreshHomeStats, updateHomeNote, markStatsStale } from './tabs/home-stats.ts';
import { openManPage, runSearch } from './tabs/browse.ts';

// -- KB state mutations -------------------------------------------------------

/**
 * Ingest one constituent's text into the worker session and track it. The
 * constituent is tracked once ingested — ingest still accepts content that
 * carries non-fatal notices (e.g. "duplicate formula ignored").
 * @returns {Promise<{ added: boolean, notices: string[] }>}
 */
export async function ingestConstituent(name, text, origin = 'sumo') {
  if (state.constituents.some((c) => c.name === name)) return { added: false, notices: [`${name}: already loaded`] };
  const { notices } = await call('ingest', { name, text });
  state.constituents.push({ name, text, origin });
  if (state.savedConstituents.find((c) => c.name == name && c.origin == origin) === undefined) {
    state.savedConstituents.push({ name, origin });
    localStorage.setItem(SUMO_FILE_SETTING, JSON.stringify(state.savedConstituents));
  }
  return { added: true, notices };
}

/** Rebuild the worker session from the current (cached) constituents — used by remove/reset/edit. */
async function rebuildSession() {
  await call('newSession');
  for (const c of state.constituents) await call('ingest', { name: c.name, text: c.text });
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
export async function updateConstituentText(name, text, origin = 'file') {
  if (origin === 'file') {
    if (!state.opfsRoot) throw new Error('File system not initialized yet');
    const handle = await state.opfsRoot.getFileHandle(name, { create: true });
    const stream = await handle.createWritable();
    await stream.write(text);
    await stream.close();
  }
  const idx = state.constituents.findIndex((c) => c.name === name && c.origin === origin);
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
  // In-place diff-commit instead of rebuildSession(): validateBuffer stages
  // the buffer under the file's own name and commits it, so only the changed
  // sentences are processed — a one-formula edit costs a diff, not a full
  // re-ingest of every constituent. reprocess() then re-promotes (no-op for
  // untouched files) and re-validates for correct whole-KB diagnostics.
  // Remove/reset below genuinely retract whole files, so they keep the rebuild.
  await call('validateBuffer', { file: name, text });
  await reprocess();
  return { added: false, notices: [] };
}

export async function removeConstituent(name, origin = 'sumo') {
  state.constituents = state.constituents.filter((c) => c.name !== name || c.origin !== origin);
  state.savedConstituents = state.savedConstituents.filter((c) => c.name !== name || c.origin !== origin);
  localStorage.setItem(SUMO_FILE_SETTING, JSON.stringify(state.savedConstituents));
  if (origin === 'file') {
    try { const h = await state.opfsRoot.getFileHandle(name); await h.remove(); } catch { /* already gone */ }
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
      if (held < MIN_TOAST_MS) await new Promise((r) => setTimeout(r, MIN_TOAST_MS - held));
      state.promoting = false;
      setPromoteTabsEnabled(true);
      showToast(false);
      // A cold-boot deep link into a promote tab (e.g. /prover) got deflected
      // to Browse while promoting — the URL still names the real tab, so
      // return to it now that it's usable (no history push: the URL never
      // changed out from under it).
      const { tab: urlTab } = routeFromLocation();
      if (currentTab() === 'browse' && PROMOTE_TABS.includes(urlTab)) showTab(urlTab, { push: false });
      // Anything that renders the `promoting` flag has to be redrawn HERE.
      // Views refreshed inside the window (renderAll → refreshHomeStats) ran
      // while the flag was still set, so their "post-processing" wording is
      // stale the moment it clears.
      if (currentTab() === 'browse') updateHomeNote();
    }
  }
}

// Promote every ingested constituent into the axiom base, THEN validate once,
// THEN refresh every view. Promote and validate are the KB-size-bound steps —
// validation runs exactly once here, not per constituent.
async function promoteAndValidate() {
  await call('promoteAll', { names: state.constituents.map((c) => c.name) });
  state.diagnostics = (await call('validate')).diagnostics;
  markStatsStale();
  renderAll();
  // The route was applied before any of this existed; re-honour ?file/?sev/?l
  // now that there is something to filter and scroll to.
  applyDiagRouteParams();
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
  renderTests();
  refreshLangSelect();
  if (state.sumoCatalog) renderPicker();
  populateEditPicker();
  refreshChangeUi();
  if (currentTab() === 'browse') refreshHomeStats();   // counts moved
}

function setPromoteTabsEnabled(on) {
  for (const t of PROMOTE_TABS) {
    const btn = document.querySelector(`nav.tabs [data-tab=${t}]`);
    if (btn) { btn.classList.toggle('disabled', !on); btn.setAttribute('aria-disabled', String(!on)); }
  }
  // push:false — this is an automatic deflection (the tab isn't usable yet),
  // not a navigation, so it must not clobber a deep-linked URL (e.g. a
  // /prover link opened cold, before the boot promote finishes).
  if (!on && PROMOTE_TABS.includes(currentTab())) showTab('browse', { push: false });
}

function showToast(on) { const t = $('toast'); if (t) t.hidden = !on; }

/**
 * Populate the header language selector from the KB's `NaturalLanguage`
 * instances, preserving the current choice. Fire-and-forget: the selected
 * symbol lands in `state.uiLanguage`, consumed by search, man-page rendering,
 * and the NL paraphrases. Only call where the language list can actually have
 * changed (boot, promote) — the worker round-trip is KB-bound.
 */
export async function refreshLangSelect() {
  const sel = $('langSelect');
  if (!sel) return;
  let languages;
  try { languages = (await call('naturalLanguages')).languages; }
  catch { return; }
  if (!languages || !languages.length) return;
  const has = (v) => languages.some((l) => l.symbol === v);
  const options = languages
    .map((l) => `<option value="${esc(l.symbol)}">${esc(l.label)}</option>`)
    .join('');
  // Unchanged list → leave the DOM alone (a rebuild would collapse the
  // dropdown under the user's pointer and reset the selection).
  if (sel.dataset.options !== options) {
    sel.dataset.options = options;
    sel.innerHTML = options;
  }
  sel.value = has(state.uiLanguage) ? state.uiLanguage
    : has('EnglishLanguage') ? 'EnglishLanguage' : languages[0].symbol;
  state.uiLanguage = sel.value;
}

// One-time: react to a language change by re-rendering whatever the Browse
// tab is showing so its documentation follows the selection.
$('langSelect')?.addEventListener('change', () => {
  const sel = $('langSelect');
  state.uiLanguage = sel.value;
  if (currentTab() !== 'browse') return;
  const params = new URLSearchParams(location.search);
  const sym = params.get('sym');
  const q = params.get('q') || $('q').value.trim();
  if (sym) openManPage(sym);
  else if (q) runSearch(q);
});
