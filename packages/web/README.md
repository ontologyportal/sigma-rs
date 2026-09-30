# SigmaKEE Web Application

**COMING SOON**

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

In Audit, open **Latest master contradiction report** to inspect the latest
completed master run. A finding makes the workflow fail, so failed runs are
included. Choose **Confirm: replace work and replay** only after saving any
local work you want to keep. It replaces the loaded KB, discards conflicting
saved edits, restores the workflow's prover settings, and runs only the
reported seed/step positions. The result compares cited axiom sets with the
report and explicitly identifies missing or additional contradictions.

The SUMO workflow publishes the same `contradictions.md` as both an Actions
artifact and `.github/latest-contradictions.md` on its `audit-state` branch.
The public copy avoids requiring an Actions artifact download token. Its
`sigma-audit-replay` block records the run/attempt, SUMO commit, ordered
constituents and SHA-256 hashes, engine source fingerprint, settings, and
findings. Reports from older workflows without this block cannot be replayed.

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
```

For an isolated visual preview of the confirmation flow (no real saved work
or GitHub requests), run `node packages/web/e2e/audit-replay-preview.mjs` and
open the printed address. Add `?stale=1` to inspect rejection of an old report.
