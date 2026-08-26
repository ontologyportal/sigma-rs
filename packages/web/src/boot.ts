/** Startup: the progress bar, the constituent fetch+ingest loop, and the
 *  hand-off to the router once the page is usable. */

import { state } from './state.ts';
import { call } from './rpc.ts';
import { $ } from './dom.ts';
import { fromOrigin } from './sources.ts';
import { ingestConstituent, renderAll, refreshLangSelect, reprocess } from './kb.ts';
import { tryRestoreFromCache } from './kb-cache.ts';
import { refreshUpstreamShas, refreshProposals } from './changes.ts';
import { BASE, applyRoute } from './router.ts';
import { renderConstituents } from './tabs/kb-tab.ts';
import { refreshChangeUi, checkStaleOnOpen } from './tabs/contribute.ts';
import { restoreTests } from './tabs/tests.ts';

// Boot progress. Each constituent contributes two steps — the fetch and the
// ingest — so the bar keeps moving across a slow download instead of sitting at
// one value for the whole of a multi-MB file. Step 1 is the engine itself.
let bootStep = 0;
let bootTotal = 1;

/** Restart the bar against a `total`-step sequence (the cache-hit path swaps in
 *  its own short, fixed sequence once it knows it has a hit). */
export function resetBootProgress(total) {
  bootStep = 0;
  bootTotal = total;
}

/** Advance the bar one step and show `label` as the quiet line beneath it. */
export function bootProgress(label) {
  bootStep += 1;
  const pct = Math.min(100, Math.round((bootStep / bootTotal) * 100));
  const fill = $('bootBarFill');
  if (fill) fill.style.width = `${pct}%`;
  $('bootBar')?.setAttribute('aria-valuenow', String(pct));
  const msg = $('overlayMsg');
  if (msg) msg.textContent = label;
}

/**
 * Show the badge from the persisted records straight away, then reconcile them
 * against upstream in the background. Not awaited: both steps are no-ops when
 * nothing is tracked, and neither should hold up a page that is already usable.
 */
function syncChanges() {
  refreshChangeUi();
  (async () => {
    await refreshProposals();
    await refreshUpstreamShas({ force: true });
    refreshChangeUi();
    // A deep link into /edit selected its file before any of this resolved, so
    // nothing could have known the file was stale at the time it was opened.
    checkStaleOnOpen(state.editCurrentFile);
  })().catch(() => { /* offline or rate-limited: the badge keeps the last known state */ });
}

export async function boot() {
  try {
    resetBootProgress(1 + state.savedConstituents.length * 2);
    $('overlayMsg').textContent = 'Starting the engine…';
    // Fetching + compiling the wasm is the longest single phase on a cold load
    // and reports no intermediate progress, so seed a visible sliver rather
    // than leaving the bar at a dead 0% for all of it.
    $('bootBarFill').style.width = '8%';
    // The worker resolves the optional Vampire runner against this; its own
    // URL sits in the bundle's asset directory, so it cannot derive the base.
    await call('boot', { baseUrl: new URL(BASE, location.href).href });
    bootProgress('Engine ready');
    state.opfsRoot = await navigator.storage.getDirectory();

    // Cache hit: the KB and every constituent's text are already restored —
    // skip the fetch+ingest+promote loop below entirely.
    if (await tryRestoreFromCache()) {
      $('overlay').remove();
      renderAll();
      applyRoute();
      restoreTests();
      syncChanges();
      return;
    }

    let i = 1;
    const total = state.savedConstituents.length;
    for (const { name, origin } of state.savedConstituents) {
      bootProgress(`Fetching ${name} (${i}/${total})`);
      const text = await fromOrigin(origin, name);
      bootProgress(`Reading ${name} (${i}/${total})`);
      await ingestConstituent(name, text, origin);   // ingest only — promote runs after
      i += 1;
    }
    $('overlay').remove();
    renderConstituents();
    refreshLangSelect();
    // Honour the URL now that the constituents exist — /edit?file=…&l=…
    // needs them loaded before it can select a file in the editor.
    applyRoute();
    restoreTests();
    syncChanges();
    reprocess();   // toast → promote all → validate → untoast (off the critical path)
  } catch (e) {
    $('overlayTitle').textContent = 'Failed to load SUMO';
    $('overlayMsg').textContent = '';
    $('overlayErr').textContent = String(e && e.message || e) + '  (Try checking your network connection.)';
    $('bootBar')?.remove();   // a stalled bar reads as "still working"
  }
}
