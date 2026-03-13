use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "sumo",
    about = "Parse, validate, translate, and query SUMO KIF knowledge bases",
    version
)]
pub struct Cli {
    /// Logging verbosity (-v = info, -vv = debug, -vvv = trace).
    #[arg(short = 'v', long = "verbose", action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Suppress all warnings (overrids verbosity setting)
    #[arg(short = 'q', long = "quiet", global = true)]
    pub quiet: bool,

    /// Path to SigmaKEE config.xml or the directory containing it.
    #[arg(long, value_name = "PATH", global = true)]
    pub config: Option<PathBuf>,

    /// Knowledge base name from config.xml to load.
    #[arg(long, value_name = "NAME", global = true)]
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

#[derive(clap::Args, Clone, Debug)]
pub struct KbArgs {
    /// KIF file to load into the knowledge base (repeatable).
    #[arg(short = 'f', long = "file", value_name = "FILE")]
    pub files: Vec<PathBuf>,

    /// Directory whose *.kif files are loaded into the knowledge base (repeatable).
    #[arg(short = 'd', long = "dir", value_name = "DIR")]
    pub dirs: Vec<PathBuf>,

    /// Save the parsed knowledge base to a JSON cache file.
    #[arg(short = 'c', long = "cache", value_name = "FILE")]
    pub cache: Option<PathBuf>,

    /// Restore the knowledge base from a previously saved JSON cache
    /// (skips loading -f / -d files).
    #[arg(short = 'r', long = "restore", value_name = "FILE")]
    pub restore: Option<PathBuf>,

    /// Path to the Vampire executable (default: 'vampire' on PATH).
    #[arg(long, value_name = "PATH")]
    pub vampire: Option<PathBuf>,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Validate KIF formula(s) against the knowledge base.
    Validate {
        /// Formula to validate.  May also be supplied via stdin.
        formula: Option<String>,

        #[command(flatten)]
        kb: KbArgs,
    },

    /// Run a KIF conjecture through Vampire against the knowledge base.
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

        #[command(flatten)]
        kb: KbArgs,
    },

    /// Translate KIF formula(s) or a full KB to TPTP.
    Translate {
        /// Formula to translate.  May also be supplied via stdin.
        formula: Option<String>,

        /// TPTP language variant to emit.
        #[arg(long, value_name = "LANG", default_value = "fof")]
        lang: String,

        /// Emit numeric literals as-is instead of encoding them as n__N tokens.
        #[arg(long)]
        show_numbers: bool,

        /// Session key controlling which assertions appear as TPTP hypotheses.
        #[arg(long, value_name = "KEY")]
        session: Option<String>,

        #[command(flatten)]
        kb: KbArgs,
    },
}
