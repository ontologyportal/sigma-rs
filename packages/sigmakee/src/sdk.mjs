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
 * The raw binding (`Session`, exported from the package root) remains
 * available for direct, lower-level control; it is aliased to `WasmSession`
 * here so this facade's own `Session` class can wrap it.
 */
import initWasm, {
  Session as WasmSession,
  Config,
  parseTest as wasmParseTest,
} from "./sumo_parser_wasm.js";

export { Config };

/**
 * Parse a `.kif.tq` test file (requires {@link init}).  Returns
 * `{ name, note, timeout, queryKif, axiomKif, expectedProof, expectedAnswer,
 * extraFiles }`; throws with the parse diagnostic's message on malformed
 * input.
 */
export function parseTest(name, text) {
  return wasmParseTest(name, text);
}

/**
 * Render an Ask/Tell pair as `.kif.tq` test-file text — the inverse of
 * {@link parseTest} for the fields the Ask/Tell window carries.  Pure string
 * building, no WASM needed.  The query is wrapped in the format's
 * `(query …)` directive; `expectedProof` becomes `(answer yes|no)`.
 */
export function formatTest({
  note = "",
  timeout = 0,
  assertions = "",
  query = "",
  expectedProof = null,
} = {}) {
  const out = [];
  if (note) out.push(`(note "${String(note).replace(/"/g, "'")}")`); // .tq notes have no escape syntax
  if (timeout > 0) out.push(`(time ${Math.round(timeout)})`);
  const a = assertions.trim();
  if (a) out.push(a);
  const q = query.trim();
  if (q) out.push(`(query ${q})`);
  if (expectedProof !== null)
    out.push(`(answer ${expectedProof ? "yes" : "no"})`);
  return out.join("\n") + "\n";
}

/**
 * Which engine a {@link Session} drives — the browser subset of the SDK's
 * `Backend` enum.
 * @enum {string}
 */
export const Backend = Object.freeze({
  /** In-browser native saturation prover (`ask` proves). */
  Native: "native",
  /** Parse / translate / lookup only; `ask` needs an external hook. */
  TranslationOnly: "translation",
});

/**
 * A knowledge-base source, mirroring the SDK's `Source` enum. Construct via the
 * static factories; feed to {@link Session#ingest}.
 */
export class Source {
  /** @param {string} kind @param {object} spec */
  constructor(kind, spec) {
    this.kind = kind;
    this.spec = spec;
  }

  /** Inline KIF text (SDK `Source::Reader`). */
  static kif(text, tag = "inline") {
    return new Source("kif", { text, tag });
  }
  /** A single document over HTTP (SDK `Source::Http`). CORS applies. */
  static url(url, tag) {
    return new Source("url", { url, tag: tag ?? url });
  }
  /** A browser `File` upload (SDK `Source::Local`). */
  static file(file) {
    return new Source("file", { file });
  }
  /**
   * Every matching file in a public GitHub repo (SDK `Source::Git`).
   * @param {{owner:string, repo:string, ref?:string, dir?:string, match?:RegExp, token?:string}} o
   */
  static gitHub(o) {
    return new Source("github", {
      ref: "HEAD",
      dir: "",
      match: /\.kif$/i,
      ...o,
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

// `load` is the binding method to use: 'loadKif' (fused ingest+promote) or
// 'ingest' (ingest only; the caller promotes later via Session.promote).
async function ingestUrl(kb, { url, tag }, load = "loadKif") {
  const res = await fetch(url);
  if (!res.ok)
    return { loaded: 0, files: [], errors: [`${url}: HTTP ${res.status}`] };
  const errors = kb[load](await res.text(), tag);
  return { loaded: errors.length ? 0 : 1, files: [tag], errors };
}

async function ingestFile(kb, { file }, load = "loadKif") {
  const errors = kb[load](await file.text(), file.name);
  return { loaded: errors.length ? 0 : 1, files: [file.name], errors };
}

async function ingestGitHub(
  kb,
  { owner, repo, ref, dir, match, token },
  load = "loadKif",
) {
  const headers = token ? { Authorization: `Bearer ${token}` } : undefined;
  const treeUrl = `https://api.github.com/repos/${owner}/${repo}/git/trees/${ref}?recursive=1`;
  const treeRes = await fetch(treeUrl, { headers });
  if (!treeRes.ok)
    return {
      loaded: 0,
      files: [],
      errors: [`${treeUrl}: HTTP ${treeRes.status}`],
    };
  const prefix = dir ? dir.replace(/^\/+|\/+$/g, "") + "/" : "";
  const paths = ((await treeRes.json()).tree || [])
    .filter(
      (e) =>
        e.type === "blob" && e.path.startsWith(prefix) && match.test(e.path),
    )
    .map((e) => e.path)
    .sort();
  if (paths.length === 0) {
    return {
      loaded: 0,
      files: [],
      errors: [`no files matching ${match} under "${dir}"`],
    };
  }
  const report = { loaded: 0, files: [], errors: [] };
  for (const path of paths) {
    const rawUrl = `https://raw.githubusercontent.com/${owner}/${repo}/${ref}/${path}`;
    try {
      const r = await fetch(rawUrl, { headers });
      if (!r.ok) {
        report.errors.push(`${path}: HTTP ${r.status}`);
        continue;
      }
      const errs = kb[load](await r.text(), path);
      if (errs.length) report.errors.push(...errs.map((e) => `${path}: ${e}`));
      else {
        report.loaded += 1;
        report.files.push(path);
      }
    } catch (e) {
      report.errors.push(`${path}: ${e}`);
    }
  }
  return report;
}

/**
 * A knowledge base plus prover — the browser analogue of the SDK's `Session`.
 * Wraps a raw binding chosen by {@link Backend}; the underlying binding is
 * reachable via {@link Session#kb} for anything the facade doesn't cover.
 */
export class Session {
  #kb;
  #backend;

  /** @param {{ backend?: string, config?: Config }} [opts] */
  constructor({ backend = Backend.Native, config } = {}) {
    this.#backend = backend;
    // One binding for both backends: the raw wasm `Session` carries the whole
    // stack (native proving AND TPTP translation); `Backend.TranslationOnly`
    // only changes how `ask`/`translate` behave on the facade.
    this.#kb = new WasmSession();
    if (config) this.configure(config);
  }

  /** The selected {@link Backend}. */
  get backend() {
    return this.#backend;
  }
  /** The underlying raw binding (escape hatch). */
  get kb() {
    return this.#kb;
  }

  /** Set the active {@link Config} (native backend only). Returns `this`. */
  configure(config) {
    this.#kb.configure?.(config);
    return this;
  }

  /**
   * Load a {@link Source} into the KB. Async because URL/GitHub sources fetch.
   * Pass `{ promote: false }` to ingest only (search / man pages / editing work
   * immediately); then call {@link Session#promote} on the returned `files` to
   * finish axiomatization (proving). Native backend only for deferred promote.
   * @param {Source} source
   * @param {{ promote?: boolean }} [opts]
   * @returns {Promise<LoadReport>}
   */
  async ingest(source, { promote = true } = {}) {
    const load = promote ? "loadKif" : "ingest";
    switch (source.kind) {
      case "kif": {
        const { text, tag } = source.spec;
        const errors = this.#kb[load](text, tag);
        return { loaded: errors.length ? 0 : 1, files: [tag], errors };
      }
      case "url":
        return ingestUrl(this.#kb, source.spec, load);
      case "file":
        return ingestFile(this.#kb, source.spec, load);
      case "github":
        return ingestGitHub(this.#kb, source.spec, load);
      default:
        throw new Error(`unknown Source kind: ${source.kind}`);
    }
  }

  /** Promote an ingested source (by tag) into the axiom base. Native only. */
  promote(tag) {
    return this.#kb.promote(tag);
  }

  /**
   * Freeze the whole KB to a portable `Uint8Array` (native backend). Stash it
   * (IndexedDB, a file, a download) and rebuild later with {@link Session#restore}
   * instead of re-ingesting. Heed-free; the bytes carry the promoted axiom base.
   * @returns {Uint8Array}
   */
  snapshot() {
    return this.#kb.snapshot();
  }

  /**
   * Thaw a KB frozen by {@link Session#snapshot}, replacing this session's
   * contents in place (native backend). The active {@link Config} is preserved.
   * @param {Uint8Array} bytes
   */
  restore(bytes) {
    return this.#kb.restore(bytes);
  }

  /** Assert one formula into an in-memory session (default "default"). */
  tell(kif, session) {
    return this.#kb.tell(kif, session);
  }

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
      if (typeof opts.hook !== "function") {
        throw new Error(
          "ask() on a TranslationOnly session needs opts.hook(tptp)",
        );
      }
      return opts.hook(this.#kb.toTptpForAsk("", queryKif));
    }
    return this.#kb.ask(queryKif, opts.session);
  }

  /**
   * Consistency-audit the whole KB (Native backend only): enumerates up to
   * `limit` distinct contradictions via the native saturation prover, each
   * cited back to `file:line` wherever a step traces to an input axiom.
   * @param {number} [limit] caps distinct contradictions found (default 5).
   * @returns {import('./sdk').AuditResult}
   */
  auditConsistency(limit) {
    if (typeof this.#kb.auditConsistency !== "function") {
      throw new Error("auditConsistency() requires a Native session");
    }
    return this.#kb.auditConsistency(limit);
  }

  /**
   * Render the whole KB as TPTP.
   * @param {{ lang?: "fof"|"tff", hideNumbers?: boolean }} [opts]
   */
  translate({ lang = "fof", hideNumbers = true } = {}) {
    return this.#kb.toTptpIndexed(lang, hideNumbers);
  }

  /** Pattern lookup; "_" is a wildcard. */
  lookup(pattern) {
    return this.#kb.lookup(pattern);
  }

  /** Semantic validation over the whole KB. Returns a `string[]` (empty ⇒ clean). */
  validate() {
    return this.#kb.validate();
  }

  /** Validate one inline formula without mutating the KB. Returns a `string[]`. */
  validateFormula(kif) {
    return this.#kb.validateFormula(kif);
  }

  /**
   * Validate an Ask/Tell pair — assertions then query — in one scratch
   * session against the live KB, so the assertions' own declarations are in
   * scope for the query. Returns `{ assertions, query }` diagnostic arrays.
   */
  validateScratch(assertions, query) {
    return this.#kb.validateScratch(assertions, query);
  }

  /**
   * Symbol / full-text search over the KB. With a lexicon loaded (see
   * {@link Session#loadWordNet}), results include WordNet synonym hits
   * (`source: "wn"`, with a `sense` tag like `"dog#n#1+"`); `wordnetOnly:
   * true` returns only those. `taxonomy` ANDs a list of taxonomic
   * constraints (see {@link TaxConstraint}) -- `subclassOf`/`instanceOf` are
   * also expressible inline in `query` itself (`-subclass->Class` /
   * `-instance->Class`); a non-empty `taxonomy` here wins over that inline
   * form rather than combining with it.
   * @param {string} query
   * @param {{ kind?: string, language?: string, limit?: number, wordnetOnly?: boolean, taxonomy?: TaxConstraint[] }} [opts]
   * @returns {Array<{symbol:string, kinds:string[], source:string, language:string, text:string, sense:string, rank:number, rank_breakdown:Array<{label:string,value:number}>, wordnet:Array<{words:string,pos:string,mapping:string,suffix:string,gloss:string}>}>}
   */
  search(query, { kind, language, limit, wordnetOnly, taxonomy } = {}) {
    return this.#kb.search(query, kind, language, limit, wordnetOnly, taxonomy);
  }

  /**
   * Load the WordNet-SUMO lexicon from the contents of the four
   * `WordNetMappings30-*.txt` files — the browser fetches the bytes (there
   * is no filesystem here) and passes strings. `indexSense`
   * (most-frequent-sense ordering) and `exceptions` (`noun.exc` + `verb.exc`
   * concatenated) are optional refinements.
   *
   * Installation is separate from KIF ingestion ({@link Session#ingest}):
   * once loaded, {@link Session#search} gains WordNet synonym hits.
   * @param {{ noun: string, verb: string, adj: string, adv: string, indexSense?: string, exceptions?: string }} files
   * @returns {number} the number of synsets indexed
   */
  loadWordNet({ noun, verb, adj, adv, indexSense, exceptions }) {
    return this.#kb.loadWordNet(noun, verb, adj, adv, indexSense, exceptions);
  }

  /** Drop the currently installed WordNet lexicon, if any — the inverse of
   *  {@link Session#loadWordNet}. Subsequent {@link Session#search} calls
   *  stop returning WordNet hits. A no-op if none was loaded. */
  clearWordNet() {
    this.#kb.clearWordNet();
  }

  /** Structured man page for a symbol, or `null` if unknown. */
  manpage(symbol) {
    return this.#kb.manpage(symbol);
  }

  /** Direct taxonomy edges `{parents, children}` — the lightweight peer of
   *  `manpage` for lazy tree expansion. */
  taxonomy(symbol) {
    return this.#kb.taxonomy(symbol);
  }

  /** `NaturalLanguage` instances as `[{symbol, label}]`, for the UI selector. */
  naturalLanguages() {
    return this.#kb.naturalLanguages();
  }

  /** Natural-language paraphrase of a single KIF formula in `language`. */
  renderNl(kif, language) {
    return this.#kb.renderNl(kif, language);
  }

  /** Drop a session's assertions. */
  flushSession(session) {
    this.#kb.flushSession(session);
  }
}

// -- KIF formatting -------------------------------------------------------------
//
// Deliberately NOT implemented as reparse-with-core-then-re-render (the
// approach `sumo-lsp`'s textDocument/formatting uses, via
// AstNode::format_plain): SUO-KIF comments (`;...`) are discarded at the
// tokenizer stage and never reach core's AST at all — core has no concept of
// a comment surviving a parse. Reformatting through core would silently
// delete every comment in the buffer, and the LSP's formatter does exactly
// that today (a known, accepted limitation there — an LSP client can at
// least show a diff/confirm before applying an edit; a live editor button
// should not carry that risk by surprise).
//
// Instead: a structural re-indenter/reflower that never discards to an AST.
// It keeps every existing line break, blank line, comment, and string
// exactly as written — it only ever ADDS line breaks, never removes one —
// and rewrites leading whitespace to match paren-nesting depth. Safe on a
// buffer with unbalanced parens (depth just clamps at 0) since it never
// requires the input to parse.

/** Two open parens are never allowed on the same output line, with two
 *  named exceptions: `not`'s sole argument, and a quantifier's
 *  (`exists`/`forall`) variable list — both conventionally sit on the same
 *  line as the symbol that introduces them. Every other case of a second
 *  open before the first has closed gets a line break inserted before it. */
const INLINE_HEAD_EXCEPTIONS = new Set(["not", "exists", "forall"]);

/** Tokenizer for the reflow pass: comments/strings/parens as before (see
 *  the two-alternative closed-vs-unterminated-string split below), plus a
 *  dedicated symbol-word group — the only token kind that can BE "not" /
 *  "exists" / "forall" — and a single-char catch-all for everything else
 *  (variables, numbers, operators). The catch-all is coarse (one char per
 *  match rather than a whole token), but that's harmless here: all that
 *  matters for anything besides parens/words is *that* something occupied
 *  the position, invalidating a pending inline exception — not what it was. */
const KIF_REFLOW_SCAN_RE =
  /(;[^\n]*)|("(?:[^"\\]|\\.)*")|("(?:[^"\\]|\\.)*$)|([()])|([A-Za-z_][A-Za-z0-9_-]*)|(\S)/g;

/**
 * Reformat `text` (raw KIF source, need not even be syntactically valid):
 * rewrite leading whitespace to match paren-nesting depth, and insert a line
 * break wherever a second open paren would otherwise land on a line that
 * already has one open (except `not`'s argument and a quantifier's variable
 * list — see {@link INLINE_HEAD_EXCEPTIONS}). Never REMOVES an existing line
 * break for anything with content, so a file that's already broken up more
 * than the rule requires is left alone; it only closes gaps where the rule
 * is violated. Two exceptions to that, both matching SUMO's own Merge.kif
 * convention: a head symbol (e.g. `exists`) gets a space inserted before an
 * immediately-adjacent `(`, and a line consisting solely of closing parens
 * is folded onto the end of the previous non-blank line rather than left to
 * trail on its own. `indentUnit` defaults to 3 spaces. Comments, strings
 * (including one that happens to span a newline, however unusual), blank
 * lines, and inter-token spacing are otherwise preserved byte-for-byte.
 *
 * @param {string} text
 * @param {{indentUnit?: string}} [opts]
 * @returns {string}
 */
export function formatKif(text, { indentUnit = "   " } = {}) {
  const srcLines = text.split("\n");
  const out = [];

  let depth = 0;
  let inString = false; // true mid-line ⇒ carried over an unterminated "…\n
  // One-shot per-nesting-level bookkeeping (0-based: level d == paren depth
  // d+1): headAtLevel[d] is the head symbol of the form open at that level,
  // once its first token is seen; exceptionOpen[d] is true only until that
  // head's one allowed inline argument (if it has one) has been placed.
  const headAtLevel = [];
  const exceptionOpen = [];

  // The output line currently being assembled — one source line may expand
  // into several of these when a break gets inserted mid-line.
  let cur = "";
  // A one-way latch, NOT a count of still-open parens: once any '(' has
  // landed on this line, it stays true even after that form fully closes —
  // "two open parens on one line" is a textual rule, not "two simultaneously
  // unclosed", so a second sibling argument that's itself a compound form
  // still isn't allowed to share the line just because the first already
  // closed (e.g. `(instance ?X Dog) (instance ?X Animal)` still splits).
  let openedOnCur = false;
  const startAt = (indentDepth) => {
    cur = indentUnit.repeat(Math.max(0, indentDepth));
    openedOnCur = false;
  };

  /** Scans `scanText` for parens/heads, updating depth/head state. When
   *  `emit` is true, also reconstructs the text into `cur` (flushing to
   *  `out` and starting a fresh indented line at each rule violation);
   *  when false, only the depth/head bookkeeping runs — used for a string
   *  continuation line's trailing content, which (matching the rest of
   *  that line) is never split or re-emitted, only scanned. */
  const scan = (scanText, emit) => {
    KIF_REFLOW_SCAN_RE.lastIndex = 0;
    let m;
    let lastWritten = 0;
    let prevWordEnd = -1; // end index of the last bare word token, for the
    // "head(" adjacency check below
    while ((m = KIF_REFLOW_SCAN_RE.exec(scanText))) {
      const [full, , , openStr, paren, word] = m; // [full, comment, closedStr, openStr, paren, word]
      if (paren === "(") {
        const parentLevel = depth - 1;
        const exempt =
          parentLevel >= 0 &&
          exceptionOpen[parentLevel] &&
          INLINE_HEAD_EXCEPTIONS.has(headAtLevel[parentLevel]);
        if (emit && openedOnCur && !exempt) {
          // Flush everything up to (not including) this '(' first, trimming
          // the dangling trailing space a line break would otherwise leave.
          cur += scanText.slice(lastWritten, m.index).replace(/[ \t]+$/, "");
          out.push(cur);
          startAt(depth);
          lastWritten = m.index;
        } else if (emit && prevWordEnd === m.index) {
          // A head symbol (e.g. `exists`) sitting directly against this '('
          // with no separating space — always insert one.
          cur += scanText.slice(lastWritten, m.index) + " ";
          lastWritten = m.index;
        }
        if (exempt) exceptionOpen[parentLevel] = false; // one-shot, now used
        depth++;
        headAtLevel[depth - 1] = undefined;
        exceptionOpen[depth - 1] = false;
        if (emit) openedOnCur = true;
      } else if (paren === ")") {
        depth--; // does NOT clear openedOnCur — see the latch comment above
      } else if (word !== undefined) {
        const lvl = depth - 1;
        if (lvl >= 0 && headAtLevel[lvl] === undefined) {
          headAtLevel[lvl] = word;
          exceptionOpen[lvl] = INLINE_HEAD_EXCEPTIONS.has(word);
        }
        prevWordEnd = m.index + word.length;
      } else if (openStr !== undefined) {
        inString = true; // reached EOL still inside a string
      }
      // comment / a fully-closed string: no effect on depth or inString.
    }
    if (emit) cur += scanText.slice(lastWritten);
  };

  for (const line of srcLines) {
    if (inString) {
      // Inside a string literal opened on an earlier line. The line is never
      // re-indented or split — it starts as string content, not code — but a
      // common SUMO pattern is a `documentation` string wrapping across
      // exactly two lines with the form's OWN closing ')' sitting right
      // after the closing quote (`...")`), so whatever follows the quote
      // still needs scanning (not emitting): skipping it undercounts depth
      // by one for every such form, and the drift compounds across the file.
      out.push(line);
      const m = /^(?:[^"\\]|\\.)*"/.exec(line);
      if (!m) continue; // still open at EOL — nothing on this line to scan
      inString = false;
      scan(line.slice(m[0].length), false);
      continue;
    }

    const trimmed = line.replace(/^[ \t]+/, "");
    if (trimmed.length === 0) {
      out.push("");
      continue;
    }
    // This line's own indent reflects depth AFTER any closing parens it
    // opens with (`)  )  qux)` should sit at the depth *after* those closes,
    // not before) — a plain run of leading ')' can't be inside a string or
    // comment, so counting it directly (ahead of the full scan below) is safe.
    let leadDepth = depth;
    for (let i = 0; trimmed[i] === ")"; i++) leadDepth--;
    startAt(leadDepth);
    scan(trimmed, true);
    out.push(cur);
  }

  // Fold a line that's nothing but closing parens onto the end of the
  // previous non-blank line, so a form's closes land together instead of
  // trailing one-per-line. Skipped when the previous line is blank (an
  // intentional separator) or carries a `;` comment, since appending after
  // a comment would get swallowed by it.
  const folded = [];
  for (const ln of out) {
    const trimmedLn = ln.trim();
    const prev = folded[folded.length - 1];
    if (
      /^\)+$/.test(trimmedLn) &&
      prev !== undefined &&
      prev.trim().length > 0 &&
      !prev.includes(";")
    ) {
      folded[folded.length - 1] = prev.replace(/[ \t]+$/, "") + trimmedLn;
    } else {
      folded.push(ln);
    }
  }
  return folded.join("\n");
}

// -- Standalone loaders --------------------------------------------------------
// The same fetch/File logic as Session#ingest, but operating directly on the
// raw wasm `Session` binding for callers not using this facade. Any object
// with `loadKif(text, tag) => string[]` works.

/** @param {{loadKif(t:string,g:string):string[]}} kb @returns {Promise<LoadReport>} */
export function loadFromUrl(kb, url, tag = url) {
  return ingestUrl(kb, { url, tag });
}
/** @param {{loadKif(t:string,g:string):string[]}} kb @param {File} file */
export function loadFromFile(kb, file) {
  return ingestFile(kb, { file });
}
/** @param {{loadKif(t:string,g:string):string[]}} kb */
export function loadFromGitHubRepo(kb, opts = {}) {
  return ingestGitHub(kb, { ref: "HEAD", dir: "", match: /\.kif$/i, ...opts });
}
