// Web Worker host for the sigmakee wasm engine. Keeps the synchronous prover
// (ingest / promote / ask / audit / validate) off the UI thread. Owns only the
// Session; the page (app.js) owns the constituent list, OPFS, localStorage, and
// the editor, and drives this worker over a tiny id-keyed RPC.

import { init, Session, Config, Backend } from './pkg/sdk.mjs';

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
  search({ query, limit }) { return { hits: session.search(query, { limit: limit ?? 100 }) }; },
  manpage({ symbol }) { return { page: session.manpage(symbol) }; },

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
};

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
