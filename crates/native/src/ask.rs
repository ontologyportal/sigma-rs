use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use log;
use sumo_parser_core::{
    kb_to_tptp, load_kif, sentence_to_tptp, KnowledgeBase, TptpLang, TptpOptions,
};

use crate::prover::{parse_vampire_output, VampireOutput, TptpProofProcessor, Binding};

// ── AskOptions / AskResult ────────────────────────────────────────────────────

/// Options for `ask()` / `run_ask_with_tptp()`.
#[derive(Debug, Default)]
pub struct AskOptions {
    /// Path to the Vampire executable. Defaults to `"vampire"` (PATH lookup).
    pub vampire_path: Option<PathBuf>,
    /// Timeout passed to Vampire in seconds. Defaults to 30.
    pub timeout_secs: Option<u32>,
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
    pub raw_output: String,
    /// Parsed vampire output
    pub output: VampireOutput,
    /// The TPTP file that was passed to Vampire (if `keep_tmp_file` was set).
    pub tmp_file: Option<PathBuf>,
    /// Any errors that prevented the call (parse, validation, I/O).
    pub errors: Vec<String>,
    /// Variable binding inferences
    pub inference: Vec<Binding>
}

impl AskResult {
    pub fn error(errors: Vec<String>) -> Self {
        return AskResult { 
            proved: false, raw_output: String::new(), 
            output: VampireOutput::default(), tmp_file: None, 
            errors, inference: Vec::new()
        }
    }
}

// ── ask() — legacy in-memory path (used in tests and translate) ───────────────

/// Assert a conjecture and run Vampire to attempt a proof.
///
/// Uses an in-memory `KnowledgeBase` for both KB axioms and the query.
/// This path is kept for backward compatibility (tests, `translate` command).
pub fn ask(kb: &mut KnowledgeBase, query_kif: &str, opts: AskOptions) -> AskResult {
    log::debug!("ask (in-memory): {}", query_kif);

    // Parse the query
    let errs = load_kif(&mut kb.store, query_kif, "__query__");
    if !errs.is_empty() {
        return AskResult::error(errs.iter().map(|e| e.1.to_string()).collect());
    }

    let sid = match kb.store.roots.last().copied() {
        Some(id) => id,
        None => return AskResult::error(vec!["No query sentence parsed".into()])
    };

    if kb.store.sentences.get(sid as usize).unwrap().file != "__query__" {
        return AskResult::error(vec!["No query sentence parsed".into()]);
    }

    log::debug!("ask: parsed conjecture sid={}", sid);

    if let Err(e) = kb.validate_sentence(sid) {
        kb.store.remove_file("__query__");
        return AskResult::error(vec![format!("{}", e)]);
    }

    log::debug!("ask: validated conjecture");

    let lang = opts.lang_or_default();
    let query_opts = TptpOptions { lang, query: true, hide_numbers: true, ..TptpOptions::default() };
    let conjecture_formula = sentence_to_tptp(sid, kb, &query_opts);
    let conjecture = format!("{}(query_0,conjecture,({})). \n", lang.as_str(), conjecture_formula);
    log::debug!("ask: conjecture TPTP = {}", conjecture);
    kb.store.remove_file("__query__");

    let kb_opts = TptpOptions { lang, hide_numbers: true, ..TptpOptions::default() };
    let kb_tptp = kb_to_tptp(kb, "kb", &kb_opts, opts.session.as_deref());

    run_vampire_on_tptp(&format!("{}\n{}", kb_tptp, conjecture), opts)
}

// ── run_ask_with_tptp() — LMDB-backed path ────────────────────────────────────

/// Run Vampire using pre-computed TPTP CNF from the LMDB store.
///
/// The query conjecture is still parsed fresh from `query_kif` via the
/// in-memory KB (for validation), then appended to the pre-built KB TPTP.
pub fn run_ask_with_tptp(
    kb:        &mut KnowledgeBase,
    query_kif: &str,
    kb_tptp:   &str,
    opts:      AskOptions,
) -> AskResult {
    log::debug!("run_ask_with_tptp: parsing conjecture '{}'", query_kif);

    // Parse and validate the query against the in-memory KB
    let errs = load_kif(&mut kb.store, query_kif, "__query__");
    if !errs.is_empty() {
        return AskResult::error(errs.iter().map(|e| e.1.to_string()).collect());
    }

    let sid = match kb.store.roots.last().copied() {
        Some(id) => id,
        None => return AskResult::error(vec!["No query sentence parsed".into()])
    };

    if kb.store.sentences.get(sid as usize).map(|s| s.file.as_str()) != Some("__query__") {
        return AskResult::error(vec!["No query sentence parsed".into()]);
    }

    if let Err(e) = kb.validate_sentence(sid) {
        kb.store.remove_file("__query__");
        return AskResult::error(vec![e.to_string()]);
    }

    // Generate the conjecture as FOF (Vampire handles FOF + CNF mixed input)
    let lang     = TptpLang::Fof;
    let q_opts   = TptpOptions { lang, query: true, hide_numbers: true, ..TptpOptions::default() };
    let conj_str = sentence_to_tptp(sid, kb, &q_opts);
    let conj_decl = format!("fof(query_0,conjecture,({})). \n", conj_str);
    log::debug!("run_ask_with_tptp: conjecture = {}", conj_str);
    kb.store.remove_file("__query__");

    let full_tptp = format!("{}\n{}", kb_tptp, conj_decl);
    run_vampire_on_tptp(&full_tptp, opts)
}

// ── Shared Vampire invocation ─────────────────────────────────────────────────

fn run_vampire_on_tptp(full_tptp: &str, opts: AskOptions) -> AskResult {
    let tmp_path: PathBuf = opts.tmp_file.clone().unwrap_or_else(|| {
        let mut p = std::env::temp_dir();
        p.push(format!("sumo_ask_{}.tptp", std::process::id()));
        p
    });

    if let Err(e) = write_file(&tmp_path, full_tptp) {
        return AskResult::error(vec![format!("Failed to write TPTP tmp file: {}", e)]);
    }
    log::debug!("run_vampire_on_tptp: wrote {} bytes to {}", full_tptp.len(), tmp_path.display());

    let vampire = opts.vampire_path.as_deref().unwrap_or_else(|| Path::new("vampire"));
    let timeout = opts.timeout_secs.unwrap_or(30).to_string();
    let args    = ["--mode", "casc", "--input_syntax", "tptp", "-t", &timeout];

    log::debug!(
        "run_vampire_on_tptp: {} {} {}",
        vampire.display(), args.join(" "), tmp_path.display()
    );

    let output = Command::new(vampire).args(args).arg(&tmp_path).output();

    let kept_path = if opts.keep_tmp_file {
        Some(tmp_path.clone())
    } else {
        let _ = fs::remove_file(&tmp_path);
        log::debug!("run_vampire_on_tptp: removed tmp file");
        None
    };

    match output {
        Err(e) => AskResult {
            proved: false, raw_output: String::new(), tmp_file: kept_path,
            errors: vec![format!("Failed to run vampire: {}", e)],
            output: VampireOutput::default(), inference: Vec::new()
        },
        Ok(out) => {
            let stdout  = String::from_utf8_lossy(&out.stdout).into_owned();
            let stderr  = String::from_utf8_lossy(&out.stderr).into_owned();
            let combined = format!("{}{}", stdout, stderr);
            let proved   = combined.contains("SZS status Theorem");
            log::info!("run_vampire_on_tptp: proved={}", proved);
            log::debug!("parsing vampire output");
            let output = parse_vampire_output(&combined);
            log::debug!("parsing vampire proof");
            let mut tptp_proof_processor = TptpProofProcessor::new();
            tptp_proof_processor.load_proof(&output.proof_steps);
            let inferences = tptp_proof_processor.extract_answers();
            AskResult { proved, raw_output: combined, output, tmp_file: kept_path, errors: Vec::new(), inference: inferences }
        }
    }
}

impl AskOptions {
    fn lang_or_default(&self) -> TptpLang { TptpLang::Fof }
}

fn write_file(path: &Path, content: &str) -> std::io::Result<()> {
    let mut f = fs::File::create(path)?;
    f.write_all(content.as_bytes())
}
