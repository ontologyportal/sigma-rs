use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use log;
use sumo_parser_core::{
    kb_to_tptp, load_kif, sentence_to_tptp, KnowledgeBase, TptpLang, TptpOptions,
};

// ── AskOptions / AskResult ────────────────────────────────────────────────────

/// Options for the `ask()` call.
#[derive(Debug, Default)]
pub struct AskOptions {
    /// Path to the Vampire executable. Defaults to `"vampire"` (PATH lookup).
    pub vampire_path: Option<PathBuf>,
    /// Timeout passed to Vampire in seconds. Defaults to 30.
    pub timeout_secs: Option<u32>,
    /// TPTP language variant (default FOF).
    pub lang: Option<TptpLang>,
    /// If true, keep the temporary TPTP file after the call.
    pub keep_tmp_file: bool,
    /// Override the tmp file path. If None a random file in /tmp is used.
    pub tmp_file: Option<PathBuf>,
    /// Session whose assertions are included as TPTP hypotheses.
    /// `None` includes assertions from all sessions.
    pub session: Option<String>,
}

/// Result from `ask()`.
#[derive(Debug)]
pub struct AskResult {
    /// True iff Vampire reported `SZS status Theorem`.
    pub proved: bool,
    /// Raw stdout + stderr from Vampire.
    pub output: String,
    /// The TPTP file that was passed to Vampire (if `keep_tmp_file` was set).
    pub tmp_file: Option<PathBuf>,
    /// Any errors that prevented the call (parse, validation, I/O).
    pub errors: Vec<String>,
}

// ── ask() ─────────────────────────────────────────────────────────────────────

/// Assert a conjecture and run Vampire to attempt a proof.
///
/// * `kb`        — the knowledge base (query is parsed in and cleaned up)
/// * `query_kif` — a single KIF formula that becomes the TPTP conjecture
/// * `opts`      — call options
pub fn ask(kb: &mut KnowledgeBase, query_kif: &str, opts: AskOptions) -> AskResult {
    log::debug!("Asking KB: {}", query_kif);
    // 1. Parse the query
    let errs = load_kif(&mut kb.store, query_kif, "__query__");
    if !errs.is_empty() {
        return AskResult {
            proved: false,
            output: String::new(),
            tmp_file: None,
            errors: errs.iter().map(|e| e.1.to_string()).collect(),
        };
    }

    let sid = match kb.store.roots.last().copied() {
        Some(id) => id,
        None => {
            return AskResult {
                proved: false,
                output: String::new(),
                tmp_file: None,
                errors: vec!["No query sentence parsed".into()],
            }
        }
    };

    if kb.store.sentences.get(sid).unwrap().file != "__query__" {
        return AskResult {
            proved: false,
            output: String::new(),
            tmp_file: None,
            errors: vec!["No query sentence parsed".into()],
        };
    };

    log::debug!("Successfully parsed the conjecture");

    match kb.validate_sentence(sid) {
        Err(e) => {
            return AskResult {
                proved: false,
                output: String::new(),
                tmp_file: None,
                errors: vec![format!("{}", e)],
            }
        }
        _ => {}
    }

    log::debug!("Successfully validated the conjecture");

    // 2. Convert query to TPTP conjecture (existential wrapper for free vars)
    let lang = opts.lang.unwrap_or(TptpLang::Fof);
    let query_opts = TptpOptions {
        lang,
        query: true,
        hide_numbers: true,
        ..TptpOptions::default()
    };
    let conjecture_formula = sentence_to_tptp(sid, &kb, &query_opts);
    let conjecture = format!(
        "{}(query_0,conjecture,({})).
",
        lang.as_str(),
        conjecture_formula
    );
    log::debug!("Converted conjecture to TPTP: {}", conjecture);
    kb.store.remove_file("__query__");

    // 3. Build full TPTP (KB axioms + hypotheses + conjecture)
    let kb_opts = TptpOptions {
        lang,
        hide_numbers: true,
        ..TptpOptions::default()
    };
    let kb_tptp = kb_to_tptp(kb, "kb", &kb_opts, opts.session.as_deref());
    let full_tptp = format!("{}
{}", kb_tptp, conjecture);

    // 4. Write to tmp file
    let tmp_path: PathBuf = opts.tmp_file.clone().unwrap_or_else(|| {
        let mut p = std::env::temp_dir();
        p.push(format!("sumo_ask_{}.tptp", std::process::id()));
        p
    });

    if let Err(e) = write_file(&tmp_path, &full_tptp) {
        return AskResult {
            proved: false,
            output: String::new(),
            tmp_file: None,
            errors: vec![format!("Failed to write tmp file: {}", e)],
        };
    }

    log::debug!(
        "Wrote TPTP to file: {}",
        format!("sumo_ask_{}.tptp", std::process::id())
    );

    // 5. Run Vampire
    let vampire = opts
        .vampire_path
        .as_deref()
        .unwrap_or_else(|| Path::new("vampire"));
    let timeout = opts.timeout_secs.unwrap_or(30).to_string();

    let args = ["--mode", "casc", "--input_syntax", "tptp", "-t", &timeout];
    log::debug!(
        "Executing theorem prover: {} {} {}",
        vampire.to_str().unwrap(),
        args.join(" "),
        tmp_path.to_str().unwrap()
    );
    let output = Command::new(vampire).args(args).arg(&tmp_path).output();

    // 6. Clean up tmp file (unless requested to keep)
    let kept_path = if opts.keep_tmp_file {
        Some(tmp_path.clone())
    } else {
        let _ = fs::remove_file(&tmp_path);
        None
    };

    match output {
        Err(e) => AskResult {
            proved: false,
            output: String::new(),
            tmp_file: kept_path,
            errors: vec![format!("Failed to run vampire: {}", e)],
        },
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            let combined = format!("{}{}", stdout, stderr);
            let proved = combined.contains("SZS status Theorem");
            AskResult {
                proved,
                output: combined,
                tmp_file: kept_path,
                errors: Vec::new(),
            }
        }
    }
}

fn write_file(path: &Path, content: &str) -> std::io::Result<()> {
    let mut f = fs::File::create(path)?;
    f.write_all(content.as_bytes())
}
