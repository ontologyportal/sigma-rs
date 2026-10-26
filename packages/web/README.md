# SigmaKEE Web Application

**COMING SOON**

## Editor autosave

Named files save locally after 5 seconds without typing. Each edit restarts
that delay, and unchanged text is not saved again. Switching files or leaving
the Edit tab saves pending edits immediately. File switching waits for the save,
including edits made while a save is running; a failed save keeps the buffer open.
Save errors appear below the toolbar. Imported files keep local edits until you
explicitly push them; autosave does not publish changes.

New unnamed files need **Save As** once to choose a filename; later edits autosave. Switching away asks before
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

## Recorded contradictions and contradiction sources

Every contradiction an audit finds is recorded by the engine, keyed by the
axioms its derivation cites. A recorded contradiction stays valid as long as
every cited axiom is still in the knowledge base (first-order logic is
monotone: adding axioms cannot undo a refutation) and is dropped the moment one
of them is edited or unloaded. The Audit tab's **Recorded contradictions** card
lists them with their origin and the time they were found; **Show proof**
expands the derivation and **Replay** audits the cited axioms together to
re-derive the contradiction against the current KB. The list is mirrored to
OPFS and re-imported after every boot, so it survives reloads; a finding whose
axioms changed meanwhile fails to re-resolve and does not come back.

Contradictions reported elsewhere arrive through **Contradictions** on the
Knowledge base tab, which lists _contradiction sources_ alongside the
constituent sources:

- a **GitHub issue tracker** whose open issues titled `[Contradiction]` embed a
  version 3 proof report (what **Report proof to GitHub** files);
- a **report URL** serving the nightly `latest-contradictions.json`, a shared
  version 3 report, or `sumo audit --json` output;
- a **one-shot upload** of such a report, listed afterwards for provenance.

The SUMO nightly report and the `ontologyportal/sumo` tracker are watched by
default. Each source carries the same update preference as a constituent
source: **Auto-update** imports new contradictions on every load,
**Auto-check** alerts and offers an **Import** button instead, **No check**
leaves it alone, and **Check now** does it immediately. A reported
contradiction is recorded only when every axiom it cites resolves to a
sentence of this KB (by its text, else by file and line); the rest are counted
as not applicable. New findings surface in the same sources-changed dialog the
constituent updates use, and link to the Audit tab.

The engine replaces the old whole-KB replay: a report's contradiction is no
longer re-run against the pinned nightly inputs but recorded against your own
KB and replayed on it. Audit reports confirmed in Ask/Tell's **Import proof
report** dialog follow the same path.

Regression tests (no GitHub writes or engine):

```bash
node --test packages/web/src/stores/contradictions.test.mjs packages/web/src/views/AuditTab.test.mjs
```

After building the CLI and WASM package, the real-engine smoke tests still
exercise the engine-level replay and recheck seams:

```bash
node packages/web/e2e/audit-replay-engine.mjs
node packages/web/e2e/audit-recheck-engine.mjs
```

The CLI's counterpart is `sumo audit --import <PATH|URL>`, which records a
report's applicable contradictions in the loaded KB (persisted with the LMDB
store), replays each one and prints its derivation.

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

## Sharing proof results

Use **Share result** on an Ask/Tell result or an audit contradiction, then
**Copy report** or **Download report**. Sharing checks every loaded constituent
against the live HEAD of its GitHub branch, using the exact text captured for
that proof (including saved and unsaved edits). It reads the branch again after
verification so a concurrent push cannot silently change the pinned revision.
If any file differs, push your changes or reload the branch and rerun the proof.
Local uploads, URL files and detached revisions must be loaded from a pushed
GitHub branch before they can be shared. Sharing never publishes local work.

Use **Report proof to GitHub** beside a proof to create an issue in
`ontologyportal/sumo`. After the same branch verification, enter a title and
description and sign in to submit it. Proof issues have a `[Proof]` title prefix;
audit contradictions have `[Contradiction]`. The issue body embeds the compact
version 3 JSON report. GitHub's issue size limit can prevent very large proofs
from being posted; the JSON report can still be copied or downloaded.

Version 3 reports contain repository/branch/commit references, ordered file
hashes, cited source axioms, the exact problem and settings, engine/prover build
identities, and the selected proof. Complete KIF files and bulk external prover
input/output are not included.

To reproduce a conjecture proof, choose **Import proof report** on Ask/Tell.
Review the source revisions, operation, settings and build compatibility.
Confirm replacing your KB only after saving a separate copy of work you want
to keep: replay discards edits to loaded and imported files. Opening or
cancelling the dialog does not change your workspace.

The importer downloads files at their pinned commits, checks all hashes and
parses the inputs before replacing the KB. It restores prover settings, reruns
the recorded problem, and displays the new proof and reproduction outcome.
An audit report confirmed there is not replayed against pinned inputs: its
contradiction is recorded against your own KB (when every cited axiom is
present) and replayed on the Audit tab -- see "Recorded contradictions and
contradiction sources" above, which also covers `[Contradiction]` issues and
report URLs as watched sources.
Timeouts are inconclusive. A branch advancing after sharing does not invalidate
the report. The engine fingerprint and external prover binaries must match;
missing/incompatible builds block import before replacement. Files must be
publicly downloadable from GitHub at the recorded commit. Older version 1/2
reports lack verified inputs and must be shared again with this version.

Sharing regression checks:

```bash
node --test packages/web/src/utils/proofReport.test.mjs packages/web/src/services/proof-report.test.mjs
node packages/web/e2e/share-proof.mjs
```
