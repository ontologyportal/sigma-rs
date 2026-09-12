// Web Worker host for the sigmakee wasm engine. Keeps the synchronous prover
// (ingest / promote / ask / audit / validate) off the UI thread. Owns only the
// Session; the page (src/main.ts and friends) owns the constituent list, OPFS, localStorage, and
// the editor, and drives this worker over a tiny id-keyed RPC.

import {
  init,
  Session,
  Config,
  Backend,
  parseTest,
  parseTptpTest,
} from "sigmakee/sdk";
import { WasmLsp } from "sigmakee";
import { installVampireBridge } from "./vampire-bridge";

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
  backend?: "native" | "vampire";
  vampireArgs?: string;
}

let session = null;
// Language-server facade over the SAME knowledge base as `session` (the
// shared-KB seam: `new WasmLsp(session.kb)` clones the KB Arc). Lazily
// (re)built by the `lsp` handler, dropped whenever the session is replaced.
let wasmLsp = null;

function newSession() {
  wasmLsp = null;
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
  if (o.maxSteps != null) cfg.maxSteps = o.maxSteps;
  if (o.maxLits != null) cfg.maxLits = o.maxLits;
  if (o.forwardClose != null) cfg.forwardClose = !!o.forwardClose;
  if (o.profile != null) cfg.profile = !!o.profile;
  // 0 means "engine default" (see src/prover-config.ts's CFG_KNOBS) — leave the wasm
  // Config field unset (its own default is `None`, same effect) rather than
  // pass a literal 0% budget. 100 searches the whole KB.
  if (o.selectionTolerancePct)
    cfg.selectionTolerancePct = o.selectionTolerancePct;
  // Vampire (WASM) rides the same Config: the engine's external prover layer
  // drives it through the bridge installed at boot (see vampire-bridge.ts),
  // with the same time limit and selection budget the native backend reads.
  // The problem text is kept so the page can offer it as a download.
  if (o.backend) cfg.backend = o.backend;
  if (o.vampireArgs != null) cfg.vampireArgs = o.vampireArgs;
  cfg.keepTptp = o.backend === "vampire";
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

  // Install the WordNet lexicon from already-fetched mapping-file text (the
  // page fetches -- see boot.ts's loadWordNetIntoWorker -- this worker only
  // ever parses/installs). Separate from KIF ingestion, same as the SDK/wasm
  // API this mirrors (`Session::loadWordNet` / `Session::ingest`).
  loadWordNet({ noun, verb, adj, adv, indexSense, exceptions }) {
    const synsets = session.loadWordNet({
      noun,
      verb,
      adj,
      adv,
      indexSense,
      exceptions,
    });
    return { synsets };
  },
  // Drop the currently installed WordNet lexicon, if any -- the KB tab's
  // disable toggle. A no-op if none was loaded.
  clearWordNet() {
    session.clearWordNet();
    return { ok: true };
  },
  // Promote a batch of ingested constituents into the axiom base (the deferred,
  // heavier step). Already-promoted names are a fast no-op in core.
  promoteAll({ names }) {
    for (const n of names) session.kb.promote(n);
    return { ok: true };
  },

  validate() {
    return { diagnostics: session.validate() };
  },
  stats() {
    return { stats: session.kb.stats() };
  },

  // Freeze/thaw seam for the page's OPFS boot cache (see src/kb-cache.ts's
  // tryRestoreFromCache/saveKbCache) — native backend only, per the SDK doc.
  snapshot() {
    return { bytes: session.snapshot() };
  },
  restore({ bytes }) {
    session.restore(bytes);
    return { ok: true };
  },

  /**
   * One LSP JSON-RPC message body in, every server->client message it
   * produced out (response first, then notifications) — `WasmLsp` is the
   * transport-free `sumo-lsp` dispatch core sharing this worker's KB, so
   * `didChange` diffs the buffer into the live KB exactly like the old
   * `validateBuffer` lane did, and diagnostics ride back in the same batch.
   */
  lsp({ json }: { json: string }) {
    if (!wasmLsp) wasmLsp = new WasmLsp(session.kb);
    return { out: wasmLsp.handleMessage(json) };
  },

  /**
   * Apply the server's pending debounced reloads (see `WasmLsp.flushReloads`)
   * and return the messages they produce -- the `publishDiagnostics` a
   * `didChange` defers. `force` applies them regardless of the debounce.
   */
  lspFlush({ force }: { force?: boolean } = {}) {
    if (!wasmLsp) return { out: [] };
    return { out: wasmLsp.flushReloads(!!force) };
  },

  /**
   * Validate scratch input (the Ask/Tell box, or an editor buffer with no
   * backing file) in a THROWAWAY session — never the live KB. That session has
   * no SUMO loaded, so every symbol reference reads "unknown"; only `parse`
   * diagnostics are meaningful without context, so the rest are dropped.
   */
  validateFormula({ kif }) {
    const diagnostics = newSession()
      .validateFormula(kif)
      .filter((d) => d.kind === "parse");
    return { diagnostics };
  },

  // Ask/Tell live validation: assertions + query in one scratch session
  // against the LIVE KB, so symbol references resolve and the assertions'
  // own declarations are in scope for the query.
  validateScratch({ assertions, query }) {
    return session.kb.validateScratch(assertions || "", query || "");
  },

  parseTest({ name, text }) {
    return { test: parseTest(name, text) };
  },
  parseTptpTest({ name, text, remap }) {
    return { test: parseTptpTest(name, text, !!remap) };
  },

  search({ query, limit, language, kind, wordnetOnly, taxonomy }) {
    // WordNet is loaded eagerly at boot (see boot.ts's loadWordNetIntoWorker);
    // if that failed or hasn't finished, `search` just runs without WordNet
    // hits -- the SDK/wasm session tolerates no lexicon installed.
    return {
      hits: session.search(query, {
        limit: limit ?? 100,
        language,
        kind,
        wordnetOnly,
        taxonomy,
      }),
    };
  },
  manpage({ symbol }) {
    return { page: session.manpage(symbol) };
  },
  taxonomy({ symbol }) {
    return { tax: session.taxonomy(symbol) };
  },
  naturalLanguages() {
    return { languages: session.naturalLanguages() };
  },
  renderNl({ kif, language, genericVars }) {
    return { text: session.renderNl(kif, language, genericVars) };
  },

  prove({ assertions, query, config, session: sess, tptp }) {
    session.configure(makeConfig(config));
    const tag = sess || "user-assertions";
    session.flushSession(tag);
    if (assertions && assertions.trim()) {
      const t = session.tell(assertions, tag, !!tptp);
      if (!t.ok)
        throw new Error(
          "assertion parse errors: " + t.errors.slice(0, 3).join("; "),
        );
    }
    return { result: session.ask(query, { session: tag, tptp: !!tptp }) };
  },

  audit({ config, limit }) {
    session.configure(makeConfig(config));
    return { result: session.auditConsistency(limit ?? 5) };
  },
};

/** Hand a result's byte payload over instead of copying it, when it owns its
 *  whole (non-shared) buffer. */
function transferables(result) {
  const b = result?.bytes;
  if (!(b instanceof Uint8Array)) return [];
  const buf = b.buffer;
  return buf instanceof ArrayBuffer &&
    b.byteOffset === 0 &&
    b.byteLength === buf.byteLength
    ? [buf]
    : [];
}

self.onmessage = async (e) => {
  const { id, cmd, args } = e.data;
  // The page's port to its Vampire worker (at boot and after a restart);
  // not an RPC, nothing to answer.
  if (cmd === "vampirePort") {
    installVampireBridge(args.port);
    return;
  }
  try {
    const fn = handlers[cmd];
    if (!fn) throw new Error(`unknown cmd: ${cmd}`);
    const result = await fn(args || {});
    self.postMessage({ id, result }, transferables(result));
  } catch (err) {
    self.postMessage({ id, error: String((err && err.message) || err) });
  }
};
