// Web Worker host for the sigmakee wasm engine. Keeps the synchronous prover
// (ingest / promote / ask / audit / validate) off the UI thread. Owns only the
// Session; the page (src/main.ts and friends) owns the constituent list, OPFS, localStorage, and
// the editor, and drives this worker over a tiny id-keyed RPC.

import { init, Session, Config, Backend, parseTest } from 'sigmakee/sdk';

// Not imported from prover-config.ts: that file is DOM code (this worker has
// no `document`/`window`), and even a type-only import pulls the whole file
// into this program's compilation graph under the worker's lib settings.
// Duplicated to match the wire shape postMessage actually carries -- worker
// and page are separate runtimes, never sharing memory.
interface ProverConfig {
  timeLimitSecs?: number;
  maxSteps?: number;
  maxLits?: number;
  forwardClose?: boolean;
  wantProof?: boolean;
  profile?: boolean;
  selectionTolerancePct?: number;
}

let session = null;
// Site base URL, supplied by the page at boot(); see loadVampireRunner().
let siteBaseUrl = self.location.href;

function newSession() {
  return new Session({ backend: Backend.Native, config: makeConfig() });
}

// Build a wasm `Config` from a plain settings object (the Ask/Tell settings
// menu). Only keys actually supplied are applied, so the Rust-side defaults
// stand for anything the UI leaves blank. `wantProof` defaults on — the demo
// always wants the proof/graph/prose.
function makeConfig(o: ProverConfig = {}) {
  const cfg = new Config();
  cfg.wantProof = o.wantProof !== undefined ? !!o.wantProof : true;
  if (o.timeLimitSecs != null) cfg.timeLimitSecs = o.timeLimitSecs;
  if (o.maxSteps     != null) cfg.maxSteps     = o.maxSteps;
  if (o.maxLits      != null) cfg.maxLits      = o.maxLits;
  if (o.forwardClose != null) cfg.forwardClose = !!o.forwardClose;
  if (o.profile      != null) cfg.profile      = !!o.profile;
  // 0 means "engine default" (see src/prover-config.ts's CFG_KNOBS) — leave the wasm
  // Config field unset (its own default is `None`, same effect) rather than
  // pass a literal 0% budget. 100 searches the whole KB.
  if (o.selectionTolerancePct) cfg.selectionTolerancePct = o.selectionTolerancePct;
  return cfg;
}

const handlers = {
  async boot({ baseUrl }: { baseUrl?: string } = {}) {
    siteBaseUrl = baseUrl || self.location.href;
    await init();
    session = newSession();
    return { ok: true };
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

  // Freeze/thaw seam for the page's OPFS boot cache (see src/kb-cache.ts's
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
  search({ query, limit, language, kind }) {
    return {
      hits: session.search(query, {
        limit: limit ?? 100,
        language,
        kind,
      }),
    };
  },
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
  // prover, built by @sigma/vampire (gitignored; not present unless that
  // package has been built). Runs Vampire as a subprocess-shaped CLI over a
  // self-contained TPTP problem, then hands its captured stdout+stderr to
  // `parseVampire{Ask,Audit}Result` — the SAME TSTP/SZS parsing the native
  // `ask`-gated subprocess backend uses (crates/core's
  // `prover::vampire_proof`) — so Vampire's status, proof steps, Graphviz
  // digraph, and prose come back in the exact shape the native `prove`/
  // `audit` results use, and render through the SAME `renderProof`/
  // `renderAudit` UI code unchanged.
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
  // `extraArgs` is raw user text (the settings panel's "extra CLI args"
  // field), appended verbatim after the fixed args below — advanced/opt-in,
  // not validated beyond what Vampire itself rejects. `tptp` rides back
  // alongside `result` so the page can offer it as a download without a
  // second worker round-trip to recompute the same selection.
  async proveVampire({ assertions, query, timeLimitSecs, selectionTolerancePct, extraArgs }) {
    const tptp = session.kb.toTptpForAsk(assertions || '', query, false, selectionTolerancePct || null);
    const raw_output = await runVampireProblem(tptp, timeLimitSecs, extraArgs);
    return { result: session.kb.parseVampireAskResult(raw_output, query || ''), tptp };
  },
  async auditVampire({ timeLimitSecs, extraArgs }) {
    const tptp = session.kb.toTptpIndexed(undefined, true);
    const raw_output = await runVampireProblem(tptp, timeLimitSecs, extraArgs);
    return { result: session.kb.parseVampireAuditResult(raw_output), tptp };
  },
};

// Lazy: only fetched/instantiated the first time a Vampire-backed handler
// runs, so a demo that never touches this backend (or a deploy where the
// Vampire build was skipped) never pays for it.
//
// `@vite-ignore` keeps this out of the bundle graph: the runner is a static
// passthrough asset that is legitimately absent from most builds, and a
// bundler-resolved import of a missing module is a build error rather than the
// runtime fallback below. Resolved against the base the page passed to boot(),
// since the built worker lives in the bundle's asset directory.
let vampireRunnerPromise = null;
function loadVampireRunner() {
  if (!vampireRunnerPromise) {
    const url = new URL('vampire/vampire-runner.js', siteBaseUrl).href;
    // Don't cache a rejection: a deploy missing public/vampire/ fails every
    // call identically, but a later redeploy — or a transient network blip
    // fetching the chunk — should get a fresh attempt instead of being stuck
    // replaying the first failure forever.
    vampireRunnerPromise = import(/* @vite-ignore */ url).catch(() => {
      vampireRunnerPromise = null;
      throw new Error(
        'The Vampire (WASM) backend is not included in this deployment ' +
        '(@sigma/vampire output missing). Use the Native backend instead.'
      );
    });
  }
  return vampireRunnerPromise;
}

// Vampire CLI args, mirroring the native subprocess backend's
// `build_vampire_args` (crates/core's `vampire::subprocess`) so its output
// parses the same way:
//  --mode vampire + --sine_selection off: our own SInE selection already ran
//    (toTptpForAsk/toTptpIndexed already filtered the axiom set), so Vampire
//    must not re-apply it — the `casc` portfolio's per-strategy encoded
//    options would silently re-enable it.
//  -p tptp: emit proof steps as `fof(f<n>, role, (...), inference(...))`
//    lines — Vampire's default human-readable proof format doesn't parse.
//  --output_axiom_names on: preserve our `kb_<sid>` axiom names in the
//    proof transcript, so steps cite back to file:line like the native path.
const VAMPIRE_ARGS = '--mode vampire --sine_selection off -p tptp --output_axiom_names on';

async function runVampireProblem(tptp, timeLimitSecs, extraArgs) {
  const { runVampire } = await loadVampireRunner();
  const t = Math.max(1, Math.floor(Number(timeLimitSecs)) || 30);
  // User-supplied args go last, so they can override a preceding default
  // (Vampire, like most CLIs, takes the last occurrence of a repeated flag).
  const extra = extraArgs && String(extraArgs).trim();
  const args = `${VAMPIRE_ARGS} --input_syntax tptp -t ${t}` + (extra ? ` ${extra}` : '');
  const res = await runVampire(tptp, args, {});
  return res.stdout + (res.stderr ? '\n' + res.stderr : '');
}

/** Hand a result's byte payload over instead of copying it, when it owns its
 *  whole (non-shared) buffer. */
function transferables(result) {
  const b = result?.bytes;
  if (!(b instanceof Uint8Array)) return [];
  const buf = b.buffer;
  return buf instanceof ArrayBuffer && b.byteOffset === 0 && b.byteLength === buf.byteLength
    ? [buf] : [];
}

self.onmessage = async (e) => {
  const { id, cmd, args } = e.data;
  try {
    const fn = handlers[cmd];
    if (!fn) throw new Error(`unknown cmd: ${cmd}`);
    const result = await fn(args || {});
    self.postMessage({ id, result }, transferables(result));
  } catch (err) {
    self.postMessage({ id, error: String(err && err.message || err) });
  }
};
