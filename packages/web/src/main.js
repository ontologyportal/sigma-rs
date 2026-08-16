/**
 * SUMO browser — a demo over the SDK-shaped facade, running the wasm engine in
 * a Web Worker (sigma.worker.js) so the prover never blocks the UI thread.
 *
 * Loading only INGESTS each constituent; axiomatization (promote) runs in the
 * background afterwards, during which the promote-dependent tabs (Diagnostics /
 * Ask-Tell / Audit) are greyed behind a "post-processing" toast. Search /
 * Knowledge base / Edit stay live throughout.
 *
 * Tabs:
 *   Home          — symbol search → results → man page       (tabs/browse.js)
 *   Knowledge base — manage loaded constituents              (tabs/kb-tab.js)
 *   Diagnostics   — the KB's validation findings             (tabs/diagnostics.js)
 *   Ask/Tell      — tell assertions + ask a query            (tabs/prover.js)
 *   Audit         — whole-KB consistency check               (tabs/audit.js)
 *   Edit          — in-browser Monaco IDE for KIF            (tabs/edit.js)
 *   History       — a file's upstream commit timeline        (tabs/history.js)
 *
 * The page owns the constituent list, OPFS, localStorage, and the editor; the
 * worker owns the Session. The engine comes from the `sigmakee` workspace
 * package, which Vite bundles alongside this file, so the whole demo can be
 * dropped at any path (locally, or /browse/ on GitHub Pages).
 *
 * Must be served over HTTP — browsers block ES modules + wasm fetch on file://.
 *   npm run web   # → http://localhost:8080/
 *
 * This module wires nothing itself: each import below installs its own view's
 * event handlers as a side effect, in roughly the order the page presents them.
 */

import './kb.js';
import './kb-cache.js';
import './router.js';
import './shell.js';
import './citations.js';
import './tabs/kb-tab.js';
import './tabs/tests.js';
import './tabs/browse.js';
import './tabs/diagnostics.js';
import './prover-config.js';
import './tabs/prover.js';
import './tabs/audit.js';
import './tabs/edit.js';
import './editor/tptp-pane.js';
import './tabs/contribute.js';
import './tabs/history.js';
import { boot } from './boot.js';

boot();
