// Web Worker host for the sigmakee wasm engine. Keeps the synchronous prover
// (ingest / promote / ask / audit / validate) off the UI thread. Owns only the
// Session; the page (app.js) owns the constituent list, OPFS, localStorage, and
// the editor, and drives this worker over a tiny id-keyed RPC.

import { init, Session, Config, Backend, parseTest } from './pkg/sdk.mjs';

let session = null;

function newSession() {
  return new Session({ backend: Backend.Native, config: makeConfig() });
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
  async boot() {
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
  if (!vampireRunnerPromise) vampireRunnerPromise = import('./vampire/vampire-runner.js');
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
