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
  proof: Array<{ index: number; rule: string; premises: number[]; kif: string }>;
  /** The proof as a Graphviz DOT digraph — always valid, even when `proof` is empty. */
  graphviz: string;
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
  /** One entry per distinct contradiction, each with its own DOT digraph. */
  contradictions: Array<{ steps: AuditStep[]; graphviz: string }>;
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
 * no source origin (e.g. synthetic/CNF sentences). */
export interface ManPageRef {
  position: number | null;
  kif: string;
  file: string | null;
  line: number | null;
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
  /** The underlying raw binding (WasmNativeProver | WasmKnowledgeBase). */
  readonly kb: unknown;
  configure(config: Config): this;
  ingest(source: Source): Promise<LoadReport>;
  tell(kif: string, session?: string): TellResult;
  /** Native backend → AskResult; TranslationOnly backend (with hook) → string. */
  ask(queryKif: string, opts?: AskOpts): AskResult | string;
  /** Native backend only: consistency-audit the whole KB. `limit` caps distinct contradictions (default 5). */
  auditConsistency(limit?: number): AuditResult;
  translate(opts?: TranslateOpts): string;
  lookup(pattern: string): string[];
  validate(): Diagnostic[];
  validateFormula(kif: string): Diagnostic[];
  search(query: string, opts?: SearchOpts): SearchHit[];
  manpage(symbol: string): ManPage | null;
  flushSession(session: string): void;
}
