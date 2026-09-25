# SigmaKEE Web Application

## E in Ask/Tell and Audit

Open prover settings and choose **E (WASM)**. Set **axiom target** to a
positive count to select relevant query premises without automatic widening;
0 keeps the percentage/default selection. Required assertions and seed axioms
are retained, so the target is not a hard size cap.

For Audit, enable **Audit e_axfilter subsets** to generate symbol-seeded,
overlapping subsets, then run E on each. Set the maximum audit subsets,
filter time limit, prover time limit (per subset), and maximum contradictions.
Repeated proofs with the same supporting premises are deduplicated. Proofs
retain links to their original KB sentences. Results report attempted and
pending subsets; a run is not resumable across button clicks.

Finding a contradiction establishes inconsistency. Finding none in selected
subsets does not establish whole-KB consistency or exhaustive coverage, even
with a long timeout. Without subset mode, an unscoped audit checks the entire
KB; the axiom target only affects queries and focused audits.

Build the browser assets with emsdk activated:

```bash
npm run build --workspace @sigma/eprover
npm run web
```

The web startup mirrors E assets automatically and attempts an optional E
build. `SKIP_EPROVER=1` skips that build and uses existing assets if available.
Cargo builds native E executables; the browser assets need the separate
Emscripten build. Cross-origin isolation is required, as for Vampire.

Regression checks:

```bash
npm test --workspace @sigma/eprover
npm run test:e2e:eprover --workspace @sigma/web
```

The browser test requires Playwright Chromium (`npx playwright install chromium`).

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
