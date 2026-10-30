/**
 * SDK-shaped facade over the raw wasm bindings — mirrors `sigmakee-rs-sdk`'s
 * `Session` / `Source` / `Backend` / `Config` for the browser.
 */
import { Config, Session as WasmSession } from "./engine";

export { Config };
/** The raw wasm binding behind {@link Session.kb}. */
export type { WasmSession };

/** Outcome of loading a {@link Source}. */
export interface LoadReport {
  loaded: number;
  files: string[];
  errors: string[];
}

/** Native-prover result (from a Native-backed {@link Session.ask}). */
export interface AskResult {
  status:
    | "Proved"
    | "Disproved"
    | "Consistent"
    | "Inconsistent"
    | "Timeout"
    | "InputError"
    | "Unknown";
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
  /** Whole-proof TPTP material belonging to no single step (e.g. TFF's
   *  type-declaration preamble), to show once ahead of the per-step `tptp`.
   *  Empty for untyped dialects and when `proof` is empty. */
  proof_tptp_prologue: string;
  /** The exact problem text handed to the external prover on the last run --
   *  Vampire backend with `Config.keepTptp` only, absent otherwise. */
  input_tptp?: string;
}

/** One step of a cited contradiction derivation (see {@link AuditResult}). */
export interface AuditStep {
  index: number;
  rule: string;
  premises: number[];
  kif: string;
  /** This step reconstructed as TPTP (framed `cnf`/`fof`/`tff`/... text), or
   *  an inline `;` comment explaining why it couldn't be represented. */
  tptp: string | null;
  /** `null` for derived/anonymous steps that don't trace to an input axiom. */
  file: string | null;
  line: number | null;
}

/** Which slice of the sampled audit's sweep to check (see {@link Session.auditConsistency}). */
export interface AuditRequest {
  /** Recheck a tracked source formula instead of an unstable sweep position. Native only. */
  focus?: { file: string; source: string; kif: string; unchanged: boolean };
  /** Replay recorded contradictions: audit exactly these cited axioms
   *  together (finding keys or 16-hex sentence ids, see
   *  {@link AuditFinding.key}) instead of a sweep. Throws when one of them
   *  is no longer in the KB. */
  culprits?: string[];
  /** Seed of the pseudorandom sweep order (default 0). */
  seed?: number;
  /** Sweep position to start from (default 0). */
  step?: number;
  /** Sentences to check (default 1). */
  count?: number;
  /** Sentences per subproblem (default 1). */
  batch?: number;
  /** Stop after this many distinct contradictions (default 5). */
  limit?: number;
  /** A loaded file tag to restrict the sweep to; omitted = the whole KB. */
  scope?: string;
  /** Most axioms one neighborhood may select; omitted = the config's selection budget. */
  budget?: number;
  /** SInE expansion depth; omitted = unlimited / the config's. */
  depth?: number;
  /** Audit only the sentence enclosing UTF-8 byte `offset` of loaded file
   *  `file` instead of a sweep (`seed`/`scope` are ignored). Throws when no
   *  sentence is there. */
  at?: { file: string; offset: number };
}

/** One focus sentence of an audit subproblem (see {@link Session.sentenceAt}). */
export interface AuditFocus {
  kif: string;
  file: string | null;
  line: number | null;
}

/** `Config.selection`: SInE axiom-selection parameters (the engine's
 *  `SineParams`, snake_case keys). Setting it takes a partial object. */
export interface SelectionParams {
  /** Trigger tolerance (>= 1); used only when `auto_budget` is `null`. */
  tolerance: number;
  /** SInE expansion depth; `null` = unlimited. */
  depth_limit: number | null;
  /** Largest selection the auto-tolerance picks; `null` = fixed `tolerance`. */
  auto_budget: number | null;
  /** Skip selection: the whole KB. */
  select_all: boolean;
  /** Widen/narrow the budget between reruns from prover feedback. */
  autoscale: boolean;
}

/** One of `Config.strategyPresets()`: a named native-prover `Strategy`
 *  (snake_case keys, the CLI's `--strategy` JSON shape). */
export interface StrategyPreset {
  name: string;
  strategy: Record<string, unknown>;
}

/** Why one audit subproblem stopped without a verdict. `StepLimit` is
 *  native-only (the given-clause cap, which only binds without a time limit). */
export type AuditStopReason =
  "TimeLimit" | "StepLimit" | "IncompleteLoad" | "GaveUp";

/** One subproblem of a sampled audit. */
export interface AuditBatch {
  /** The sentences whose SInE neighborhood was checked. */
  focus: AuditFocus[];
  /** `Consistent` means only that this neighborhood saturated clean. */
  status: "Consistent" | "Inconsistent" | "Timeout" | "InputError" | "Unknown";
  stop_reason: AuditStopReason | null;
  elapsed_ms: number;
}

/** Sampled consistency-audit result (from {@link Session.auditConsistency}). */
export interface AuditResult {
  /** Never `Consistent`: unchecked neighborhoods remain. */
  status: "Inconsistent" | "Unknown";
  inconsistent: boolean;
  raw_output: string;
  seed: number;
  step: number;
  /** Where to resume the sweep. */
  next_step: number;
  /** Size of the whole sweep (eligible sentences). */
  total: number;
  batches: AuditBatch[];
  /** One entry per distinct contradiction, each with its own DOT digraph and prose. */
  contradictions: Array<{
    steps: AuditStep[];
    graphviz: string;
    /** This contradiction's derivation narrated as connected English prose. */
    prose: string;
    /** Symbols the prose showed by bare name (no `format`/`termFormat`). */
    prose_missing: string[];
    /** See {@link AskResult.proof_tptp_prologue}. */
    proof_tptp_prologue: string;
  }>;
}

/** One distinct contradiction of an {@link AuditResult}. */
export type AuditContradiction = AuditResult["contradictions"][number];

/** An axiom a recorded contradiction cites (see {@link AuditFinding}). */
export interface AuditCulprit {
  /** The sentence id as 16 hex digits. */
  sid: string;
  kif: string;
  file: string | null;
  line: number | null;
}

/** A contradiction recorded in the KB's findings cache (see
 *  {@link Session.auditFindings}). Recorded by every audit and by
 *  {@link Session.importContradictions}; dropped automatically when one of
 *  its cited axioms leaves the KB. */
export interface AuditFinding {
  /** Stable identity: the sorted culprit sids joined by `+`. Pass it in
   *  {@link AuditRequest.culprits} to replay. */
  key: string;
  culprits: AuditCulprit[];
  /** `true` when every leaf of the derivation cites a source axiom. */
  complete: boolean;
  /** The session whose assertions the proof reasoned in, if any. */
  session: string | null;
  /** `"native"`, `"vampire"`, `"eprover"`, or the import label given. */
  origin: string;
  /** Unix milliseconds when it was recorded. */
  found_at_ms: number;
  contradiction: AuditContradiction;
}

/** An axiom as a contradiction report cites it. */
export interface CitedAxiom {
  file: string | null;
  line: number | null;
  kif: string;
}

/** What {@link Session.importContradictions} did with a report. */
export interface ImportOutcome {
  /** Findings recorded (or already present) for the report's entries. */
  imported: AuditFinding[];
  rejected: Array<{
    axioms: CitedAxiom[];
    /** The report's derivation as parsed: `{ rule, premises, kif, axiom }`
     *  per step, `axiom` indexing `axioms`; empty when it carried none. */
    steps: Array<{
      rule: string;
      premises: number[];
      kif: string;
      axiom: number | null;
    }>;
    reason: string;
    /** The cited axioms not present in the KB. */
    missing: CitedAxiom[];
  }>;
}

/** Which engine a {@link Session} drives (browser subset of the SDK `Backend`). */
export const Backend: {
  readonly Native: "native";
  /** Vampire through the engine's external prover layer and the
   *  `__sigmaRunVampireSync` bridge (see {@link installVampireBridge}). */
  readonly Vampire: "vampire";
  readonly E: "e";
  readonly TranslationOnly: "translation";
};
export type Backend = (typeof Backend)[keyof typeof Backend];

/** The synchronous bridge the engine's Vampire runner calls: run Vampire on
 *  `tptp` with the command line `args` (it already carries `-t`), giving up
 *  after `timeoutMs` (0 = none), and return its captured output. */
export type VampireBridge = (
  tptp: string,
  args: string,
  timeoutMs: number,
) => { stdout: string; stderr: string; code?: number };

/** Install the bridge the engine's Vampire runner calls; a legacy
 *  `(tptp) => string` hook returning the raw transcript is accepted too.
 *  `null` uninstalls. */
export function installVampireBridge(
  bridge: VampireBridge | ((tptp: string) => string) | null,
): void;

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
 * defaults to `"  "` (2 spaces), matching the core Rust formatter
 * (`crates/core/src/parse/kif/dis.rs`) so this fallback never visibly
 * disagrees with the LSP's canonical layout.
 */
export function formatKif(text: string, opts?: { indentUnit?: string }): string;

/** A parsed `.kif.tq` test file (see {@link parseTest}). */
export interface ParsedTest {
  name: string;
  /** `(note ...)`, else the file name (see `noteGiven`). */
  note: string;
  /** `(time N)` in seconds, else the harness default (see `timeGiven`). */
  timeout: number;
  /** Whether `note` / `timeout` came from directives rather than defaults. */
  noteGiven: boolean;
  timeGiven: boolean;
  /** Every `(category ...)` value, in source order. */
  categories: string[];
  /** The `(query …)` formula, or `null` for an axioms-only file. */
  queryKif: string | null;
  /** Hypotheses as newline-joined KIF. */
  axiomKif: string;
  /** `(answer yes|no)` → true/false; `null` when absent or bindings-style. */
  expectedProof: boolean | null;
  /** Bindings-style `(answer …)` values, when present. */
  expectedAnswer: string[] | null;
  /** `(include …)` directives — paths preserved as metadata; dependency loading is deferred. */
  includes: string[];
  /** Minimum external-prover translation language; null when absent. */
  minLang: "fof" | "tff" | "thf" | null;
}

/** The SUMO symbol constants the engine was compiled with (its
 *  `.cargo/config.toml` `[env]` table). */
export interface SumoSymbols {
  /** The language assumed for rendering / documentation lookups when none is named. */
  defaultLanguage: string;
  /** The class whose instances are the documentation languages. */
  naturalLanguageClass: string;
}

/** The build's {@link SumoSymbols} (requires {@link init}). */
export function sumoSymbols(): SumoSymbols;

/** Parse a `.kif.tq` test file (requires {@link init}); throws on malformed input. */
export function parseTest(name: string, text: string): ParsedTest;
/** TPTP-dialect counterpart to {@link parseTest}: parses a `.p`/`.tptp` problem.
 *  `remap` (default `false`) decodes SUMO-mangled symbol names. */
export function parseTptpTest(
  name: string,
  text: string,
  remap?: boolean,
): ParsedTest;

/** Render an Ask/Tell pair as `.kif.tq` text (pure; the inverse of {@link parseTest}). */
export function formatTest(opts?: {
  minLang?: "fof" | "tff" | "thf" | null;
  note?: string;
  /** `(time N)`; omitted when 0. */
  timeout?: number;
  /** One `(category ...)` directive each. */
  categories?: string[];
  /** One `(include ...)` directive each. */
  includes?: string[];
  assertions?: string;
  query?: string;
  /** `(answer yes|no)`; `null` writes no answer. */
  expectedProof?: boolean | null;
  /** Bindings-style `(answer A B ...)`; wins over `expectedProof` when non-empty. */
  expectedAnswer?: string[] | null;
}): string;

export interface AskOpts {
  session?: string;
  /** Parse `query` as TPTP instead of SUO-KIF. */
  tptp?: boolean;
  /** Vampire backend: run the prover for this call only (see {@link installVampireBridge}). */
  hook?: VampireBridge | ((tptp: string) => string);
}
export interface TranslateOpts {
  lang?: "fof" | "tff";
  hideNumbers?: boolean;
  session?: string;
}
export interface TellResult {
  ok: boolean;
  errors: string[];
}

export interface Diagnostic {
  severity: "error" | "warning" | "info" | "hint";
  kind: string; // coarse category, e.g. "semantic"
  code: string; // leaf id, e.g. "free-var-in-consequent"
  message: string;
  file: string; // source tag the sentence was loaded under
  line: number; // 1-based
  col: number; // 1-based
  end_line: number;
  end_col: number;
}

/** One labeled contribution to a {@link SearchHit.rank} score. */
export interface RankComponent {
  label: string;
  value: number;
}
/** One WordNet synset anchored to a SUMO symbol — e.g. for `Canine`:
 * `{ words: "dog, domestic dog, Canis familiaris", pos: "n", mapping:
 * "subsuming", suffix: "+", gloss: "a member of the genus Canis..." }`.
 * Not query-scoped: every synset anchored to the symbol is listed,
 * independent of how the symbol was found (see {@link SearchHit.wordnet} /
 * {@link ManPage.wordnet}). */
export interface WordNetMapping {
  /** The synset's id, `"<offset>-<pos>"` (see {@link WordNetSense.id}). */
  id: string;
  /** The synset's word(s) (near-synonyms share one synset), comma-joined. */
  words: string;
  /** WordNet part-of-speech tag: `"n"`, `"v"`, `"a"`, or `"r"`. */
  pos: string;
  /** How this synset is anchored to the symbol: `"equivalent"`,
   * `"subsuming"`, `"instance"`, or `"other"`. */
  mapping: string;
  /** The mapping-kind suffix in the mappings files' own notation (`=`, `+`,
   * `@`, or another character), for compact rendering. */
  suffix: string;
  /** The synset's gloss (definition). */
  gloss: string;
}
export interface SearchHit {
  symbol: string;
  kinds: ManKind[];
  source: string;
  language: string;
  text: string;
  /** For a WordNet-sourced hit (`source === "wn"`): the sense tag, e.g.
   * `"dog#n#1+"`. Empty for every other source. */
  sense: string;
  /** Relevance score, higher = better. Hits are returned sorted by this desc.
   * The sum of {@link rank_breakdown}'s values. */
  rank: number;
  /** The named contributions {@link rank} sums to -- e.g. a name-match tier,
   * a source tier, a match position, and a usage-frequency nudge for a text
   * hit; an anchor-strength tier and a most-frequent-sense bonus for a
   * WordNet hit. For surfacing *why* a hit ranked where it did (e.g. a hover
   * tooltip). */
  rank_breakdown: RankComponent[];
  /** Every WordNet synset anchored to {@link symbol}, regardless of this
   * hit's own {@link source} — populated whenever a lexicon is installed,
   * empty otherwise. A plain documentation-text hit still lists the
   * symbol's WordNet senses here, if it has any. */
  wordnet: WordNetMapping[];
}
/**
 * One taxonomic constraint on search hits (see {@link SearchOpts.taxonomy}),
 * keyed to a class name. `subclassOf`/`instanceOf` walk the `subclass` /
 * `instance` taxonomy; `rangeOf`/`rangeSubclassOf` are a schema probe over
 * relations' declared `range`/`rangeSubclass`.
 */
export type TaxConstraint =
  | { subclassOf: string }
  | { instanceOf: string }
  | { rangeOf: string }
  | { rangeSubclassOf: string };
export interface SearchOpts {
  kind?: string;
  language?: string;
  limit?: number;
  /** Return only WordNet synonym hits (see {@link Session.loadWordNet}). */
  wordnetOnly?: boolean;
  /** ANDed taxonomic constraints (every hit must satisfy all of them). */
  taxonomy?: TaxConstraint[];
}
/** The four mapping-file contents plus optional companions for {@link Session.loadWordNet}. */
export interface WordNetFiles {
  noun: string;
  verb: string;
  adj: string;
  adv: string;
  /** `index.sense` contents (most-frequent-sense ordering). */
  indexSense?: string;
  /** `noun.exc` + `verb.exc` contents, concatenated. */
  exceptions?: string;
}
/** A capped report list: `items` holds up to the caller's `limit` rows,
 * `total` is the true count -- so a caller can render "50 of 3,412" instead
 * of silently truncating. */
export interface Capped<T> {
  items: T[];
  total: number;
}
/** One synset row in a {@link WordNetDiagnostics} report: `term`/`suffix`
 * are empty for an `unmappedSynsets` row (it has no SUMO anchor). `file`/
 * `line` locate the record in its source `WordNetMappings30-*.txt` (or
 * local-extension) file. */
export interface WordNetSynsetRow {
  words: string;
  pos: string;
  term: string;
  suffix: string;
  file: string;
  /** 1-based line number within `file`. */
  line: number;
}
/** A loaded KB term with no WordNet synset mapped to it. */
export interface WordNetUnsynsetTerm {
  symbol: string;
  kinds: ManKind[];
}
/** A noun synset's hypernym edge whose SUMO anchors disagree with the KB's
 * own subclass taxonomy: `hypernymTerm` is not an ancestor of `term`.
 * `file`/`line` locate `word`'s (the offending, not the hypernym's) synset. */
export interface WordNetTaxonomyMismatch {
  word: string;
  term: string;
  hypernym_word: string;
  hypernym_term: string;
  file: string;
  /** 1-based line number within `file`. */
  line: number;
}
/** Mapping-kind x part-of-speech counts across the whole lexicon. */
export interface WordNetMappingCounts {
  equivalent: number;
  subsuming: number;
  instance: number;
  anti_subsuming: number;
  anti_instance: number;
  anti_equivalent: number;
  nouns: number;
  verbs: number;
  adjectives: number;
  adverbs: number;
}
/** WordNet<->KB diagnostics report (see {@link Session.wordnetDiagnostics}):
 * a port of Java Sigma's WordNet diagnostics page
 * (ontologyportal/sigma-rs#64). `missingTerms` is the flip side of a
 * {@link SearchHit} whose {@link SearchHit.kinds} came back empty -- this
 * report is where that gap becomes an itemized, browsable list. */
export interface WordNetDiagnostics {
  counts: WordNetMappingCounts;
  unmapped_synsets: Capped<WordNetSynsetRow>;
  missing_terms: Capped<WordNetSynsetRow>;
  terms_without_synsets: Capped<WordNetUnsynsetTerm>;
  taxonomy_mismatches: Capped<WordNetTaxonomyMismatch>;
}
/** A mapping-kind filter name for {@link Session.wordnetSenses}. */
export type WordNetMappingKind =
  | "unmapped"
  | "equivalent"
  | "subsuming"
  | "instance"
  | "anti_equivalent"
  | "anti_subsuming"
  | "anti_instance"
  | "other";
/** One `&%Term<suffix>` SUMO anchor on a {@link WordNetSense}. */
export interface WordNetAnchor {
  term: string;
  /** Mapping kind in the mapping files' notation: `=` equivalent, `+`
   * subsuming, `@` instance, or a rarer negated form. */
  suffix: string;
}
/** One synset as {@link Session.wordnetSenses} returns it. */
export interface WordNetSense {
  /** `"<offset>-<pos>"`, e.g. `"02084071-n"`. */
  id: string;
  /** WordNet part-of-speech tag: `"n"`, `"v"`, `"a"`, or `"r"`. */
  pos: string;
  /** Zero-padded 8-digit synset offset. */
  offset: string;
  /** Sense tag (`"dog#n#1"`) for a word lookup; empty otherwise. */
  label: string;
  words: string[];
  gloss: string;
  mappings: WordNetAnchor[];
  /** The `WordNetMappings30-*.txt` file holding this record. */
  file: string;
  /** 1-based line number within `file`. */
  line: number;
}
export interface DocBlock {
  language: string;
  text: string;
}
/** Where a signature slot's declaration comes from: on the symbol itself,
 * inherited from a `subrelation` ancestor, or nowhere. */
export type SortStatus = "declared" | "inherited" | "undeclared";
/** One domain/range slot of a relation's signature. `type` is the class
 * name (`null` only for an `undeclared` argument position within the
 * arity); `subclass` marks `domainSubclass` / `rangeSubclass` slots;
 * `inherited_from` names the `subrelation` ancestor whose own declaration
 * supplies the slot when `status` is `"inherited"`. */
export interface SortSig {
  /** Absent (or `null`) only when `status` is `"undeclared"`. */
  type?: string | null;
  subclass: boolean;
  status: SortStatus;
  /** Present only when `status` is `"inherited"`. */
  inherited_from?: string | null;
}
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
export type ManKind =
  "class" | "relation" | "function" | "predicate" | "instance" | "individual";
export interface ManPage {
  name: string;
  kinds: ManKind[];
  documentation: DocBlock[];
  /** Every WordNet synset anchored to this symbol — see
   * {@link SearchHit.wordnet}'s doc comment for the shape. Populated
   * whenever a lexicon is installed, empty otherwise. */
  wordnet: WordNetMapping[];
  term_format: DocBlock[];
  format: DocBlock[];
  parents: Array<{ relation: string; parent: string }>;
  children: Array<{ relation: string; parent: string }>;
  arity: number | null;
  /** One entry per argument position, 1-based, up to the declared arity
   * (or the last declared position when the arity is unknown). */
  domains: Array<SortSig & { position: number }>;
  range: SortSig | null;
  appears_in_count: number;
  consequent_count: number;
  references: ManPageRef[];
}

/** Summary counts describing the loaded KB, as `Session.kb.stats()` returns
 *  them (the raw binding is wasm-bindgen generated, so it is typed `any`
 *  there). `documented`/`labeled` divide by `symbols` for a coverage
 *  percentage. `rules_first_order` + `rules_higher_order` always sum to
 *  `rules` -- a rule is higher-order when a formula (a relation/operator/
 *  predicate-variable application) occurs anywhere as an argument in its
 *  tree, rather than only nested inside a logical operator's own arguments.
 *  `rules_higher_order_categories` breaks the higher-order rules down by the
 *  configured predicate categories (temporal, deontic, epistemic); a rule may
 *  land in several, and `rules_higher_order_other` counts those in none. */
export interface KbStats {
  files: number;
  symbols: number;
  axioms: number;
  rules: number;
  rules_first_order: number;
  rules_higher_order: number;
  rules_higher_order_categories: Array<{ category: string; rules: number }>;
  rules_higher_order_other: number;
  classes: number;
  instances: number;
  relations: number;
  predicates: number;
  functions: number;
  documented: number;
  labeled: number;
  doc_languages: Array<{ language: string; documented: number }>;
  term_languages: Array<{ language: string; documented: number }>;
}

/** How a file's top-level formulas break down by shape, as part of
 *  {@link FileStats.axiom_kinds}. Every formula counted in
 *  `FileStats.axioms` falls into exactly one bucket: `documentation` is a
 *  `documentation`/`termFormat`/`format` entry, `typing` a taxonomy
 *  declaration (`instance`/`subclass`/`subrelation`/... ), `conditionals` a
 *  rule (top-level `=>`/`<=>`), and `facts` everything else. */
export interface FileAxiomKindCounts {
  documentation: number;
  typing: number;
  conditionals: number;
  facts: number;
}

/** One file's edit-relevant KB footprint, as `Session.kb.fileStats(file)`
 *  returns it (the raw binding is wasm-bindgen generated, so it is typed
 *  `any` there); `null` when `file` has no root sentences (not loaded, or
 *  loaded but empty). `terms_unique` counts terms from `terms` that occur in
 *  no other loaded file. `depends_on` lists other files this one relies on:
 *  for each term it uses without itself declaring a type for (see
 *  `axiom_kinds`), the file(s) elsewhere in the KB that do. */
export interface FileStats {
  file: string;
  axioms: number;
  axiom_kinds: FileAxiomKindCounts;
  terms: number;
  terms_unique: number;
  depends_on: string[];
}

/** Browser analogue of the SDK's `Session`. */
export class Session {
  constructor(opts?: { backend?: Backend; config?: Config });
  readonly backend: Backend;
  /** The underlying raw wasm Session binding. */
  readonly kb: WasmSession;
  configure(config: Config): this;
  /** `{ promote: false }` ingests only (search/man pages work); call `promote` later. */
  ingest(source: Source, opts?: { promote?: boolean }): Promise<LoadReport>;
  /** Promote an ingested source (by tag) into the axiom base. Native backend only. */
  promote(tag: string): string[];
  /** Freeze the whole KB (promoted axioms included) to a portable byte buffer. Native backend only. */
  snapshot(): Uint8Array<ArrayBuffer>;
  /** Thaw a KB frozen by {@link Session.snapshot}, replacing this session in place. Native backend only. */
  restore(bytes: Uint8Array): void;
  /** `tptp` parses `text` as TPTP instead of SUO-KIF. */
  tell(text: string, session?: string, tptp?: boolean): TellResult;
  /** Prove with this session's backend (Native or Vampire); TranslationOnly throws. */
  ask(query: string, opts?: AskOpts): AskResult;
  /** Sampled consistency audit with this session's backend: check `request`'s slice of the KB's seeded sweep, one SInE neighborhood per subproblem. */
  auditConsistency(request?: AuditRequest): AuditResult;
  /** Every contradiction recorded in the KB's findings cache, newest first. */
  auditFindings(): AuditFinding[];
  /** The recorded finding with `key`, or `null`. */
  auditFinding(key: string): AuditFinding | null;
  /** Record a contradiction report's entries whose cited axioms are all
   *  loaded. `json` is a version 3 `sigma-proof-report` audit, the nightly
   *  `latest-contradictions.json`, or `sumo audit --json` output; `origin`
   *  labels the findings. Throws when the report cannot be read. */
  importContradictions(json: string, origin: string): ImportOutcome;
  /** Forget every recorded contradiction. */
  clearAuditFindings(): void;
  /** The root sentence enclosing UTF-8 byte `offset` of loaded file `file`, or `null`. */
  sentenceAt(file: string, offset: number): AuditFocus | null;
  /** The promoted axioms with a clause unifying with a clause of `formula`
   *  (clausified as an axiom, polarity ignored), best matches first. Throws
   *  on parse errors. */
  matchingAxioms(formula: string): AuditFocus[];
  translate(opts?: TranslateOpts): string;
  lookup(pattern: string): string[];
  validate(): Diagnostic[];
  validateFormula(kif: string): Diagnostic[];
  /** Ask/Tell pair validated in one scratch session against the live KB. */
  validateScratch(
    assertions: string,
    query: string,
  ): { assertions: Diagnostic[]; query: Diagnostic[] };
  search(query: string, opts?: SearchOpts): SearchHit[];
  /** WordNet<->KB diagnostics report over this session's installed lexicon
   * and current KB (see {@link WordNetDiagnostics}). Each itemized report
   * capped at `limit` rows (default 50). `null` when no lexicon is
   * installed ({@link Session.loadWordNet}). */
  wordnetDiagnostics(limit?: number): WordNetDiagnostics | null;
  /** One page of synsets plus the total: every synset for an empty query,
   * a word's senses (else the senses of every lemma it prefixes), a synset
   * id (`02084071`, `02084071-n`), or -- with `byTerm` -- every synset
   * anchored to a SUMO term. `kinds` keeps synsets with an anchor of one of
   * the named kinds, or `"unmapped"` ones; `pos` keeps one part of speech.
   * Empty without a lexicon. */
  wordnetSenses(
    query: string,
    opts?: {
      byTerm?: boolean;
      kinds?: WordNetMappingKind[];
      pos?: string;
      offset?: number;
      limit?: number;
    },
  ): Capped<WordNetSense>;
  /** Load the WordNet-SUMO lexicon from already-fetched mapping-file text
   * (the browser fetches; there is no filesystem here). Separate from KIF
   * ingestion — once loaded, `search` gains WordNet synonym hits. Returns
   * the number of synsets indexed. */
  loadWordNet(files: WordNetFiles): number;
  /** Drop the currently installed WordNet lexicon, if any — the inverse of
   * `loadWordNet`. A no-op if none was loaded. */
  clearWordNet(): void;
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
  /** Natural-language paraphrase of a single KIF formula in `language`.
   *  `genericVars` renders variables as generic noun phrases ("an entity")
   *  instead of `?Var`. */
  renderNl(kif: string, language: string, genericVars?: boolean): string;
  flushSession(session: string): void;
}
