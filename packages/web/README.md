# SigmaKEE Web Application

**COMING SOON**

## Editor autosave

Named files save locally after 5 seconds without typing. Each edit restarts
that delay, and unchanged text is not saved again. Switching files or leaving
the Edit tab saves pending edits immediately. File switching waits for the save,
including edits made while a save is running; a failed save keeps the buffer open.
Save errors appear below the toolbar. Imported files keep local edits until you
explicitly push them; autosave does not publish changes.

New unnamed files need an explicit first save. Switching away asks before
discarding their text. Closing or reloading the page warns while edits remain
unsaved or a save is still running. Browser shutdown and crashes can still
interrupt a save; the idle delay is not a crash recovery guarantee.

## Deployment disconnection

Production builds cache tab code, styles, the editor, and core engine assets
in the background after the app opens. Vampire, E (including e_axfilter), and
Cytoscape graph libraries download and cache only when used. A status line
shows when editing disconnect protection is ready. Background downloads run one at a time
at low priority; opening a tab uses its normal on-demand load without waiting for
the background queue. Unchanged hashed assets are reused across deployments.
If caching fails (for example, storage quota or a missing asset), a banner warns
that some features still need the deployment server; the app remains usable.
Optional prover assets are available only when those backends were built.

Once editing disconnect protection is ready, the open page can switch tabs,
edit, and save locally even when the deployment server stops. Optional tools
need a connection on first use; after their assets are cached, they also work
when the server stops. Saving work uses browser storage and does not require
downloading prover binaries. GitHub requests, login,
remote file downloads, and update checks still need their respective services.
This is feature-asset caching, not an offline page-reload or data-sync mode.

Caching requires HTTPS or localhost and runs only in a production build, including
one served locally with `npm run preview --workspace @sigma/web`. Vite development
mode keeps its normal live module loading. Close existing production tabs before
using a development server at the same origin; use a separate port for development.
Updated caches install in the background and activate after existing tabs close.
The first load does not download optional prover binaries or graph libraries.
The optional WordNet lexicon (about 23 MB of mapping text) also stays off the
startup path. It loads on the first search, a WordNet view, or an explicit
**Load WordNet now** action, and its payload is reused for the rest of the session.
Opening the editor or saving files does not download WordNet.

Run `npm run test:asset-cache --workspace @sigma/web` for lifecycle tests. After
building, `npm run test:e2e:asset-cache --workspace @sigma/web` starts an isolated
production server, stalls a background download, verifies that startup and a clicked
tab still complete, and checks that optional tools have not been downloaded.
It exercises online first use, then disables the HTTP cache, stops the server,
and exercises tabs, local editing/saving, cached optional tools, and worker recovery. It requires
Playwright Chromium and both optional prover builds. The same test supports
builds configured with `VITE_BASE=/browse/`.

## E prover in the browser

Open the gear beside **Prove** or **Run audit**, then choose **E** as the
backend. E runs locally in a dedicated WASM worker. Proofs include source
citations and the exact TPTP input can be downloaded from Ask/Tell.

With an activated Emscripten SDK, build from the repository root:

```bash
npm install
npm run build --workspace @sigma/eprover
npm test --workspace @sigma/eprover
SKIP_VAMPIRE=1 npm run build --workspace @sigma/web
```

The web build rebuilds sigmakee and mirrors E's generated assets into
`public/eprover/`. Do not commit generated binaries or upstream caches.
`SKIP_EPROVER=1` skips recompilation and reuses complete existing E output;
Local startup also reuses complete existing output if recompilation fails
(for example, because emsdk is not activated), with an explicit warning.
Without those files, selecting E reports that its assets are unavailable.
Serve with the existing COOP/COEP headers: external prover workers require
cross-origin isolation and SharedArrayBuffer. Restart the server and reload
after rebuilding WASM.

An optional **Audit e_axfilter subsets** setting checks smaller subsets within
each sampled audit neighborhood. No contradiction found does not establish
whole-KB consistency. Its filter deadline and per-subset prover deadline are
separate; timed-out workers are replaced so subsequent queries can run.
Verified nightly-report replay and rechecks remain pinned to the report's
SUPr configuration, independent of the currently selected backend.

After mirroring the assets, run `npm run test:e2e:eprover --workspace @sigma/web`
for real-browser coverage (requires Playwright Chromium and Vampire assets for
the backend-switch regression). The isolated `e2e/eprover.html` page also
provides a manual smoke-test button without loading or changing saved SUMO work.

## 3D hierarchy

Under Explore, choose the **Visualize** tab. Entity starts at the center;
terms are shaded spheres connected by subclass, instance, subrelation, and
subAttribute assertions. Larger visible subtrees receive proportionally more
angular space, and successive branches continue outward. Multiple parents
remain connected even though positioning uses a cycle-safe spanning tree.

Drag to orbit, scroll or use the zoom buttons to zoom, and Shift-drag to pan.
With the canvas focused, arrow keys rotate, +/- zoom, and Home resets the view.
Click a sphere or use the searchable term list to center the view on it, inspect its
parents and children, and open its term page. Rotation and zoom stay unchanged
when focusing a term; subsequent rotation orbits that term. Reset view returns
to Entity. Relationship checkboxes filter
the reachable graph; labels can be hidden.

The default view loads four levels from the current knowledge base. Choose
more levels or All for deeper exploration. Views are limited to 6,000 terms
and explicitly marked when partial. Branch weights describe the visible
subtree, not a count of unloaded descendants. Reload refreshes the view;
knowledge-base reprocessing also reloads it automatically.

Run the layout and traversal tests from the repository root:

```bash
node --test packages/web/src/services/taxonomy-3d.test.mjs
```

## Replaying the master contradiction audit

In Audit, open **Latest Contradiction Report** to inspect the latest
completed master run. A finding makes the workflow fail, so failed runs are
included. Choose **Confirm: replace work and replay** only after saving any
local work you want to keep. It replaces the loaded KB, discards conflicting
saved edits, restores the workflow's prover settings, and runs only the
reported seed/step positions. The result compares cited axiom sets with the
report and explicitly identifies missing or additional contradictions.

After loading a report, **Recheck reported contradictions** tests edits against
the same source targets without downloading inputs, resetting settings, or
discarding saved or live editor changes. The original workflow settings are
used for each recheck. Targets follow source-formula slots, not the old sweep
positions: changing formula content can reorder the seeded sweep.

Tracking lasts for the current app/worker session, including navigation to the
editor and back. A refresh, worker restart, or KB replacement requires loading
a report again. Added or removed formulas permanently invalidate the session's
steps, even if subsequently undone. Reordered or duplicated targets and ambiguous
multi-formula replacements are rejected; make edits to one existing formula at
a time. Temporary parse errors block rechecking until corrected. An edited target
that expands into multiple normalized formulas cannot be safely remapped.

Results distinguish targets that still produce contradictions, those that no
longer reproduce a contradiction, and inconclusive checks (including timeouts).
They do not certify that the entire KB is consistent. Rechecking a previously
loaded, pinned report does not synchronize to a newer master commit.

The SUMO workflow publishes the same `contradictions.json` as both an Actions
artifact and `.github/latest-contradictions.json` on its `audit-state` branch.
The public copy avoids requiring an Actions artifact download token. The
versioned JSON document records the run/attempt, SUMO commit, ordered
constituents and SHA-256 hashes, engine source fingerprint, settings, and all
distinct findings accumulated while those SUMO and engine inputs remain
unchanged. Markdown reports from older workflows cannot be replayed.

Replay checks live master before download, before replacing work, after
loading, and after execution. Any master commit change invalidates the report,
even if the constituent files did not change. Inputs are fetched by immutable
commit, checked against that commit's `.github/full-sumo.txt`, and hashed before
any saved work is replaced. The loaded constituents stay pinned to that commit.

Build `sigmakee` before deploying this feature: its build writes
`dist/build-info.json`, which Vite embeds to identify the actual bundled WASM
(including when `NO_REBUILD=1` reuses it). The workflow's pinned sigma-rs revision
must have matching `Cargo.toml`, `Cargo.lock`, `.cargo/` configuration, and tracked `crates/` sources.
Unknown or different engine inputs disable replay. Browser/native time and
memory limits can still differ; a mismatch is reported rather than presented
as successful reproduction. A new completed master audit is needed after the
workflow changes are deployed.

Replay regression tests (no GitHub writes or real KB replacement):

```bash
node --test packages/web/src/utils/auditReplay.test.mjs packages/web/src/services/audit-replay.test.mjs packages/web/src/views/AuditTab.test.mjs
```

After building the CLI and WASM package, a real-engine smoke test checks that
the same synthetic contradiction is found at the same seed and step:

```bash
node packages/web/e2e/audit-replay-engine.mjs
node packages/web/e2e/audit-recheck-engine.mjs
```

For an isolated visual preview of the confirmation flow (no real saved work
or GitHub requests), run `node packages/web/e2e/audit-replay-preview.mjs` and
open the printed address. Add `?stale=1` to inspect rejection of an old report.

## Working on a SUMO branch

In **Knowledge base > Constituents**, click **Branch: master** (or the current
branch name) to open the branch dropdown. Long names are truncated in the
button; hover over it to see the full name. Search the published branches and
select one to load the current file set from that branch. If the KB is empty
or contains multiple sources, selection loads Minimal SUMO instead.

To use your fork, expand **Use another repository or fork**, enter
`your-username/sumo`, and click **Show branches**. The account before `/` is
the GitHub user or organization that owns the repository; the name after `/`
is the repository. The standard-set menu also uses the selected branch.

The app confirms before replacing the entire KB and discarding local edits.
Switching also replaces the library's repository catalog, so files from the
previous branch disappear from the constituent list, including after reload.
All files are downloaded from one commit before replacement starts; a missing
branch or failed download leaves the current KB intact. Loaded constituents
retain the selected repository and branch across browser restarts. Source
update preferences still determine whether subsequent upstream commits are
automatically loaded.

Public branches require no login (signing in can raise GitHub's API limit).
Create and push your branch and commits with Git, or use the existing web
contribution flow to publish edits as a branch and pull request. On another
computer, enter that branch's owner, repository, and name. Browser-local
edits and Git branches that have not been pushed are not available elsewhere.
The web loader reads public raw GitHub files; signing in does not add support
for loading private repository contents through this path.

Branch-loading regression checks:

```bash
node --test packages/web/src/stores/kb.test.mjs packages/web/src/services/sources.test.mjs packages/web/src/views/KbTab.test.mjs
node packages/web/e2e/branches.mjs
```


## Pushing saved edits

In the editor, save your files, then open the GitHub panel. Select the files
and use **Push to BRANCH** to commit them directly to their loaded source
repository and branch. The destination is shown above the title field.
Sign in with GitHub and use a token with repository Contents write access.
Direct push requires write access to that repository; select your fork if
you cannot push to upstream SUMO.

One push is one commit. Files from different branches cannot be pushed
together. The app checks the edited files against the remote branch and
refuses to overwrite newer remote edits. Concurrent pushes and branch
protection failures are reported without clearing your local edits.

**Create pull request** keeps the existing contribution workflow. After a
direct push, **Create pull request from this branch** opens a PR against
upstream SUMO, or links to an existing open PR for that branch.

```bash
node --test packages/web/src/api/push.test.mjs packages/web/src/components/edit/ContributePanel.test.mjs packages/web/src/stores/changes.test.mjs
node packages/web/e2e/push.mjs
```
