use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Version string embedding build provenance.  Renders as e.g.
///
/// ```text
/// sumo 1.0.0 (release build, commit a1b2c3d4e5f6, aarch64-apple-darwin)
/// ```
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

    /// Suppress ANSI styling (colors, bold).  Useful for plain-text logs,
    /// scripting, CI pipelines, or terminals that mangle escapes.
    #[arg(long = "ugly", global = true)]
    pub ugly: bool,

    /// Print a per-phase timing breakdown after the command runs.
    /// Works with every subcommand: a process-global aggregator captures
    /// the instrumented phases (file load, promote, SInE, persist,
    /// saturation, …) and prints the totals at the end, sorted by cost.
    #[arg(long = "profile", global = true)]
    pub profile: bool,

    /// Path to SigmaKEE config.xml or the directory containing it.
    #[arg(long, value_name = "PATH", global = true)]
    pub config: Option<PathBuf>,

    /// Skip config.xml entirely: every `KBManager` setting comes from its
    /// CLI-flag/hardcoded default instead of the on-disk config. Without this,
    /// config.xml is always read from `--config` / `$SIGMA_HOME` / the default
    /// location (when present) for its *preferences* (sumokbname, editDir,
    /// prover settings, …) — no `-c` needed for that.
    #[arg(long = "no-config", global = true)]
    pub no_config: bool,

    /// Also load the active KB's constituent files declared in config.xml
    /// (its `<kb>` section) into the session — the equivalent of passing
    /// `-f`/`-d` for each one. Config.xml's *preferences* (sumokbname,
    /// editDir, thoroughness, …) apply regardless of this flag; it only
    /// controls whether the ontology's files themselves get ingested.
    #[arg(short = 'c', global = true)]
    pub load_kb: bool,

    /// Knowledge base name from config.xml to select (drives the LMDB path
    /// and, with `-c`, which constituents load). Ignored under `--no-config`.
    #[arg(long, value_name = "NAME", global = true)]
    pub kb: Option<String>,

    /// Warning control (mimics GCC).
    /// By default, semantic errors are warnings.
    /// Use '-W all' to treat all as errors.
    /// Use `-W <CODE>` (e.g., `-W E005`) to treat a specific one as an error.
    #[arg(short = 'W', long = "warning", value_name = "CODE_OR_ALL", global = true)]
    pub suppress: Vec<String>,

    // -- Universal source-selection flags -------------------------------------

    /// KIF file to load into the knowledge base (repeatable).
    #[arg(short = 'f', long = "file", value_name = "FILE", global = true)]
    pub files: Vec<PathBuf>,

    /// Directory whose *.kif files are loaded into the knowledge base (repeatable).
    #[arg(short = 'd', long = "dir", value_name = "DIR", global = true)]
    pub dirs: Vec<PathBuf>,

    /// Git repository URL to load the ontology from.
    /// With `load`: clones and commits to the LMDB database (cached).
    /// With other commands: clones on the fly into a temporary directory.
    /// -f / -d / -c paths are resolved relative to the repository root.
    #[arg(long = "git", value_name = "URL", global = true)]
    pub git: Option<String>,

    /// Path to the LMDB database directory.
    /// Defaults to `./sumo.lmdb` in the current working directory.
    #[arg(long, value_name = "DIR", default_value = "./sumo.lmdb", global = true)]
    pub db: PathBuf,

    /// Session key for --tell assertions and TPTP hypothesis filtering.
    #[arg(long, value_name = "KEY", default_value = None)]
    pub session: Option<String>,

    /// Skip the LMDB database entirely -- do not open or warn about it.
    /// Useful when running without a pre-built database.
    #[arg(long, global = true)]
    pub no_db: bool,

    #[command(subcommand)]
    pub command: Cmd,
}

/// Shared arguments for database and KIF-source selection.
///
/// The universal source flags (`-f`, `-d`, `--db`, `--no-db`) live on the
/// top-level [`Cli`] with `global = true`; this struct's equivalent fields
/// carry `#[arg(skip)]` and are populated from the top-level parse by
/// `main.rs` before the struct is handed to a `run_*` handler.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct KbArgs {
    /// KIF file to load into the knowledge base (repeatable).
    /// Populated from the top-level `Cli::files` by `main.rs`.
    #[arg(skip)]
    pub files: Vec<PathBuf>,

    /// Directory whose *.kif files are loaded (repeatable).
    /// Populated from `Cli::dirs`.
    #[arg(skip)]
    pub dirs: Vec<PathBuf>,

    /// Path to the LMDB database directory.  Populated from `Cli::db`;
    /// the `./sumo.lmdb` default is defined on the top-level flag.
    #[arg(skip)]
    pub db: PathBuf,

    /// Skip the LMDB database entirely.  Populated from `Cli::no_db`.
    #[arg(skip)]
    pub no_db: bool,
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
    },

    /// Run a KIF conjecture through Vampire against the knowledge base.
    #[cfg(feature = "ask")]
    Ask {
        /// KIF conjecture to prove.  May also be supplied via stdin.
        formula: Option<String>,

        /// Assert a KIF formula into the KB before asking (repeatable).
        #[arg(short = 't', long = "tell", value_name = "KIF")]
        tell: Vec<String>,

        #[command(flatten)]
        kb: KbArgs,

        /// Write the generated TPTP to FILE (for debugging).
        /// When omitted, TPTP is piped directly to Vampire via stdin.
        #[arg(short = 'k', long, value_name = "FILE")]
        keep: Option<PathBuf>,
    },

    /// Sweep strategy configs over a problem corpus and pick a
    /// complementary portfolio by greedy marginal coverage.
    ///
    /// Corpus paths may mix `.kif.tq` test files/dirs (run against the
    /// loaded KB) and TPTP `.p` files (each on a fresh KB).  Configs:
    /// the shipping default is always lane 0; add a JSON array of
    /// (partial) Strategy specs via --configs and/or seeded random
    /// samples via --random.  Each (config, problem) cell is one
    /// single-shot run (fixed SInE budget, no autoscale) so the step
    /// count is a deterministic objective.
    #[cfg(feature = "sweep")]
    Sweep {
        /// Corpus: .kif.tq files/dirs and/or TPTP .p files.
        #[arg(value_name = "PATH", num_args = 1..)]
        paths: Vec<PathBuf>,

        /// JSON file holding an array of strategy specs (each spec
        /// names only the knobs it changes; the rest default).
        #[arg(long, value_name = "FILE")]
        configs: Option<PathBuf>,

        /// Generate N random strategies (deterministic from --seed).
        #[arg(long, value_name = "N", default_value_t = 0)]
        random: usize,

        /// Seed for --random (config i uses seed+i).
        #[arg(long, value_name = "SEED", default_value_t = 0xC0FFEE)]
        seed: u64,

        /// Fixed SInE axiom budget per run (single shot, no autoscale).
        #[arg(long, value_name = "AXIOMS", default_value_t = 2000)]
        budget: usize,

        /// Given-clause step cap per run (the primary objective bound).
        #[arg(long, value_name = "N", default_value_t = 200_000)]
        steps: usize,

        /// Wall-clock cap per run in seconds (the safety net).
        #[arg(long, value_name = "SECS", default_value_t = 10)]
        timeout: u32,

        /// Worker threads (default: available parallelism).
        #[arg(long, value_name = "N")]
        jobs: Option<usize>,

        /// Write the full (config x problem) matrix as CSV.
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,

        /// Maximum portfolio lanes the greedy cover may pick.
        #[arg(long, value_name = "K", default_value_t = 6)]
        lanes: usize,

        /// Write the chosen portfolio as a strategy-spec JSON array
        /// (feed it back via --configs, or to the portfolio runner).
        #[arg(long, value_name = "FILE")]
        portfolio_out: Option<PathBuf>,

        #[command(flatten)]
        kb: KbArgs,
    },

    /// CASC-style batch run: prove every TPTP problem under a directory (or
    /// listed one-per-line in a plain file) on the native backend with the
    /// TPTP regime (full saturation + the budget-adaptive strategy portfolio), and
    /// report SZS status per problem plus a final summary.
    ///
    /// Each problem gets its own fresh, self-contained `Session` (no shared
    /// base KB, no `-f`/`-d`/`-c` ontology) — exactly the isolation
    /// `sumo test` already gives standalone `.p`/`.tptp` files, reused here
    /// via the same `Session::test` machinery (so `include(...)` axiom
    /// libraries resolve the same way, against `$TPTP` / the problem's own
    /// directory / its parent).
    ///
    /// Output is deliberately unstyled (as if `--ugly` were passed): every
    /// `% SZS status <STATUS> for <NAME>` line plus the summary block are
    /// meant to be greppable / diffable against a reference CASC run.
    #[cfg(feature = "ask")]
    Casc {
        /// A directory of `.p`/`.tptp` files, OR a plain-text file listing
        /// one problem path per line (blank lines and `#`-prefixed comment
        /// lines are skipped).
        #[arg(value_name = "DIR_OR_LIST")]
        path: PathBuf,

        /// Wall-clock budget per problem, in seconds.
        #[arg(long, value_name = "SECS", default_value_t = 60)]
        timeout: u32,

        /// Worker threads (problems run in parallel; each gets its own
        /// fresh KB, so this is safe up to available parallelism).
        #[arg(long, value_name = "K", default_value_t = 4)]
        jobs: usize,
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

        /// Emit numeric literals as-is instead of encoding them as n__N tokens.
        #[arg(long)]
        show_numbers: bool,

        /// Emit a `% <original KIF>` comment before each TPTP formula.
        #[arg(long)]
        show_kif: bool,

        /// Translate a `.kif.tq` test file into the exact TPTP problem the
        /// prover would receive (conjecture + selected axioms + assertions),
        /// *without* invoking Vampire.  The negated conjecture, SInE
        /// selection, predicate-variable instantiation, and taxonomy-closure
        /// injection are all applied — it is `test … -k` minus the prover.
        /// Output goes to `--keep` if given, otherwise stdout.
        #[arg(long = "test", value_name = "FILE")]
        test: Option<PathBuf>,

        /// (with `--test`) Disable SInE preselection and emit the whole KB
        /// plus the test's assertions and conjecture — mirrors `test --full-kb`.
        #[arg(long = "full-kb")]
        full_kb: bool,

        /// (with `--test`) Write the generated TPTP to FILE instead of stdout.
        #[arg(short = 'k', long, value_name = "FILE")]
        keep: Option<PathBuf>,
    },

    /// Run proof problems with the native prover.  The handling of each
    /// PATH is chosen by extension:
    ///
    ///   * `.kif.tq`           — a KIF test query, run against the loaded
    ///                           base KB (`-f`/`-d`, plus any `.ax` below);
    ///   * `.p` / `.tptp`      — a self-contained TPTP problem, each on a
    ///                           FRESH KB (`include('Axioms/…')` resolved
    ///                           against $TPTP / the problem dir / parent),
    ///                           SZS-styled and checked against the header
    ///                           `% Status :`;
    ///   * `.ax`               — a TPTP axiom library; ingested to POPULATE
    ///                           the KB (no conjecture), so the `.kif.tq`
    ///                           and `.p` problems run against it;
    ///   * a directory         — every `*.kif.tq` inside it.
    ///
    /// When no path is supplied, the test directory is read from
    /// config.xml's `inferenceTestDir` preference (loaded whenever
    /// config.xml is, independent of `-c`).  `.p`/`.ax` require the native
    /// backend.
    #[cfg(feature = "ask")]
    Test {
        /// Path(s) to `.kif.tq` / `.p` / `.tptp` / `.ax` files or
        /// directories.  Multiple arguments and shell-expanded globs are
        /// accepted.  Optional — defaults to the `inferenceTestDir`
        /// preference in config.xml when present.
        #[arg(value_name = "PATH", num_args = 0..)]
        paths: Vec<PathBuf>,

        #[command(flatten)]
        kb: KbArgs,

        /// Write the generated TPTP to FILE (for debugging).
        /// When omitted, TPTP is piped directly to Vampire via stdin.
        #[arg(short = 'k', long, value_name = "FILE")]
        keep: Option<PathBuf>,

        /// Interactively single-step the native prover: pause at each
        /// given-clause and each inference (`make`), printing a readable
        /// view and waiting for input.  Use with ONE problem; the prover
        /// runs single-threaded.  (Same effect as `SIGMA_STEP=1`.)
        #[arg(long)]
        step: bool,

        /// Disable SInE axiom preselection: feed the prover the entire KB
        /// (every axiom) plus the test's assertions and conjecture.  This
        /// mirrors the legacy Java SigmaKEE behaviour (no preselection) and
        /// is intended for prover-vs-prover benchmarking, where the
        /// `subprocess` (standalone Vampire) and `embedded` (Rust) backends
        /// must solve the identical, un-pruned problem.  Much slower per
        /// query — `--scope` is ignored when this is set.
        #[arg(long = "full-kb")]
        full_kb: bool,
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
        /// stdout is not a TTY (e.g. when piping to another program),
        /// when the global `--ugly` flag is set, or when the `NO_PAGER`
        /// environment variable is set.
        #[arg(long = "no-pager", short = 'P')]
        no_pager: bool,
    },

    /// Substring search over SUMO's natural-language fields
    /// (`documentation`, `termFormat`, `format`) — `apropos(1)` to
    /// `man`'s deep-dive.  Use this when you know the English concept
    /// but not which SUMO symbol encodes it.
    ///
    /// Example: `sumo search wedding` surfaces `Wedding` (class),
    /// `weddingdate` (relation), and any other symbol whose
    /// docstring mentions "wedding".
    Search {
        /// Substring to look for (case-insensitive).
        query: String,

        /// Filter results to one symbol kind.  Accepted values:
        /// `class`, `instance`, `relation`, `function`, `predicate`,
        /// `individual`.  When omitted, all kinds are returned.
        #[arg(long, value_name = "KIND")]
        kind: Option<String>,

        /// Filter to axioms tagged with this language (e.g.
        /// `EnglishLanguage`).  When omitted, all languages match.
        #[arg(long, value_name = "LANG")]
        lang: Option<String>,

        /// Cap on the number of results.  Default: 50.  Pass `0` for unlimited.
        #[arg(long, value_name = "N", default_value = "50")]
        limit: usize,
    },

    /// Audit the knowledge base for inconsistency, enumerating the
    /// contradictions found and the axioms implicated in each.
    ///
    /// With no `<FILE>`, the ENTIRE KB is audited.  Optionally pass:
    ///   * a `.kif` file already loaded in the KB — restrict the audit
    ///     to its sentences (and their SInE-relevant neighbourhood),
    ///   * or a `.kif.tq` test case — its assertions are injected into a
    ///     temporary session and used as the sample, so you can answer
    ///     "which axioms made this *test* fail?" by running `audit`
    ///     directly on the failing `.kif.tq`.
    ///
    /// Each contradiction lists the implicated axioms (formula +
    /// file:line).  With `--proof` (and unless `--ugly` is set) each
    /// contradiction's full derivation is shown one-per-page in the
    /// pager; `--proof --ugly` prints the derivations inline.
    ///
    /// The flow is:
    ///   1. Collect the sample sentences:
    ///        - `.kif`:    look up `<FILE>` in the loaded KB and take
    ///          its root sentences.
    ///        - `.kif.tq`: parse via the test-file grammar, inject
    ///          every `(...)` assertion into a debug session, take
    ///          the resulting SIDs.  `--thoroughness` is ignored —
    ///          the entire test bundle is always used.
    ///   2. (`.kif` only) Randomly subsample by `--thoroughness`
    ///      (default 1.0 = all).
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
    /// Uses TPTP FOF (TFF is not currently wired through `audit`).
    #[cfg(feature = "ask")]
    Audit {
        /// OPTIONAL path to scope the audit.  Omit to audit the entire
        /// KB.  A `.kif` file already loaded into the KB (via `-f` or the
        /// LMDB store) restricts the sample to that file; a `.kif.tq`
        /// test file injects its assertions into a debug session.  For
        /// `.kif` files the tag must match a loaded tag (same path form
        /// you used when loading); for `.kif.tq` the file is read fresh.
        file: Option<PathBuf>,

        /// Fraction of the file's root sentences to sample for the
        /// consistency check, in (0.0, 1.0].  `1.0` uses every
        /// sentence; `0.5` a random half; `0.1` a random tenth.
        /// Smaller values run faster at the cost of coverage — the
        /// SInE expansion step then pulls in a proportionally smaller
        /// relevant axiom set.
        #[arg(long, value_name = "F", default_value_t = 1.0)]
        thoroughness: f32,

        /// Stop after finding N distinct contradictions (native backend).
        /// A smaller limit returns faster — the audit terminates the search
        /// as soon as N are found instead of saturating for the rest.
        #[arg(long, value_name = "N", default_value_t = 64)]
        limit: usize,

        /// Write the generated TPTP to FILE (for debugging).  When
        /// omitted, TPTP is piped directly to Vampire via stdin.
        #[arg(short = 'k', long, value_name = "FILE")]
        keep: Option<PathBuf>,

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
    /// Source builds never overwrite themselves.
    Update {
        /// Don't apply the update — just check upstream and report.
        #[arg(long)]
        check: bool,
    },

    /// Print the resolved KBManager configuration (from config.xml when found
    /// and `--no-config` wasn't passed, else built-in defaults) and how each
    /// option maps to its CLI flag.
    Config {},
}
