use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Custom version string surfacing the build provenance embedded by
/// `crates/cli/build.rs`.  Renders as e.g.
///
/// ```text
/// sumo 1.0.0 (release build, commit a1b2c3d4e5f6, aarch64-apple-darwin)
/// ```
///
/// The `build kind` is the same flag `sumo update` reads to decide
/// between self-replace and "rebuild from source".  The target
/// triple lets us pick the right release archive when self-updating
/// (and tells you which platform binary you're running on a bug
/// report).  Three pieces of provenance, one source of truth.
const VERSION_LINE: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("SUMO_BUILD_KIND"),
    " build, commit ",
    env!("SUMO_BUILD_COMMIT"),
    ", ",
    env!("SUMO_BUILD_TARGET"),
    ")",
);

#[derive(Parser)]
#[command(
    name = "sumo",
    about = "Parse, validate, translate, and query SUMO KIF knowledge bases",
    after_help = "Reference:\n  Niles, I., and Pease, A.  2001.  Towards a Standard Upper Ontology.  In\n  Proceedings of the 2nd International Conference on Formal Ontology in\n  Information Systems (FOIS-2001), Chris Welty and Barry Smith, eds,\n  Ogunquit, Maine, October 17-19, 2001.  Also see http://www.ontologyportal.org",
    version = VERSION_LINE,
)]
pub struct Cli {
    /// Logging verbosity (-v = info, -vv = debug, -vvv = trace).
    #[arg(short = 'v', long = "verbose", action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Suppress all warnings
    #[arg(short = 'q', long = "quiet", global = true)]
    pub quiet: bool,

    /// Path to SigmaKEE config.xml or the directory containing it.
    #[arg(long, value_name = "PATH", global = true)]
    pub config: Option<PathBuf>,

    /// Whether to use the system's sigma config.xml to configure the runtime
    #[arg(short = 'c', global = true)]
    pub enable_config: bool,

    /// Knowledge base name from config.xml to load.
    /// Requires -c (config mode) to be active.
    #[arg(long, value_name = "NAME", global = true, requires = "enable_config")]
    pub kb: Option<String>,

    /// Warning control (mimics GCC).
    /// By default, semantic errors are warnings.
    /// Use '-W all' to treat all as errors.
    /// Use `-W <CODE>` (e.g., `-W E005`) to treat a specific one as an error.
    #[arg(short = 'W', long = "warning", value_name = "CODE_OR_ALL", global = true)]
    pub suppress: Vec<String>,

    // -- Universal source-selection flags -------------------------------------
    //
    // Every KB-touching subcommand needs these, so they live on the
    // top-level `Cli` with `global = true` — accepted either before
    // OR after the subcommand name.  The `KbArgs` helper struct
    // below still carries the same fields (via `#[arg(skip)]`) so
    // every `run_*` handler can receive a single `KbArgs` value;
    // `main.rs` populates those fields from the top-level parse.

    /// KIF file to load into the knowledge base (repeatable).
    #[arg(short = 'f', long = "file", value_name = "FILE", global = true)]
    pub files: Vec<PathBuf>,

    /// Directory whose *.kif files are loaded into the knowledge base (repeatable).
    #[arg(short = 'd', long = "dir", value_name = "DIR", global = true)]
    pub dirs: Vec<PathBuf>,

    /// Path to the LMDB database directory.
    /// Defaults to `./sumo.lmdb` in the current working directory.
    #[arg(long, value_name = "DIR", default_value = "./sumo.lmdb", global = true)]
    pub db: PathBuf,

    /// Skip the LMDB database entirely -- do not open or warn about it.
    /// Useful when running without a pre-built database.
    #[arg(long, global = true)]
    pub no_db: bool,

    #[command(subcommand)]
    pub command: Cmd,
}

/// Shared arguments for database and KIF-source selection.
///
/// The universal source flags (`-f`, `-d`, `--db`, `--no-db`) live
/// on the top-level [`Cli`] with `global = true`.  This struct's
/// equivalent fields carry `#[arg(skip)]` so clap doesn't try to
/// re-register them; `main.rs` populates those fields from the
/// top-level parse before handing the struct to a `run_*` handler.
///
/// The one genuinely per-subcommand field is `vampire`, which only
/// the prover-driven subcommands (`ask`, `test`, `debug`, `serve`)
/// expose.  Those subcommands flatten `KbArgs` into their variant
/// so clap surfaces `--vampire` in their help text.  Subcommands
/// that don't touch the prover (`validate`, `translate`, `load`,
/// `man`) don't flatten `KbArgs` and therefore don't advertise
/// `--vampire`; they still receive a fully-populated `KbArgs` from
/// `main.rs` because the struct's shape hasn't changed.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct KbArgs {
    /// KIF file to load into the knowledge base (repeatable).
    ///
    /// Populated from the top-level `Cli::files` by `main.rs`; not
    /// re-parsed here (clap sees `#[arg(skip)]`).
    #[arg(skip)]
    pub files: Vec<PathBuf>,

    /// Directory whose *.kif files are loaded (repeatable).
    /// Populated from `Cli::dirs`.
    #[arg(skip)]
    pub dirs: Vec<PathBuf>,

    /// Path to the LMDB database directory.  Populated from
    /// `Cli::db`.  The "./sumo.lmdb" default is defined on the
    /// top-level flag; here it falls back to `PathBuf::new()` when
    /// `main.rs` doesn't populate it (shouldn't happen in practice).
    #[arg(skip)]
    pub db: PathBuf,

    /// Skip the LMDB database entirely.  Populated from `Cli::no_db`.
    #[arg(skip)]
    pub no_db: bool,

    // #[cfg(feature = "cnf")]
    // /// Hard upper bound on CNF clauses per formula.
    // /// Overrides the SUMO_MAX_CLAUSES environment variable.
    // #[arg(long, value_name = "N", default_value_t = 10_000)]
    // pub max_clauses: usize,

    /// Path to the Vampire executable (default: 'vampire' on PATH).
    ///
    /// Only the prover-driven subcommands (`ask`, `test`, `debug`,
    /// `serve`) flatten `KbArgs` and thus expose this flag.  Other
    /// subcommands leave it `None` — their run handlers ignore the
    /// field anyway.
    #[arg(long, value_name = "PATH")]
    pub vampire: Option<PathBuf>,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Parse KIF file(s) into the LMDB database and validate all formulas.
    ///
    /// KIF files are treated as initialisation scripts (like SQL migrations).
    /// After a successful run the database at --db is the canonical store.
    /// Subsequent `ask` and `translate` commands read from the database.
    Validate {
        /// Formula to validate against the database.  If omitted, validates
        /// every formula already in the database.
        formula: Option<String>,

        /// Perform parse-only validation -- skip semantic checks entirely.
        #[arg(long)]
        parse: bool,

        /// Do not semantically validate the loaded KB files; assume they are
        /// correct and only check the inline formula (if provided).
        /// Parse errors in KB files are still reported.
        #[arg(long)]
        no_kb_check: bool,
        // Source selection (`-f`, `-d`, `--db`, `--no-db`) lives
        // on the top-level `Cli`; main.rs synthesises a `KbArgs`
        // for this variant's `run_validate` call.
    },

    /// Run a KIF conjecture through Vampire against the knowledge base.
    #[cfg(feature = "ask")]
    Ask {
        /// KIF conjecture to prove.  May also be supplied via stdin.
        formula: Option<String>,

        /// Assert a KIF formula into the KB before asking (repeatable).
        #[arg(short = 't', long = "tell", value_name = "KIF")]
        tell: Vec<String>,

        /// Vampire proof-search timeout in seconds.
        #[arg(long, value_name = "SECS", default_value_t = 30)]
        timeout: u32,

        /// Session key for --tell assertions and TPTP hypothesis filtering.
        #[arg(long, value_name = "KEY", default_value = "default")]
        session: String,

        /// Prover backend: 'subprocess' (default, requires vampire on PATH) or
        /// 'embedded' (in-process, requires the integrated-prover feature).
        #[arg(long, value_name = "BACKEND", default_value = "subprocess")]
        backend: String,

        /// TPTP language variant: 'fof' (default) or 'tff'.
        #[arg(long, value_name = "LANG", default_value = "fof")]
        lang: String,

        #[command(flatten)]
        kb: KbArgs,

        /// Write the generated TPTP to FILE (for debugging).
        /// When omitted, TPTP is piped directly to Vampire via stdin.
        #[arg(short = 'k', long, value_name = "FILE")]
        keep: Option<PathBuf>,

        /// Print the proof steps when Vampire finds one.
        ///
        /// Accepted values:
        /// - `tptp`: raw TSTP proof section as emitted by Vampire (no translation).
        /// - `kif`:  SUO-KIF pretty-print of each step's formula.
        /// - any SUMO language symbol (e.g. `EnglishLanguage`,
        ///   `ChineseLanguage`): natural-language rendering using the KB's
        ///   `format` / `termFormat` relations.  Steps whose formulas reference
        ///   a symbol that lacks a `format` or `termFormat` entry in the
        ///   chosen language fall back to the KIF line with a warning listing
        ///   the missing specifiers.
        ///
        /// Omit the flag to suppress proof output.  Specifying `--proof`
        /// without a value is rejected (clap: value required).
        #[arg(long, value_name = "FORMAT")]
        proof: Option<String>,

        /// Print a timing breakdown of the major pipeline phases.
        #[arg(long)]
        profile: bool,
    },

    /// Translate KIF formula(s) or a full KB to TPTP.
    ///
    /// Without --db (or with --db pointing to a non-existent path) and with
    /// -f / -d files supplied: parses in-memory and emits TPTP FOF (legacy mode).
    ///
    /// With an existing --db: reads CNF from the database and emits TPTP CNF.
    /// Any -f / -d / inline formula is treated as a session assertion.
    Translate {
        /// Formula to translate.  May also be supplied via stdin.
        formula: Option<String>,

        /// TPTP language variant to emit (legacy in-memory mode only).
        #[arg(long, value_name = "LANG", default_value = "fof")]
        lang: String,

        /// Emit numeric literals as-is instead of encoding them as n__N tokens.
        #[arg(long)]
        show_numbers: bool,

        /// Emit a `% <original KIF>` comment before each TPTP formula.
        #[arg(long)]
        show_kif: bool,

        /// Session key controlling which assertions appear as TPTP hypotheses.
        #[arg(long, value_name = "KEY")]
        session: Option<String>,
        // Source selection lives on the top-level `Cli`.
    },

    /// Run one or more KIF test files (*.kif.tq).
    #[cfg(feature = "ask")]
    Test {
        /// Path(s) to .kif.tq files or directories containing them.
        /// Accepts multiple arguments and shell-expanded globs.
        #[arg(value_name = "PATH", num_args = 1..)]
        paths: Vec<PathBuf>,

        #[command(flatten)]
        kb: KbArgs,

        /// Write the generated TPTP to FILE (for debugging).
        /// When omitted, TPTP is piped directly to Vampire via stdin.
        #[arg(short = 'k', long, value_name = "FILE")]
        keep: Option<PathBuf>,

        /// Prover backend: 'subprocess' (default) or 'embedded'.
        #[arg(long, value_name = "BACKEND", default_value = "subprocess")]
        backend: String,

        /// TPTP language variant: 'fof' (default) or 'tff'.
        #[arg(long, value_name = "LANG", default_value = "fof")]
        lang: String,

        /// Override the per-test timeout (seconds). Overrides any (time N) directive in the test file.
        #[arg(long, value_name = "SECS")]
        timeout: Option<u32>,

        /// Print a timing breakdown of the major pipeline phases.
        #[arg(long)]
        profile: bool,
    },

    /// Parse KIF file(s) and commit them to the LMDB database.
    ///
    /// This is the only command that writes to the database.
    /// Validates all loaded formulas before committing -- parse errors or
    /// promoted warnings (-W) abort the commit and leave the database unchanged.
    /// If no files are given, the database is created/opened but left empty.
    ///
    /// Default behaviour vs `--flush`:
    /// - Default: the DB is opened *in place*; per-file reconcile
    ///   diffs disk content against DB content under the same file
    ///   tag and commits only the delta (added + removed).  Files
    ///   unrelated to the `-f` / `-d` set are left untouched.
    /// - `--flush`: drops the entire DB and rebuilds it from just
    ///   the `-f` / `-d` set.  With no files the result is an
    ///   empty database — useful as a reset.
    Load {
        /// Recanonicalise the database: drop every persisted axiom
        /// and rewrite the DB from just the supplied `-f` / `-d`
        /// files.  With no files, leaves an empty database.  Use
        /// when the DB has accumulated stale axioms from earlier
        /// loads and you want to start clean.
        #[arg(long)]
        flush: bool,
        // Source selection lives on the top-level `Cli`.
    },

    /// Show documentation, signatures, and taxonomy for a symbol -- the
    /// KIF-native equivalent of `man`.
    ///
    /// The information is extracted from the ontology-level relations
    /// `documentation`, `termFormat`, `format`, plus the `subclass` /
    /// `instance` / `domain` / `range` declarations.  KIF has no
    /// syntactic doc-comment; everything man shows is first-class
    /// ontology data.
    Man {
        /// Symbol to describe (e.g. `Human`, `subclass`, `instance`).
        symbol: String,

        /// Language tag to filter documentation / term-format entries
        /// (e.g. `EnglishLanguage`).  When omitted, entries in all
        /// languages are shown.
        #[arg(long, value_name = "LANG")]
        lang: Option<String>,

        /// Disable the interactive pager; print the man page directly
        /// to stdout.  The pager is also disabled automatically when
        /// stdout is not a TTY (e.g. when piping to another program)
        /// or when the `NO_PAGER` environment variable is set.
        #[arg(long = "no-pager", short = 'P')]
        no_pager: bool,
        // Source selection lives on the top-level `Cli`.
    },

    /// Consistency-check a single loaded KIF file against the rest of
    /// the knowledge base via Vampire, surfacing any axioms that
    /// contradict each other.
    ///
    /// The flow is:
    ///   1. Collect the sentences of `<FILE>` (must already be in the
    ///      KB — pass `-f` / `-d` the same way as other subcommands).
    ///   2. Randomly subsample by `--thoroughness` (default 1.0 = all).
    ///   3. SInE-expand from the sampled sentences' symbols at
    ///      tolerance `--scope` (default: crate SInE default, usually
    ///      2.0).  This pulls in every axiom the sampled sentences
    ///      semantically depend on, across every other loaded file.
    ///   4. Feed the union (sampled ∪ SInE-expanded) to Vampire with
    ///      NO conjecture — pure axiom-satisfiability.
    ///   5. If Vampire reports Unsatisfiable / ContradictoryAxioms,
    ///      parse the refutation proof and trace each axiom-role step
    ///      back to its source `file:line`.
    ///   6. Report: verdict, contradictory axioms (if any), and the
    ///      set of other files whose axioms SInE pulled in.
    ///
    /// Uses TPTP FOF (TFF is not currently wired through `debug`).
    #[cfg(feature = "ask")]
    Debug {
        /// Path to a `.kif` file already loaded into the KB (via `-f`
        /// or via the LMDB store).  The file tag is matched
        /// case-sensitively against the loaded tags; pass the same
        /// path form you used when loading.
        file: PathBuf,

        /// Fraction of the file's root sentences to sample for the
        /// consistency check, in (0.0, 1.0].  `1.0` uses every
        /// sentence; `0.5` a random half; `0.1` a random tenth.
        /// Smaller values run faster at the cost of coverage — the
        /// SInE expansion step then pulls in a proportionally smaller
        /// relevant axiom set.
        #[arg(long, value_name = "F", default_value_t = 1.0)]
        thoroughness: f32,

        /// SInE tolerance factor (`≥ 1.0`) for the axiom expansion
        /// step.  Higher values pull in more axioms — more thorough
        /// but more expensive.  Values below 1.0 are clamped.  When
        /// omitted, uses the crate default (usually 2.0; overridable
        /// at sumo-kb build time via `SINE_TOLERANCE`).
        #[arg(long, value_name = "F")]
        scope: Option<f32>,

        /// Vampire proof-search timeout in seconds.
        #[arg(long, value_name = "SECS", default_value_t = 60)]
        timeout: u32,

        /// Write the generated TPTP to FILE (for debugging).  When
        /// omitted, TPTP is piped directly to Vampire via stdin.
        #[arg(short = 'k', long, value_name = "FILE")]
        keep: Option<PathBuf>,

        /// Print the full refutation proof when Vampire finds one.
        ///
        /// Accepted values (same as `sumo ask --proof`):
        /// - `tptp`: raw TSTP proof section as emitted by Vampire.
        /// - `kif`:  SUO-KIF pretty-print of each step's formula.
        /// - any SUMO language symbol (e.g. `EnglishLanguage`,
        ///   `ChineseLanguage`): natural-language rendering via the
        ///   KB's `format` / `termFormat` relations.  Steps with a
        ///   symbol lacking a spec in the chosen language fall back
        ///   to the KIF line with a warning listing what was missing.
        ///
        /// Omit the flag to suppress the full proof — the compact
        /// "contradiction summary" (axioms contributing to the
        /// refutation, one per line) is always shown.  The two are
        /// complementary: the summary is a quick scan, `--proof` is
        /// the full derivation.
        #[arg(long, value_name = "FORMAT")]
        proof: Option<String>,

        #[command(flatten)]
        kb: KbArgs,
    },

    /// Run as a persistent kernel: read newline-delimited JSON
    /// requests from stdin and write responses to stdout.  The
    /// process owns one long-lived `KnowledgeBase` so every
    /// request amortises the load cost.
    ///
    /// Designed for editor integrations (e.g. the VSCode extension
    /// that spawns `sumo-kernel`) -- the LSP analog of a Jupyter
    /// kernel.  See `crates/native/src/cli/serve.rs` for the wire
    /// format; current methods are `tell`, `ask`, `shutdown`.
    #[cfg(feature = "server")]
    Serve {
        #[command(flatten)]
        kb: KbArgs,
    },

    /// Update the `sumo` binary to the latest official release, OR
    /// (for source builds) report the latest available version and
    /// recommend the right rebuild incantation.
    ///
    /// The dispatch is determined at compile time by the
    /// `SUMO_BUILD_KIND` env var the build script reads.  Release
    /// CI sets it to `release`; everything else defaults to `source`.
    /// Source builds intentionally never overwrite themselves —
    /// replacing a developer's local build with an unrelated
    /// upstream binary would surprise them.
    Update {
        /// Don't apply the update — just check upstream and report.
        #[arg(long)]
        check: bool,
    },
}
