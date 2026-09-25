// The sigma worker's RPC surface: one handler per `cmd`, each taking the
// single `args` object the page posts and returning the result it posts back.
//
// Split out of sigma.worker.ts so the page can import `Handlers` type-only
// (services/sigma.ts derives `call`'s cmd/args/result types from it) without
// pulling the worker entry -- and its `self`/WebWorker globals -- into the
// page's DOM-lib program. Nothing here may touch `self`, `document` or
// `window`: this file is compiled by both tsconfigs.

import {
  init,
  Session,
  Config,
  Backend,
  parseTest,
  parseTptpTest,
  sumoSymbols,
} from "sigmakee/sdk";
import type {
  AskResult,
  AuditResult,
  Diagnostic,
  FileStats,
  KbStats,
  ManPage,
  ParsedTest,
  SearchHit,
  TaxConstraint,
  SumoSymbols,
  WordNetDiagnostics,
  WordNetFiles,
} from "sigmakee/sdk";
import { WasmLsp } from "sigmakee";

// Not imported from prover-config.ts: that file is DOM code (this worker has
// no `document`/`window`), and even a type-only import pulls the whole file
// into this program's compilation graph under the worker's lib settings.
// Duplicated to match the wire shape postMessage actually carries -- worker
// and page are separate runtimes, never sharing memory.
export interface ProverConfig {
  timeLimitSecs?: number;
  maxSteps?: number;
  maxLits?: number;
  forwardClose?: boolean;
  wantProof?: boolean;
  profile?: boolean;
  selectionTolerancePct?: number;
  backend?: "native" | "vampire" | "e";
  vampireArgs?: string;
  selectionBudget?: number;
  auditAxfilter?: boolean;
  auditSubsetLimit?: number;
  selectionTimeLimitSecs?: number;
}

let session: Session | null = null;
// Language-server facade over the SAME knowledge base as `session` (the
// shared-KB seam: `new WasmLsp(active().kb)` clones the KB Arc). Lazily
// (re)built by the `lsp` handler, dropped whenever the session is replaced.
let wasmLsp: WasmLsp | null = null;

/** The booted session. Every handler but `boot` runs after `boot` has
 *  resolved, so a null here is a page-side ordering bug rather than a
 *  recoverable state -- it surfaces as an RPC rejection either way. */
function active(): Session {
  if (!session) throw new Error("session not booted");
  return session;
}

function newSession(): Session {
  wasmLsp = null;
  return new Session({ backend: Backend.Native, config: makeConfig() });
}

// Build a wasm `Config` from a plain settings object (the Ask/Tell settings
// menu). Only keys actually supplied are applied, so the Rust-side defaults
// stand for anything the UI leaves blank. `wantProof` defaults on — the demo
// always wants the proof/graph/prose.
function makeConfig(o: ProverConfig = {}): Config {
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
  cfg.keepTptp = o.backend === "vampire" || o.backend === "e";
  if (o.selectionBudget != null) cfg.selectionBudget = o.selectionBudget;
  if (o.auditAxfilter != null) cfg.auditAxfilter = o.auditAxfilter;
  if (o.auditSubsetLimit != null) cfg.auditSubsetLimit = o.auditSubsetLimit;
  if (o.selectionTimeLimitSecs != null)
    cfg.selectionTimeLimitSecs = o.selectionTimeLimitSecs;
  return cfg;
}

export const handlers = {
  async boot(): Promise<{ symbols: SumoSymbols }> {
    await init();
    session = newSession();
    return { symbols: sumoSymbols() };
  },
  // Drop the session and start fresh (the page re-ingests every constituent).
  newSession(): { ok: true } {
    session = newSession();
    return { ok: true };
  },

  // Ingest one constituent WITHOUT promoting; the page promotes later.
  ingest({ name, text }: { name: string; text: string }): {
    notices: string[];
  } {
    return { notices: active().kb.ingest(text, name) };
  },

  // Install the WordNet lexicon from already-fetched mapping-file text (the
  // page fetches -- see boot.ts's loadWordNetIntoWorker -- this worker only
  // ever parses/installs). Separate from KIF ingestion, same as the SDK/wasm
  // API this mirrors (`Session::loadWordNet` / `Session::ingest`).
  loadWordNet(files: WordNetFiles): { synsets: number } {
    return { synsets: active().loadWordNet(files) };
  },
  // Drop the currently installed WordNet lexicon, if any -- the KB tab's
  // disable toggle. A no-op if none was loaded.
  clearWordNet(): { ok: true } {
    active().clearWordNet();
    return { ok: true };
  },
  // Promote a batch of ingested constituents into the axiom base (the deferred,
  // heavier step). Already-promoted names are a fast no-op in core.
  promoteAll({ names }: { names: string[] }): { ok: true } {
    for (const n of names) active().promote(n);
    return { ok: true };
  },

  validate(): { diagnostics: Diagnostic[] } {
    return { diagnostics: active().validate() };
  },

  /**
   * WordNet<->KB diagnostics report over the active session's installed
   * lexicon (see boot.ts's loadWordNetIntoWorker) and current KB. `null`
   * when no lexicon is loaded -- the Diagnostics tab hides its WordNet
   * card in that case rather than showing an empty one.
   */
  wordnetDiagnostics({ limit }: { limit?: number } = {}): {
    diagnostics: WordNetDiagnostics | null;
  } {
    return { diagnostics: active().wordnetDiagnostics(limit ?? 50) };
  },
  stats(): { stats: KbStats } {
    return { stats: active().kb.stats() };
  },

  /** One file's edit-relevant KB footprint (see {@link FileStats}), for the
   *  Edit tab's file-info panel -- `null` when it has no root sentences. */
  fileStats({ file }: { file: string }): { stats: FileStats | null } {
    return { stats: active().kb.fileStats(file) };
  },

  // Freeze/thaw seam for the page's OPFS boot cache (see src/kb-cache.ts's
  // tryRestoreFromCache/saveKbCache) — native backend only, per the SDK doc.
  snapshot(): { bytes: Uint8Array<ArrayBuffer> } {
    return { bytes: active().snapshot() };
  },
  restore({ bytes }: { bytes: Uint8Array }): { ok: true } {
    active().restore(bytes);
    return { ok: true };
  },

  /**
   * One LSP JSON-RPC message body in, every server->client message it
   * produced out (response first, then notifications) — `WasmLsp` is the
   * transport-free `sumo-lsp` dispatch core sharing this worker's KB, so
   * `didChange` diffs the buffer into the live KB exactly like the old
   * `validateBuffer` lane did, and diagnostics ride back in the same batch.
   */
  lsp({ json }: { json: string }): { out: string[] } {
    if (!wasmLsp) wasmLsp = new WasmLsp(active().kb);
    return { out: wasmLsp.handleMessage(json) };
  },

  /**
   * Apply the server's pending debounced reloads (see `WasmLsp.flushReloads`)
   * and return the messages they produce -- the `publishDiagnostics` a
   * `didChange` defers. `force` applies them regardless of the debounce.
   */
  lspFlush({ force }: { force?: boolean } = {}): { out: string[] } {
    if (!wasmLsp) return { out: [] };
    return { out: wasmLsp.flushReloads(!!force) };
  },

  /**
   * Validate scratch input (the Ask/Tell box, or an editor buffer with no
   * backing file) in a THROWAWAY session — never the live KB. That session has
   * no SUMO loaded, so every symbol reference reads "unknown"; only `parse`
   * diagnostics are meaningful without context, so the rest are dropped.
   */
  validateFormula({ kif }: { kif: string }): { diagnostics: Diagnostic[] } {
    const diagnostics = newSession()
      .validateFormula(kif)
      .filter((d) => d.kind === "parse");
    return { diagnostics };
  },

  // Ask/Tell live validation: assertions + query in one scratch session
  // against the LIVE KB, so symbol references resolve and the assertions'
  // own declarations are in scope for the query.
  validateScratch({
    assertions,
    query,
  }: {
    assertions?: string;
    query?: string;
  }): { assertions: Diagnostic[]; query: Diagnostic[] } {
    return active().validateScratch(assertions || "", query || "");
  },

  parseTest({ name, text }: { name: string; text: string }): {
    test: ParsedTest;
  } {
    return { test: parseTest(name, text) };
  },
  parseTptpTest({
    name,
    text,
    remap,
  }: {
    name: string;
    text: string;
    remap?: boolean;
  }): { test: ParsedTest } {
    return { test: parseTptpTest(name, text, !!remap) };
  },

  search({
    query,
    limit,
    language,
    kind,
    wordnetOnly,
    taxonomy,
  }: {
    query: string;
    limit?: number;
    language?: string;
    kind?: string;
    wordnetOnly?: boolean;
    taxonomy?: TaxConstraint[];
  }): { hits: SearchHit[] } {
    // WordNet is loaded eagerly at boot (see boot.ts's loadWordNetIntoWorker);
    // if that failed or hasn't finished, `search` just runs without WordNet
    // hits -- the SDK/wasm session tolerates no lexicon installed.
    return {
      hits: active().search(query, {
        limit: limit ?? 100,
        language,
        kind,
        wordnetOnly,
        taxonomy,
      }),
    };
  },
  manpage({ symbol }: { symbol: string }): { page: ManPage | null } {
    return { page: active().manpage(symbol) };
  },
  taxonomy({ symbol }: { symbol: string }): {
    tax: ReturnType<Session["taxonomy"]>;
  } {
    return { tax: active().taxonomy(symbol) };
  },
  naturalLanguages(): { languages: Array<{ symbol: string; label: string }> } {
    return { languages: active().naturalLanguages() };
  },
  renderNl({
    kif,
    language,
    genericVars,
  }: {
    kif: string;
    language: string;
    genericVars?: boolean;
  }): { text: string } {
    return { text: active().renderNl(kif, language, genericVars) };
  },

  prove({
    assertions,
    query,
    config,
    session: sess,
    tptp,
  }: {
    assertions?: string;
    query: string;
    config?: ProverConfig;
    session?: string;
    tptp?: boolean;
  }): { result: AskResult } {
    active().configure(makeConfig(config));
    const tag = sess || "user-assertions";
    active().flushSession(tag);
    if (assertions && assertions.trim()) {
      const t = active().tell(assertions, tag, !!tptp);
      if (!t.ok)
        throw new Error(
          "assertion parse errors: " + t.errors.slice(0, 3).join("; "),
        );
    }
    return { result: active().ask(query, { session: tag, tptp: !!tptp }) };
  },

  audit({ config, limit }: { config?: ProverConfig; limit?: number }): {
    result: AuditResult;
  } {
    active().configure(makeConfig(config));
    return { result: active().auditConsistency(limit ?? 5) };
  },
};

/** The worker's RPC surface, as the page's `call` reads it (see
 *  services/sigma.ts): command name -> its args and result. */
export type Handlers = typeof handlers;
