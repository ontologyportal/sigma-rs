// Web Worker host for the sigmakee wasm engine. Keeps the synchronous prover
// (ingest / promote / ask / audit / validate) off the UI thread. Owns only the
// Session; the page (app.js) owns the constituent list, OPFS, localStorage, and
// the editor, and drives this worker over a tiny id-keyed RPC.

import { init, Session, Config, Backend } from './pkg/sdk.mjs';

let session = null;

function newSession() {
  const cfg = new Config();
  cfg.wantProof = true;
  return new Session({ backend: Backend.Native, config: cfg });
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
  validateFormula({ kif }) { return { diagnostics: session.validateFormula(kif) }; },
  search({ query, limit }) { return { hits: session.search(query, { limit: limit ?? 100 }) }; },
  manpage({ symbol }) { return { page: session.manpage(symbol) }; },

  prove({ assertions, query, timeLimitSecs, session: sess }) {
    const cfg = new Config();
    cfg.wantProof = true;
    if (timeLimitSecs != null) cfg.timeLimitSecs = timeLimitSecs;
    session.configure(cfg);
    const tag = sess || 'user-assertions';
    session.flushSession(tag);
    if (assertions && assertions.trim()) {
      const t = session.tell(assertions, tag);
      if (!t.ok) throw new Error('assertion parse errors: ' + t.errors.slice(0, 3).join('; '));
    }
    return { result: session.ask(query, { session: tag }) };
  },

  audit({ timeLimitSecs, limit }) {
    const cfg = new Config();
    cfg.wantProof = true;
    if (timeLimitSecs != null) cfg.timeLimitSecs = timeLimitSecs;
    session.configure(cfg);
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
