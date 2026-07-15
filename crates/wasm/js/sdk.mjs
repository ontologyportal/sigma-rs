/**
 * SDK-shaped facade over the raw wasm bindings.
 *
 * Mirrors the `sigmakee-rs-sdk` crate's surface — `Session`, `Source`,
 * `Backend`, `Config` — for the browser subset of what that crate does. Proving,
 * translation, and lookup run in-browser; `validate`/`search`/`manpage`/`fork`/
 * `persist` and native git/http `Source`s are server-side only and are
 * intentionally absent here.
 *
 *     import { init, Session, Source, Backend, Config } from "sigmakee/sdk";
 *     await init();
 *     const s = new Session({ backend: Backend.Native });
 *     await s.ingest(Source.kif("(instance Socrates Man)"));
 *     s.ask("(instance Socrates Man)");   // -> { status: "Proved", ... }
 *
 * The raw bindings (`WasmNativeProver`, `WasmKnowledgeBase`) remain available
 * from the package root for direct, lower-level control.
 */
import initWasm, { WasmNativeProver, WasmKnowledgeBase, Config } from './sumo_parser_wasm.js';

export { Config };

/**
 * Which engine a {@link Session} drives — the browser subset of the SDK's
 * `Backend` enum.
 * @enum {string}
 */
export const Backend = Object.freeze({
  /** In-browser native saturation prover (`ask` proves). */
  Native: 'native',
  /** Parse / translate / lookup only; `ask` needs an external hook. */
  TranslationOnly: 'translation',
});

/**
 * A knowledge-base source, mirroring the SDK's `Source` enum. Construct via the
 * static factories; feed to {@link Session#ingest}.
 */
export class Source {
  /** @param {string} kind @param {object} spec */
  constructor(kind, spec) { this.kind = kind; this.spec = spec; }

  /** Inline KIF text (SDK `Source::Reader`). */
  static kif(text, tag = 'inline') { return new Source('kif', { text, tag }); }
  /** A single document over HTTP (SDK `Source::Http`). CORS applies. */
  static url(url, tag) { return new Source('url', { url, tag: tag ?? url }); }
  /** A browser `File` upload (SDK `Source::Local`). */
  static file(file) { return new Source('file', { file }); }
  /**
   * Every matching file in a public GitHub repo (SDK `Source::Git`).
   * @param {{owner:string, repo:string, ref?:string, dir?:string, match?:RegExp, token?:string}} o
   */
  static gitHub(o) {
    return new Source('github', {
      ref: 'HEAD', dir: '', match: /\.kif$/i, ...o,
    });
  }
}

let _initPromise = null;
/**
 * Instantiate the WASM module (idempotent). In the browser call with no args to
 * fetch the `.wasm` next to the JS; in Node pass `{ module_or_path: bytes }`.
 * @returns {Promise<unknown>}
 */
export function init(input) {
  if (!_initPromise) _initPromise = initWasm(input);
  return _initPromise;
}

/** @typedef {{ loaded: number, files: string[], errors: string[] }} LoadReport */

async function ingestUrl(kb, { url, tag }) {
  const res = await fetch(url);
  if (!res.ok) return { loaded: 0, files: [], errors: [`${url}: HTTP ${res.status}`] };
  const errors = kb.loadKif(await res.text(), tag);
  return { loaded: errors.length ? 0 : 1, files: [tag], errors };
}

async function ingestFile(kb, { file }) {
  const errors = kb.loadKif(await file.text(), file.name);
  return { loaded: errors.length ? 0 : 1, files: [file.name], errors };
}

async function ingestGitHub(kb, { owner, repo, ref, dir, match, token }) {
  const headers = token ? { Authorization: `Bearer ${token}` } : undefined;
  const treeUrl = `https://api.github.com/repos/${owner}/${repo}/git/trees/${ref}?recursive=1`;
  const treeRes = await fetch(treeUrl, { headers });
  if (!treeRes.ok) return { loaded: 0, files: [], errors: [`${treeUrl}: HTTP ${treeRes.status}`] };
  const prefix = dir ? dir.replace(/^\/+|\/+$/g, '') + '/' : '';
  const paths = ((await treeRes.json()).tree || [])
    .filter((e) => e.type === 'blob' && e.path.startsWith(prefix) && match.test(e.path))
    .map((e) => e.path).sort();
  if (paths.length === 0) {
    return { loaded: 0, files: [], errors: [`no files matching ${match} under "${dir}"`] };
  }
  const report = { loaded: 0, files: [], errors: [] };
  for (const path of paths) {
    const rawUrl = `https://raw.githubusercontent.com/${owner}/${repo}/${ref}/${path}`;
    try {
      const r = await fetch(rawUrl, { headers });
      if (!r.ok) { report.errors.push(`${path}: HTTP ${r.status}`); continue; }
      const errs = kb.loadKif(await r.text(), path);
      if (errs.length) report.errors.push(...errs.map((e) => `${path}: ${e}`));
      else { report.loaded += 1; report.files.push(path); }
    } catch (e) { report.errors.push(`${path}: ${e}`); }
  }
  return report;
}

/**
 * A knowledge base plus prover — the browser analogue of the SDK's `Session`.
 * Wraps a raw binding chosen by {@link Backend}; the underlying binding is
 * reachable via {@link Session#kb} for anything the facade doesn't cover.
 */
export class Session {
  #kb; #backend;

  /** @param {{ backend?: string, config?: Config }} [opts] */
  constructor({ backend = Backend.Native, config } = {}) {
    this.#backend = backend;
    this.#kb = backend === Backend.TranslationOnly
      ? new WasmKnowledgeBase()
      : new WasmNativeProver();
    if (config) this.configure(config);
  }

  /** The selected {@link Backend}. */
  get backend() { return this.#backend; }
  /** The underlying raw binding (escape hatch). */
  get kb() { return this.#kb; }

  /** Set the active {@link Config} (native backend only). Returns `this`. */
  configure(config) { this.#kb.configure?.(config); return this; }

  /**
   * Load a {@link Source} into the KB. Async because URL/GitHub sources fetch.
   * @param {Source} source
   * @returns {Promise<LoadReport>}
   */
  async ingest(source) {
    switch (source.kind) {
      case 'kif': {
        const { text, tag } = source.spec;
        const errors = this.#kb.loadKif(text, tag);
        return { loaded: errors.length ? 0 : 1, files: [tag], errors };
      }
      case 'url':    return ingestUrl(this.#kb, source.spec);
      case 'file':   return ingestFile(this.#kb, source.spec);
      case 'github': return ingestGitHub(this.#kb, source.spec);
      default: throw new Error(`unknown Source kind: ${source.kind}`);
    }
  }

  /** Assert one formula into an in-memory session (default "default"). */
  tell(kif, session) { return this.#kb.tell(kif, session); }

  /**
   * Prove `queryKif`.
   * - Native backend: returns the result object (`{ status, proved, proof, … }`).
   * - TranslationOnly backend: requires `opts.hook(tptp) => string` and returns
   *   the hook's raw output.
   * @param {string} queryKif
   * @param {{ session?: string, hook?: (tptp: string) => string }} [opts]
   */
  ask(queryKif, opts = {}) {
    if (this.#backend === Backend.TranslationOnly) {
      if (typeof opts.hook !== 'function') {
        throw new Error('ask() on a TranslationOnly session needs opts.hook(tptp)');
      }
      return this.#kb.ask(queryKif, opts.hook);
    }
    return this.#kb.ask(queryKif, opts.session);
  }

  /**
   * Render the KB as TPTP (TranslationOnly backend).
   * @param {{ lang?: "fof"|"tff", hideNumbers?: boolean, session?: string }} [opts]
   */
  translate({ lang = 'fof', hideNumbers = true, session } = {}) {
    if (typeof this.#kb.toTptp !== 'function') {
      throw new Error('translate() requires a TranslationOnly session');
    }
    return this.#kb.toTptp(lang, hideNumbers, session);
  }

  /** Pattern lookup; "_" is a wildcard. */
  lookup(pattern) { return this.#kb.lookup(pattern); }

  /** Semantic validation over the whole KB. Returns a `string[]` (empty ⇒ clean). */
  validate() { return this.#kb.validate(); }

  /** Validate one inline formula without mutating the KB. Returns a `string[]`. */
  validateFormula(kif) { return this.#kb.validateFormula(kif); }

  /**
   * Symbol / full-text search over the KB.
   * @param {string} query
   * @param {{ kind?: string, language?: string, limit?: number }} [opts]
   * @returns {Array<{symbol:string, kinds:string[], source:string, language:string, text:string}>}
   */
  search(query, { kind, language, limit } = {}) {
    return this.#kb.search(query, kind, language, limit);
  }

  /** Structured man page for a symbol, or `null` if unknown. */
  manpage(symbol) { return this.#kb.manpage(symbol); }

  /** Drop a session's assertions. */
  flushSession(session) { this.#kb.flushSession(session); }
}

// -- Standalone loaders --------------------------------------------------------
// The same fetch/File logic as Session#ingest, but operating directly on a raw
// binding (`WasmNativeProver` / `WasmKnowledgeBase`) for callers not using the
// Session facade. Any object with `loadKif(text, tag) => string[]` works.

/** @param {{loadKif(t:string,g:string):string[]}} kb @returns {Promise<LoadReport>} */
export function loadFromUrl(kb, url, tag = url) { return ingestUrl(kb, { url, tag }); }
/** @param {{loadKif(t:string,g:string):string[]}} kb @param {File} file */
export function loadFromFile(kb, file) { return ingestFile(kb, { file }); }
/** @param {{loadKif(t:string,g:string):string[]}} kb */
export function loadFromGitHubRepo(kb, opts = {}) {
  return ingestGitHub(kb, { ref: 'HEAD', dir: '', match: /\.kif$/i, ...opts });
}
