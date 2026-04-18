use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "sumo",
    about = "Parse, validate, translate, and query SUMO KIF knowledge bases",
    after_help = "Reference:\n  Niles, I., and Pease, A.  2001.  Towards a Standard Upper Ontology.  In\n  Proceedings of the 2nd International Conference on Formal Ontology in\n  Information Systems (FOIS-2001), Chris Welty and Barry Smith, eds,\n  Ogunquit, Maine, October 17-19, 2001.  Also see http://www.ontologyportal.org",
    version
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
    /// Use '-W <CODE>' (e.g., -W E005) to treat a specific one as an error.
    #[arg(short = 'W', long = "warning", value_name = "CODE_OR_ALL", global = true)]
    pub suppress: Vec<String>,

    #[command(subcommand)]
    pub command: Cmd,
}

/// Shared arguments for database and KIF-source selection.
///
/// KIF files are the initialisation source (like SQL migration scripts).
/// Once loaded, the LMDB database at `--db` is the canonical store.
#[derive(clap::Args, Clone, Debug)]
pub struct KbArgs {
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

    // #[cfg(feature = "cnf")]
    // /// Hard upper bound on CNF clauses per formula.
    // /// Overrides the SUMO_MAX_CLAUSES environment variable.
    // #[arg(long, value_name = "N", default_value_t = 10_000)]
    // pub max_clauses: usize,

    /// Path to the Vampire executable (default: 'vampire' on PATH).
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

        #[command(flatten)]
        kb: KbArgs,
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

        /// Print the proof steps translated to SUO-KIF after a successful proof.
        #[arg(long)]
        proof: bool,

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

        #[command(flatten)]
        kb: KbArgs,
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
    Load {
        #[command(flatten)]
        kb: KbArgs,
    },
}
