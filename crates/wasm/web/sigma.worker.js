// Web Worker host for the sigmakee wasm engine. Keeps the synchronous prover
// (ingest / promote / ask / audit / validate) off the UI thread. Owns only the
// Session; the page (app.js) owns the constituent list, OPFS, localStorage, and
// the editor, and drives this worker over a tiny id-keyed RPC.

// The SDK bindings (`init`/`Session`/`Config`/`Backend`/`parseTest`) are
// resolved at boot() rather than statically imported: which bundle to load
// (single-threaded `./pkg/` vs. threads-enabled `./pkg-threaded/`) is a
// runtime decision (see `loadEngine` below), and a static `import` can't be
// conditional. Every other handler in this file reaches the engine through
// these module-level bindings once `boot()` has resolved them.
let init, Session, Config, Backend, parseTest;
let session = null;
// Surfaced to the page (via boot()'s return) so the UI can show whether the
// threaded backend actually came up, vs. silently running single-threaded.
let threaded = false;

function newSession() {
  return new Session({ backend: Backend.Native, config: makeConfig() });
}

// Threads need cross-origin isolation (COOP/COEP — see serve.sh and, for
// deployed Pages, its follow-up slice) for `SharedArrayBuffer`, which is how
// wasm-bindgen-rayon's worker pool shares the wasm heap across workers.
// Plain deploys — and any deploy that never shipped `pkg-threaded/` — are
// never crossOriginIsolated, so this is a zero-cost check for them: it's
// `false` and the plain `./pkg/` path runs exactly as before.
//
// The threaded bundle is a SEPARATE wasm-bindgen output (own .wasm, own
// generated JS, own `sdk.mjs` copy — see build-npm.sh's THREADED=1 variant),
// not a runtime flag on the single-threaded one, so switching bundles means
// switching which module URL every binding comes from.
async function loadEngine(wantThreads) {
  // Opt-in only (`boot({threads: N})`): measured on SUMO-scale KBs, the
  // threaded engine is a net LOSS — every parallel hot path is
  // allocation-heavy and wasm's dlmalloc funnels all workers through one
  // global lock, so a 16-thread pool ran validate ~1.6x slower and TPTP
  // export ~5x slower than the plain bundle. Until wasm gets a scalable
  // allocator (or the hot paths shed their allocations), the plain bundle
  // is the right default even on cross-origin-isolated deploys.
  const canThread = wantThreads > 0
    && self.crossOriginIsolated === true && typeof SharedArrayBuffer !== 'undefined';
  if (canThread) {
    try {
      const [sdk, raw] = await Promise.all([
        import('./pkg-threaded/sdk.mjs'),
        // The raw wasm-bindgen module, imported separately only for
        // `initThreadPool` — sdk.mjs doesn't re-export it (that file is
        // BYTE-IDENTICAL between pkg/ and pkg-threaded/, copied verbatim by
        // build-npm.sh, and the plain build's generated module has no such
        // export). Both imports resolve to the same pkg-threaded/ wasm
        // instance (module specifiers dedupe by resolved URL), so
        // `raw.initThreadPool` operates on the module `sdk.init()` below
        // instantiates.
        import('./pkg-threaded/sumo_parser_wasm.js'),
      ]);
      await sdk.init();
      await raw.initThreadPool(Math.min(wantThreads, navigator.hardwareConcurrency || 4));
      ({ Session, Config, Backend, parseTest } = sdk);
      init = sdk.init;
      threaded = true;
      return;
    } catch (err) {
      // pkg-threaded/ absent from this deploy, nightly-only build skipped,
      // pool init failed, etc. — fall back to the plain bundle exactly like
      // loadVampireRunner() falls back off the Vampire backend below.
      threaded = false;
    }
  }
  const sdk = await import('./pkg/sdk.mjs');
  ({ Session, Config, Backend, parseTest } = sdk);
  init = sdk.init;
}

// Build a wasm `Config` from a plain settings object (the Ask/Tell settings
// menu). Only keys actually supplied are applied, so the Rust-side defaults
// stand for anything the UI leaves blank. `wantProof` defaults on — the demo
// always wants the proof/graph/prose.
function makeConfig(o = {}) {
  const cfg = new Config();
  cfg.wantProof = o.wantProof !== undefined ? !!o.wantProof : true;
  if (o.timeLimitSecs != null) cfg.timeLimitSecs = o.timeLimitSecs;
  if (o.maxSteps     != null) cfg.maxSteps     = o.maxSteps;
  if (o.maxLits      != null) cfg.maxLits      = o.maxLits;
  if (o.forwardClose != null) cfg.forwardClose = !!o.forwardClose;
  if (o.profile      != null) cfg.profile      = !!o.profile;
  // 0 means "engine default" (see app.js's CFG_KNOBS) — leave the wasm
  // Config field unset (its own default is `None`, same effect) rather than
  // pass a literal 0% budget. 100 searches the whole KB.
  if (o.selectionTolerancePct) cfg.selectionTolerancePct = o.selectionTolerancePct;
  return cfg;
}

const handlers = {
  async boot({ threads } = {}) {
    const bootThreads = threads | 0;
    await loadEngine(bootThreads);
    await init();
    session = newSession();
    // The KB's `plan_threads` gates read `CacheConfig.max_threads`, which
    // defaults to `available_parallelism()` — always 1 on wasm32 regardless
    // of pool size — so without this every gate would serialize even against
    // a live N-worker rayon pool. Only the native-prover session exposes
    // `setMaxThreads`; a no-op on a bundle with no pool to size for.
    if (threaded && session.kb && typeof session.kb.setMaxThreads === 'function') {
      session.kb.setMaxThreads(Math.min(bootThreads, navigator.hardwareConcurrency || 4));
    }
    return { ok: true, threaded };
  },
  // Drop the session and start fresh (the page re-ingests every constituent).
  newSession() {
    session = newSession();
    return { ok: true };
  },

  // Ingest one constituent WITHOUT promoting; the page promotes later.
  ingest({ name, text }) {
    return { notices: session.kb.ingest(text, name) };
  },
  // Promote a batch of ingested constituents into the axiom base (the deferred,
  // heavier step). Already-promoted names are a fast no-op in core.
  promoteAll({ names }) {
    for (const n of names) session.kb.promote(n);
    return { ok: true };
  },

  validate() { return { diagnostics: session.validate() }; },
  stats() { return { stats: session.kb.stats() }; },

  // Freeze/thaw seam for the page's OPFS boot cache (see app.js's
  // tryRestoreFromCache/saveKbCache) — native backend only, per the SDK doc.
  snapshot() { return { bytes: session.snapshot() }; },
  restore({ bytes }) { session.restore(bytes); return { ok: true }; },

  /**
   * Revalidate an edited constituent with FULL KB context by diffing the buffer
   * into its own session and committing it — the live KB tracks the editor.
   * Symbols resolve against the real KB, so semantic diagnostics are meaningful.
   */
  validateBuffer({ file, text }) {
    return { diagnostics: session.kb.validateBuffer(file, text) };
  },

  /**
   * Validate scratch input (the Ask/Tell box, or an editor buffer with no
   * backing file) in a THROWAWAY session — never the live KB. That session has
   * no SUMO loaded, so every symbol reference reads "unknown"; only `parse`
   * diagnostics are meaningful without context, so the rest are dropped.
   */
  validateFormula({ kif }) {
    const diagnostics = newSession().validateFormula(kif)
      .filter((d) => d.kind === 'parse');
    return { diagnostics };
  },

  // Ask/Tell live validation: assertions + query in one scratch session
  // against the LIVE KB, so symbol references resolve and the assertions'
  // own declarations are in scope for the query.
  validateScratch({ assertions, query }) {
    return session.kb.validateScratch(assertions || '', query || '');
  },

  parseTest({ name, text }) { return { test: parseTest(name, text) }; },

  // Whole-KB TPTP dump (Edit tab's split TPTP pane). `toTptpIndexed` is the
  // heavy call (re-translates every axiom) — the page only fires it on the
  // edit-validate debounce, not per keystroke. `tptpLineForPosition` is cheap
  // (cache lookups against the last generated dump) and safe to call on
  // every cursor move.
  toTptpIndexed({ lang, hideNumbers }) {
    return { text: session.kb.toTptpIndexed(lang, hideNumbers) };
  },
  tptpLineForPosition({ file, offset }) {
    return { line: session.kb.tptpLineForPosition(file, offset) ?? null };
  },
  search({ query, limit, language }) { return { hits: session.search(query, { limit: limit ?? 100, language }) }; },
  manpage({ symbol }) { return { page: session.manpage(symbol) }; },
  taxonomy({ symbol }) { return { tax: session.taxonomy(symbol) }; },
  naturalLanguages() { return { languages: session.naturalLanguages() }; },
  renderNl({ kif, language }) { return { text: session.renderNl(kif, language) }; },

  prove({ assertions, query, config, session: sess }) {
    session.configure(makeConfig(config));
    const tag = sess || 'user-assertions';
    session.flushSession(tag);
    if (assertions && assertions.trim()) {
      const t = session.tell(assertions, tag);
      if (!t.ok) throw new Error('assertion parse errors: ' + t.errors.slice(0, 3).join('; '));
    }
    return { result: session.ask(query, { session: tag }) };
  },

  audit({ config, limit }) {
    session.configure(makeConfig(config));
    return { result: session.auditConsistency(limit ?? 5) };
  },

  // -- Vampire (WASM) backend — an alternative to the native in-browser
  // prover, built by build-vampire.sh (gitignored; not present unless that
  // script has run). Bare-bones: runs Vampire as a subprocess-shaped CLI
  // over a self-contained TPTP problem and surfaces only its SZS status +
  // raw output — no structured proof/graph/prose (that's the native path's
  // job; Vampire's own proof text isn't parsed into this app's step shape
  // yet). Both handlers build a `{status, ..., raw_output}` object shaped
  // like the native `prove`/`audit` results, so they render through the
  // SAME `renderProof`/`renderAudit` UI code unchanged.
  // `selectionTolerancePct` mirrors the native backend's `Config.
  // selectionTolerancePct` (see makeConfig) — same slider, same meaning,
  // both backends: a percentage of the KB's axioms a query-relevant
  // selection may admit, `null`/0 for the engine default, 100 for the whole
  // KB. Unlike the native backend (which can widen its selection and retry
  // when saturation doesn't close), Vampire's selection is a ONE-SHOT
  // budget: too low a percentage can under-select and cause a false
  // Disproved/Unknown on a query the KB actually proves.
  // Audit has no query to seed relevance from, so it's unaffected — it
  // already always searches the whole KB either way (native or Vampire).
  proveVampire({ assertions, query, timeLimitSecs, selectionTolerancePct }) {
    const tptp = session.kb.toTptpForAsk(assertions || '', query, false, selectionTolerancePct || null);
    return runVampireProblem(tptp, timeLimitSecs, ASK_SZS_TO_STATUS, false);
  },
  auditVampire({ timeLimitSecs }) {
    const tptp = session.kb.toTptpIndexed(undefined, true);
    return runVampireProblem(tptp, timeLimitSecs, AUDIT_SZS_TO_STATUS, true);
  },
};

// Lazy: only fetched/instantiated the first time a Vampire-backed handler
// runs, so a demo that never touches this backend (or a deploy where
// build-vampire.sh was skipped) never pays for it.
let vampireRunnerPromise = null;
function loadVampireRunner() {
  if (!vampireRunnerPromise) {
    // Don't cache a rejection: a deploy missing web/vampire/ (build-vampire.sh
    // wasn't run) fails every call identically, but a later redeploy — or a
    // transient network blip fetching the chunk — should get a fresh attempt
    // instead of being stuck replaying the first failure forever.
    vampireRunnerPromise = import('./vampire/vampire-runner.js').catch(() => {
      vampireRunnerPromise = null;
      throw new Error(
        'The Vampire (WASM) backend is not included in this deployment ' +
        '(build-vampire.sh output missing). Use the Native backend instead.'
      );
    });
  }
  return vampireRunnerPromise;
}

function parseSzsStatus(text) {
  const m = /%\s*SZS status (\w+)/.exec(text || '');
  return m ? m[1] : null;
}

// Vampire's SZS ontology -> this app's status vocabulary (see index.html's
// `.status.<Name>` CSS and renderProof/renderAudit in app.js). Two separate
// maps because the SAME SZS status means opposite things depending on
// whether Vampire was given a conjecture (Ask/Tell) or not (Audit) — e.g.
// "Satisfiable" is a DISPROOF of a conjecture, but a CONSISTENCY verdict
// for the bare axioms.
const ASK_SZS_TO_STATUS = {
  Theorem: 'Proved', ContradictoryAxioms: 'Proved',
  CounterSatisfiable: 'Disproved', Satisfiable: 'Disproved',
  Timeout: 'Timeout',
  GaveUp: 'Unknown', Unknown: 'Unknown', ResourceOut: 'Unknown',
};
const AUDIT_SZS_TO_STATUS = {
  Unsatisfiable: 'Inconsistent', ContradictoryAxioms: 'Inconsistent',
  Satisfiable: 'Consistent', CounterSatisfiable: 'Consistent',
  Timeout: 'Unknown',
  GaveUp: 'Unknown', Unknown: 'Unknown', ResourceOut: 'Unknown',
};

async function runVampireProblem(tptp, timeLimitSecs, szsMap, isAudit) {
  const { runVampire } = await loadVampireRunner();
  const t = Math.max(1, Math.floor(Number(timeLimitSecs)) || 30);
  const res = await runVampire(tptp, `--input_syntax tptp -t ${t}`, {});
  const szs = parseSzsStatus(res.stdout) || parseSzsStatus(res.stderr);
  const status = (szs && szsMap[szs]) || 'Unknown';
  const raw_output = res.stdout + (res.stderr ? '\n' + res.stderr : '');
  return {
    result: isAudit
      ? { status, given_steps: null, inconsistent: status === 'Inconsistent', contradictions: [], raw_output }
      : { status, given_steps: null, proof: [], raw_output, graphviz: null, prose: null, prose_missing: null },
  };
}

self.onmessage = async (e) => {
  const { id, cmd, args } = e.data;
  try {
    const fn = handlers[cmd];
    if (!fn) throw new Error(`unknown cmd: ${cmd}`);
    self.postMessage({ id, result: await fn(args || {}) });
  } catch (err) {
    self.postMessage({ id, error: String(err && err.message || err) });
  }
};
