/** Startup: the progress bar, the constituent fetch+ingest loop, and the
 *  hand-off to the router once the page is usable. */

import { state } from "./state.ts";
import { call } from "./rpc.ts";
import { $ } from "./dom.ts";
import { fromOrigin } from "./sources.ts";
import { fetchWordNetMappings } from "./wordnet.ts";
import {
  ingestConstituent,
  renderAll,
  refreshLangSelect,
  reprocess,
} from "./kb.ts";
import { tryRestoreFromCache } from "./kb-cache.ts";
import { refreshUpstreamShas, refreshProposals } from "./changes.ts";
import { BASE, applyRoute } from "./router.ts";
import { renderConstituents, renderWordNetPanel } from "./tabs/kb-tab.ts";
import { refreshChangeUi, checkStaleOnOpen } from "./tabs/contribute.ts";
import { restoreTests } from "./tabs/tests.ts";


// The fetched-and-parsed WordNet payload, cached for the life of the page so
// a later reinstall (session rebuild, or the KB tab's toggle switching back
// on) never re-fetches the ~23 MB of mapping text. `null` until the first
// successful fetch; never cleared by disabling (re-enabling should be
// instant when the data is already in hand).
let wordNetPayload = null;

/**
 * Fetch (or reuse the cached fetch of) the WordNet-SUMO mapping files and
 * populate `state.wordnetFiles` with their sizes for the KB tab's WordNet
 * panel. Returns the payload shape `loadWordNet` expects; throws on a fetch
 * failure (callers decide how to handle that).
 */
async function fetchWordNetPayload() {
  if (wordNetPayload) return wordNetPayload;
  const { texts, indexSense, exceptions, files } = await fetchWordNetMappings();
  const byPos = Object.fromEntries(texts.map(([text, pos]) => [pos, text]));
  wordNetPayload = {
    noun: byPos.noun,
    verb: byPos.verb,
    adj: byPos.adj,
    adv: byPos.adv,
    indexSense,
    exceptions,
  };
  state.wordnetFiles = files;
  return wordNetPayload;
}

/**
 * Fetch + install the WordNet-SUMO lexicon into the worker's session,
 * contributing exactly two steps to whichever boot-progress sequence is
 * current (own portion of the bar, split fetch/load like a KIF constituent)
 * -- so only call this where those two steps were reserved (both boot
 * paths, fresh and cache-restore; see their own `resetBootProgress` calls).
 * Use [`reinstallWordNetIfEnabled`] instead anywhere else (a session
 * rebuild, or the KB tab's enable toggle), which does the same fetch/install
 * without the boot-progress bookkeeping.
 *
 * A no-op (after still advancing both progress steps, so the reserved
 * budget is never left short) when [`state.wordnetEnabled`] is off.
 * Best-effort otherwise: WordNet synonym expansion is an optional search
 * enhancement, never a hard dependency of a usable KB, so a fetch/install
 * failure is logged and swallowed rather than failing the whole boot.
 */
export async function loadWordNetIntoWorker() {
  bootProgress("Fetching WordNet lexicon…");
  let payload = null;
  if (state.wordnetEnabled) {
    try {
      payload = await fetchWordNetPayload();
    } catch (e) {
      console.warn("WordNet lexicon fetch failed:", e);
    }
  }
  bootProgress("Loading WordNet lexicon…");
  if (payload) {
    try {
      await call("loadWordNet", payload);
    } catch (e) {
      console.warn("WordNet lexicon install failed:", e);
    }
  }
}

/**
 * Re-fetch (or reuse the cache) and reinstall WordNet into the worker's
 * CURRENT session, without touching the boot-progress bar -- for a session
 * rebuild (`kb.ts`'s `rebuildSession`, which replaces the worker's `Session`
 * and so drops any previously loaded lexicon) or the KB tab's toggle
 * switching back on. A no-op when [`state.wordnetEnabled`] is off.
 */
export async function reinstallWordNetIfEnabled() {
  if (!state.wordnetEnabled) return;
  try {
    const payload = await fetchWordNetPayload();
    await call("loadWordNet", payload);
  } catch (e) {
    console.warn("WordNet lexicon reinstall failed:", e);
  }
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
  })().catch(() => {
    /* offline or rate-limited: the badge keeps the last known state */
  });
}

export async function boot() {
  
}
