/**
 * SDK-shaped facade over the raw wasm bindings — mirrors `sigmakee-rs-sdk`'s
 * `Session` / `Source` / `Backend` / `Config` for the browser.
 */
import { Config } from './sumo_parser_wasm';

export { Config };

/** Outcome of loading a {@link Source}. */
export interface LoadReport {
  loaded: number;
  files: string[];
  errors: string[];
}

/** Native-prover result (from a Native-backed {@link Session.ask}). */
export interface AskResult {
  status:
    | 'Proved' | 'Disproved' | 'Consistent' | 'Inconsistent'
    | 'Timeout' | 'InputError' | 'Unknown';
  proved: boolean;
  given_steps: number | null;
  raw_output: string;
  /** Same shape as {@link AuditStep} so both proofs render through one code path. */
  proof: AuditStep[];
  /** The proof as a Graphviz DOT digraph — always valid, even when `proof` is empty. */
  graphviz: string;
  /** The proof narrated as connected English prose. Empty when there is no proof. */
  prose: string;
  /** Symbols the prose showed by bare name (no `format`/`termFormat` in the language). */
  prose_missing: string[];
}

/** One step of a cited contradiction derivation (see {@link AuditResult}). */
export interface AuditStep {
  index: number;
  rule: string;
  premises: number[];
  kif: string;
  /** `null` for derived/anonymous steps that don't trace to an input axiom. */
  file: string | null;
  line: number | null;
}

/** Consistency-audit result (from a Native-backed {@link Session.auditConsistency}). */
export interface AuditResult {
  status: 'Consistent' | 'Inconsistent' | 'Timeout' | 'InputError' | 'Unknown';
  inconsistent: boolean;
  given_steps: number | null;
  raw_output: string;
  /** One entry per distinct contradiction, each with its own DOT digraph and prose. */
  contradictions: Array<{
    steps: AuditStep[];
    graphviz: string;
    /** This contradiction's derivation narrated as connected English prose. */
    prose: string;
    /** Symbols the prose showed by bare name (no `format`/`termFormat`). */
    prose_missing: string[];
  }>;
}

/** Which engine a {@link Session} drives (browser subset of the SDK `Backend`). */
export const Backend: {
  readonly Native: 'native';
  readonly TranslationOnly: 'translation';
};
export type Backend = (typeof Backend)[keyof typeof Backend];

export interface GitHubSpec {
  owner: string;
  repo: string;
  ref?: string;
  dir?: string;
  match?: RegExp;
  token?: string;
}

/** A knowledge-base source (browser subset of the SDK `Source` enum). */
export class Source {
  readonly kind: string;
  readonly spec: Record<string, unknown>;
  static kif(text: string, tag?: string): Source;
  static url(url: string, tag?: string): Source;
  static file(file: File): Source;
  static gitHub(opts: GitHubSpec): Source;
}

/** Instantiate the WASM module (idempotent). */
export function init(input?: unknown): Promise<unknown>;

/**
 * Reformat raw KIF source by rewriting each line's leading whitespace to
 * match its paren-nesting depth. Pure JS, no WASM/KB involved — comments,
 * strings (including one spanning a newline), and blank lines are preserved
 * byte-for-byte; only leading indentation is rewritten. `indentUnit`
 * defaults to `"   "` (3 spaces), matching SUMO's own Merge.kif convention.
 */
export function formatKif(text: string, opts?: { indentUnit?: string }): string;

/** A parsed `.kif.tq` test file (see {@link parseTest}). */
export interface ParsedTest {
  name: string;
  note: string;
  /** `(time N)` directive; 0 when absent. */
  timeout: number;
  /** The `(query …)` formula, or `null` for an axioms-only file. */
  queryKif: string | null;
  /** Hypotheses as newline-joined KIF. */
  axiomKif: string;
  /** `(answer yes|no)` → true/false; `null` when absent or bindings-style. */
  expectedProof: boolean | null;
  /** Bindings-style `(answer …)` values, when present. */
  expectedAnswer: string[] | null;
  /** `(file …)` directives — external KIF files the test expects loaded. */
  extraFiles: string[];
}

/** Parse a `.kif.tq` test file (requires {@link init}); throws on malformed input. */
export function parseTest(name: string, text: string): ParsedTest;

/** Render an Ask/Tell pair as `.kif.tq` text (pure; the inverse of {@link parseTest}). */
export function formatTest(opts?: {
  note?: string; timeout?: number; assertions?: string; query?: string; expectedProof?: boolean | null;
}): string;

export interface AskOpts {
  session?: string;
  hook?: (tptp: string) => string;
}
export interface TranslateOpts {
  lang?: 'fof' | 'tff';
  hideNumbers?: boolean;
  session?: string;
}
export interface TellResult {
  ok: boolean;
  errors: string[];
}

export interface Diagnostic {
  severity: 'Error' | 'Warning' | 'Info' | 'Hint';
  kind: string;      // coarse category, e.g. "semantic"
  code: string;      // leaf id, e.g. "free-var-in-consequent"
  message: string;
  file: string;      // source tag the sentence was loaded under
  line: number;      // 1-based
  col: number;       // 1-based
  end_line: number;
  end_col: number;
}

export interface SearchHit {
  symbol: string;
  kinds: string[];
  source: string;
  language: string;
  text: string;
  /** Relevance score, higher = better. Hits are returned sorted by this desc. */
  rank: number;
}
export interface SearchOpts {
  kind?: string;
  language?: string;
  limit?: number;
}
export interface DocBlock { language: string; text: string; }
export interface SortSig { class: string; subclass: boolean; }
/** One formula referencing the man-paged symbol. `position` is the symbol's
 * 0-based root-level position in the sentence, or `null` when it only occurs
 * nested inside a sub-sentence. `file`/`line` are `null` for sentences with
 * no source origin (e.g. synthetic/CNF sentences). `kind` classifies the
 * formula's top-level shape — "fact" (a relation atom, possibly under `not`),
 * "=>", "<=>", "and", "or", or "other"; for a fact, `arg_pos` is the symbol's
 * argument index in the atom (0 = the relation itself), after peeling one
 * top-level `not`, or `null` when it isn't a direct argument. */
export interface ManPageRef {
  position: number | null;
  kif: string;
  file: string | null;
  line: number | null;
  kind: string;
  arg_pos: number | null;
}
export interface ManPage {
  name: string;
  kinds: string[];
  documentation: DocBlock[];
  term_format: DocBlock[];
  format: DocBlock[];
  parents: Array<{ relation: string; parent: string }>;
  children: Array<{ relation: string; parent: string }>;
  arity: number | null;
  domains: Array<{ position: number; sort: SortSig }>;
  range: SortSig | null;
  appears_in_count: number;
  consequent_count: number;
  references: ManPageRef[];
}

/** Browser analogue of the SDK's `Session`. */
export class Session {
  constructor(opts?: { backend?: Backend; config?: Config });
  readonly backend: Backend;
  /** The underlying raw wasm Session binding. */
  readonly kb: unknown;
  configure(config: Config): this;
  /** `{ promote: false }` ingests only (search/man pages work); call `promote` later. */
  ingest(source: Source, opts?: { promote?: boolean }): Promise<LoadReport>;
  /** Promote an ingested source (by tag) into the axiom base. Native backend only. */
  promote(tag: string): string[];
  /** Freeze the whole KB (promoted axioms included) to a portable byte buffer. Native backend only. */
  snapshot(): Uint8Array;
  /** Thaw a KB frozen by {@link Session.snapshot}, replacing this session in place. Native backend only. */
  restore(bytes: Uint8Array): void;
  tell(kif: string, session?: string): TellResult;
  /** Native backend → AskResult; TranslationOnly backend (with hook) → string. */
  ask(queryKif: string, opts?: AskOpts): AskResult | string;
  /** Native backend only: consistency-audit the whole KB. `limit` caps distinct contradictions (default 5). */
  auditConsistency(limit?: number): AuditResult;
  translate(opts?: TranslateOpts): string;
  lookup(pattern: string): string[];
  validate(): Diagnostic[];
  validateFormula(kif: string): Diagnostic[];
  /** Ask/Tell pair validated in one scratch session against the live KB. */
  validateScratch(assertions: string, query: string): { assertions: Diagnostic[]; query: Diagnostic[] };
  search(query: string, opts?: SearchOpts): SearchHit[];
  manpage(symbol: string): ManPage | null;
  /** Direct taxonomy edges — the lightweight peer of `manpage` for lazy tree
   * expansion. Downward rows carry the child in `parent`, as in
   * `ManPage.children`. */
  taxonomy(symbol: string): {
    parents: Array<{ relation: string; parent: string }>;
    children: Array<{ relation: string; parent: string }>;
  };
  /** `NaturalLanguage` instances as `{symbol, label}`, for the UI selector. */
  naturalLanguages(): Array<{ symbol: string; label: string }>;
  /** Natural-language paraphrase of a single KIF formula in `language`. */
  renderNl(kif: string, language: string): string;
  flushSession(session: string): void;
}
