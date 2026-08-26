/**
 * SUMO browser — a demo over the SDK-shaped facade, running the wasm engine in
 * a Web Worker (sigma.worker.ts) so the prover never blocks the UI thread.
 *
 * Loading only INGESTS each constituent; axiomatization (promote) runs in the
 * background afterwards, during which the promote-dependent tabs (Diagnostics /
 * Ask-Tell / Audit) are greyed behind a "post-processing" toast. Search /
 * Knowledge base / Edit stay live throughout.
 *
 * Tabs:
 *   Home          — symbol search → results → man page       (tabs/browse.ts)
 *   Knowledge base — manage loaded constituents              (tabs/kb-tab.ts)
 *   Diagnostics   — the KB's validation findings             (tabs/diagnostics.ts)
 *   Ask/Tell      — tell assertions + ask a query            (tabs/prover.ts)
 *   Audit         — whole-KB consistency check               (tabs/audit.ts)
 *   Edit          — in-browser Monaco IDE for KIF            (tabs/edit.ts)
 *   History       — a file's upstream commit timeline        (tabs/history.ts)
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

import './kb.ts';
import './kb-cache.ts';
import './router.ts';
import './shell.ts';
import './auth.ts';
import './citations.ts';
import './tabs/kb-tab.ts';
import './tabs/tests.ts';
import './tabs/browse.ts';
import './tabs/diagnostics.ts';
import './prover-config.ts';
import './tabs/prover.ts';
import './tabs/audit.ts';
import './tabs/edit.ts';
import './editor/tptp-pane.ts';
import './tabs/contribute.ts';
import './tabs/history.ts';
import { boot } from './boot.ts';

boot();
