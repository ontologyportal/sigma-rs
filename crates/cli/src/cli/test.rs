use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use regex::Regex;
use sigmakee_rs_sdk::{KnowledgeBase, Parser, ProverStatus, ProvingLayer};
use sigmakee_rs_sdk::manager::{KBManager, ProverOptsFor};
use sigmakee_rs_sdk::{Session, Source, TestCaseOutcome, TestOutcome};

use crate::cli::proof::{is_quiet_proof_format, print_proof};
use crate::style::*;

/// Entry point for `sumo test`.
///
/// The base KB is already loaded into `session` by `dispatch`.  Each discovered
/// test file (`.kif.tq` / `.p` / `.tptp`) runs on a fresh [`Session::fork`] of
/// it, so one test's ingested + promoted axioms can never leak into the next.
/// [`Session::test`] does the rest: split the conjecture from the background
/// theory, promote that background, prove, and grade against the expectation.
///
/// `branch` mirrors `--branch`: which branch a git-shaped PATH resolves
/// against when it doesn't carry its own (a bare repo URL can't, since the
/// syntax is `<repo>#<path-in-repo>` — no room for a third field).
pub fn run_test<L>(
    session: Session<L>,
    manager: KBManager,
    paths:   Vec<String>,
    keep:    Option<PathBuf>,
    branch:  Option<&str>,
) -> bool
where
    L: ProvingLayer,
    L::Opts: ProverOptsFor,
{
    log::debug!("run_test(paths={:?})", paths);
    let _ = keep;

    let test_sources = match discover_test_sources(&paths, branch) {
        Ok(s) => s,
        Err(()) => return false,
    };
    if test_sources.is_empty() {
        log::error!("no test files found");
        return false;
    }

    let opts = <L::Opts as ProverOptsFor>::from_manager(&manager);

    // `casc`/`graphviz` output must be pure SZS/TPTP or DOT text on stdout
    // (no interleaved suite UI) — see `render_case`'s matching gate on the
    // per-case verdict decoration.
    let quiet = is_quiet_proof_format(manager.proof.as_str());

    let total = test_sources.len();
    let mut passed = 0usize;
    let mut informational = 0usize;
    let mut false_verdicts = 0usize;
    let mut all_passed = true;
    let t_all = Instant::now();

    for (label, src) in test_sources {
        if !quiet { println!("Running test: {label}"); }
        let mut case = match session.fork() {
            Ok(c) => c,
            Err(e) => {
                if !quiet {
                    println!("  {color_bright_red}ERROR{color_reset}  (could not fork session: {e})");
                }
                all_passed = false;
                continue;
            }
        };
        let t_case = Instant::now();
        match case.test(src, Some(opts.clone())) {
            Ok(outcome) => {
                match render_case(&outcome, t_case.elapsed(), &manager, case.kb()) {
                    CaseVerdict::Passed        => passed += 1,
                    CaseVerdict::Informational => informational += 1,
                    CaseVerdict::FalseVerdict  => { false_verdicts += 1; all_passed = false; }
                    CaseVerdict::Failed        => all_passed = false,
                }
            }
            Err(errs) => {
                if !quiet { println!("  {color_bright_red}ERROR{color_reset}"); }
                for e in errs { log::error!("  {e}"); }
                all_passed = false;
            }
        }
    }

    if !quiet {
        let graded = total - informational;
        print!("\nTest Summary: {passed} / {graded} passed");
        if informational > 0 {
            print!("  ({informational} informational, not graded)");
        }
        if false_verdicts > 0 {
            print!("  {color_bright_red}{false_verdicts} FALSE VERDICT{}{color_reset}",
                if false_verdicts == 1 { "" } else { "S" });
        }
        println!("  (tests {:.2}s)", t_all.elapsed().as_secs_f64());
    }
    all_passed
}

/// What one rendered case counted as, for the suite-level summary tally.
enum CaseVerdict {
    Passed,
    Failed,
    /// A confident, wrong claim (see [`TestOutcome::FalseVerdict`]) — always
    /// counted as a suite failure, but tallied separately so it stands out
    /// from an ordinary timeout/give-up `Failed`.
    FalseVerdict,
    /// `Open`/`Unknown` header (or no header) — reported, not graded.
    Informational,
}

/// A git remote reference: `git@host:path`, `git://…`/`ssh://…`, or an
/// `https://` URL ending in `.git` (optionally with its own `#fragment`,
/// handled the same as the bare-prefix forms by [`parse_git_arg`]). Checked
/// *before* [`HTTP_RE`] — a `.git`-suffixed `https://` URL would otherwise
/// also match that.
static GIT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:git@[^/:]+:.+|(?:git|ssh)://\S+|https?://\S+\.git(?:#\S*)?)$").unwrap()
});

/// A plain `http(s)://` URL — fetched directly as one test file.
static HTTP_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^https?://\S+$").unwrap());

/// Split a git-shaped PATH into `(repo uri, in-repo path)`. The `#<path>`
/// fragment is required: unlike a local directory, there's no way to list
/// "every test file in this repo" without a full clone, so a bare repo URL
/// with nothing to fetch is a clear error rather than a silent no-op.
fn parse_git_arg(raw: &str) -> Result<(String, PathBuf), String> {
    match raw.split_once('#') {
        Some((uri, path)) if !path.is_empty() => Ok((uri.to_string(), PathBuf::from(path))),
        _ => Err(format!(
            "git test source `{raw}` needs a `#<path-in-repo>` fragment, e.g. `{raw}#Problems/PUZ001+1.p`"
        )),
    }
}

/// Classify and resolve each of `paths`, collecting one `(label, Source)` per
/// discovered test file (`.kif.tq` / `.p` / `.tptp`). Linked `.ax` libraries
/// and `include(...)` directives are resolved downstream (by the loaded base
/// KB and [`Source::read`]), not here.
///
/// Each PATH is either a local file/directory (today's behavior — a
/// directory is walked non-recursively, one test case per recognized file),
/// a git reference (`<repo>#<path>`, sparse-fetching exactly that one file
/// as one test case — `branch` selects which branch when the reference
/// doesn't carry its own), or a plain URL (fetched directly as one test
/// case). See [`GIT_RE`]/[`HTTP_RE`] for the exact shapes recognized.
fn discover_test_sources(paths: &[String], branch: Option<&str>) -> Result<Vec<(String, Source)>, ()> {
    let mut out: Vec<(String, Source)> = Vec::new();
    for raw in paths {
        if GIT_RE.is_match(raw) {
            let (uri, path) = parse_git_arg(raw).map_err(|e| log::error!("{e}"))?;
            push_if_test_labeled(raw.clone(), path, |p| {
                Source::Git { uri, paths: vec![p], branch: branch.map(str::to_string) }
            }, &mut out);
        } else if HTTP_RE.is_match(raw) {
            let uri = raw.parse().map_err(|e| log::error!("invalid URL `{raw}`: {e}"))?;
            push_if_test_labeled(raw.clone(), PathBuf::from(raw), |_| Source::Http { uri }, &mut out);
        } else {
            let path = PathBuf::from(raw);
            if path.is_dir() {
                let entries = std::fs::read_dir(&path).map_err(|e| {
                    log::error!("failed to read directory {}: {e}", path.display());
                })?;
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_file() { push_if_test(p, &mut out); }
                }
            } else if path.is_file() {
                push_if_test(path, &mut out);
            } else {
                log::error!("path not found: {}", path.display());
                return Err(());
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.dedup_by(|a, b| a.0 == b.0);
    Ok(out)
}

fn push_if_test(p: PathBuf, out: &mut Vec<(String, Source)>) {
    let is_test = Parser::from_filename(&p.to_string_lossy())
        .map_or(false, |parser| parser.is_test());
    if is_test {
        out.push((p.display().to_string(), Source::Local(vec![p])));
    }
}

/// Like [`push_if_test`], but for a remote (git/http) reference: the
/// recognized-extension gate is the same (checked against `path`, the
/// in-repo path or URL — whichever carries the file's name/extension), but
/// the `Source` itself is built by the caller (`build`) since it isn't a
/// plain local path.
fn push_if_test_labeled(
    label: String,
    path:  PathBuf,
    build: impl FnOnce(PathBuf) -> Source,
    out:   &mut Vec<(String, Source)>,
) {
    let is_test = Parser::from_filename(&path.to_string_lossy())
        .map_or(false, |parser| parser.is_test());
    if is_test {
        out.push((label, build(path)));
    } else {
        log::error!("`{label}` is not a recognized test file (.kif.tq / .p / .tptp)");
    }
}

/// Print one case's verdict (+ optional `--proof` / `--prose`) and its `%
/// SZS status` line, returning the suite-tally bucket it counts as.
/// Rendered against the fork's KB, so proof citations resolve to the test's
/// own axioms.
fn render_case<L>(
    oc:      &TestCaseOutcome,
    elapsed: Duration,
    manager: &KBManager,
    kb:      &KnowledgeBase<L>,
) -> CaseVerdict
where
    L: ProvingLayer,
{
    // Fold this case's per-mechanism saturation timers (only populated
    // under `--profile`) into the same aggregator the coarser `ask.*`
    // phases report through — see `run_ask`'s matching call.
    if let Some(sink) = crate::progress::global() {
        crate::progress::record_mechanism_profile(&sink, &oc.result.phase_profile);
    }

    let format = manager.proof.as_str();
    // `casc`/`graphviz` output must be pure SZS/TPTP or DOT text on stdout —
    // the pass/fail banner, expectation diagnostics, and prose paraphrase
    // below are all suite-UI, not proof content, so they're suppressed
    // rather than interleaved with it.  The `CaseVerdict` is still computed
    // either way: suppressing the print doesn't change the suite tally.
    let quiet = is_quiet_proof_format(format);
    let note = format!("(total {:.2}s)", elapsed.as_secs_f64());
    let verdict = match &oc.outcome {
        TestOutcome::Passed => {
            if !quiet { println!("  {color_bright_green}PASSED{color_reset}  {note}"); }
            CaseVerdict::Passed
        }
        TestOutcome::Incomplete { inferred, missing } => {
            // The query was proven; only the answer-set enumeration was partial.
            if !quiet {
                println!("  {color_bright_green}PASSED{color_reset}  {note}");
                println!("    the query was proven but only some answers were inferred");
                println!("    inferred: {}", inferred.join(", "));
                println!("    missing:  {}", missing.join(", "));
            }
            CaseVerdict::Passed
        }
        TestOutcome::Failed { expected, got, status } => {
            if !quiet {
                println!("  {color_bright_red}FAILED{color_reset}  {note}");
                println!("    expected: {}, got: {} ({})",
                    if *expected { "yes" } else { "no" },
                    if *got      { "yes" } else { "no" },
                    reason_tag(*status));
            }
            CaseVerdict::Failed
        }
        TestOutcome::FalseVerdict { expected, status } => {
            // Distinct from FAILED: the prover didn't run out of budget, it
            // made a CONFIDENT claim that contradicts the file's own `%
            // Status` header — the harness's most serious finding.
            if !quiet {
                println!("  {color_bright_red}{style_bold}FALSE VERDICT{style_reset}{color_reset}  {note}");
                println!("    expected: {expected:?}, got: {status:?} ({})", reason_tag(*status));
            }
            CaseVerdict::FalseVerdict
        }
        TestOutcome::Informational => {
            if !quiet {
                println!("  {color_bright_cyan}INFO{color_reset}      {note}");
                println!("    no graded expectation (Open/Unknown status, or none) — reporting only");
            }
            CaseVerdict::Informational
        }
    };
    if is_quiet_proof_format(format) {
        // Strict CASC/graphviz output (matches Vampire's own stdout / a bare
        // DOT graph): `print_proof` emits the leading status line itself
        // (`% SZS status ... for ...`, or nothing for graphviz), flush-left,
        // with no other decoration — don't print a second one here.
        print_proof(kb, &oc.result, format, &basename(&oc.name), oc.szs);
    } else {
        println!("  % SZS status {} for {}", oc.szs, basename(&oc.name));
        if format != "none" && !oc.result.proof_kif.is_empty() {
            println!("    {style_bold}Proof:{style_reset}");
            print_proof(kb, &oc.result, format, &basename(&oc.name), oc.szs);
        }
    }
    if manager.prose && !quiet && !oc.result.proof_kif.is_empty() {
        let report = kb.render_proof_prose(None, &oc.result.proof_kif, "EnglishLanguage");
        println!("\n    {style_bold}Proof (prose):{style_reset}\n\n{}", report.rendered);
    }
    verdict
}

/// The bare file-stem SZS convention prints (`PUZ001+1`, not the full path
/// or extension).
fn basename(name: &str) -> String {
    std::path::Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| name.to_string())
}

/// Short, lowercase tag describing why the prover landed on its verdict —
/// rendered next to a failed case's `got:` line to distinguish a timeout from a
/// countermodel from contradictory axioms.
fn reason_tag(status: ProverStatus) -> &'static str {
    match status {
        ProverStatus::Proved       => "refutation",
        ProverStatus::Disproved    => "disproved",
        ProverStatus::Consistent   => "countermodel",
        ProverStatus::Inconsistent => "inconsistent",
        ProverStatus::Timeout      => "timeout",
        ProverStatus::InputError   => "input error",
        ProverStatus::Unknown      => "gave up",
    }
}

#[cfg(test)]
mod source_classification_tests {
    use super::*;

    #[test]
    fn git_re_matches_ssh_shorthand_and_schemes() {
        assert!(GIT_RE.is_match("git@github.com:o/r.git#Problems/P.p"));
        assert!(GIT_RE.is_match("git://example.com/o/r#P.p"));
        assert!(GIT_RE.is_match("ssh://git@example.com/o/r#P.p"));
        assert!(GIT_RE.is_match("https://github.com/o/r.git#Problems/P.p"));
        assert!(GIT_RE.is_match("https://github.com/o/r.git")); // fragment optional to MATCH; required to parse
    }

    #[test]
    fn git_re_does_not_match_plain_urls_or_local_paths() {
        assert!(!GIT_RE.is_match("https://example.com/Merge.kif"));
        assert!(!GIT_RE.is_match("Problems/PUZ001+1.p"));
        assert!(!GIT_RE.is_match("/abs/Problems/PUZ001+1.p"));
    }

    #[test]
    fn http_re_matches_plain_urls_only() {
        assert!(HTTP_RE.is_match("https://example.com/Merge.kif"));
        assert!(HTTP_RE.is_match("http://example.com/x.p"));
        assert!(!HTTP_RE.is_match("Problems/PUZ001+1.p"));
    }

    #[test]
    fn git_checked_before_http_for_dot_git_urls() {
        // A `.git`-suffixed https URL must classify as git, not a plain
        // HTTP fetch — discover_test_sources checks GIT_RE first for
        // exactly this reason.
        let s = "https://github.com/o/r.git#Problems/P.p";
        assert!(GIT_RE.is_match(s));
    }

    #[test]
    fn parse_git_arg_splits_uri_and_path() {
        let (uri, path) = parse_git_arg("https://github.com/o/r.git#Problems/P.p").unwrap();
        assert_eq!(uri, "https://github.com/o/r.git");
        assert_eq!(path, PathBuf::from("Problems/P.p"));
    }

    #[test]
    fn parse_git_arg_requires_a_nonempty_fragment() {
        assert!(parse_git_arg("https://github.com/o/r.git").is_err());
        assert!(parse_git_arg("https://github.com/o/r.git#").is_err());
    }

    #[test]
    fn discover_test_sources_classifies_git_http_and_local() {
        let dir = std::env::temp_dir().join(format!("sumo-test-classify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let local = dir.join("case.p");
        std::fs::write(&local, "fof(a,axiom,p(a)).").unwrap();

        let paths = vec![
            "https://github.com/o/r.git#Problems/P.p".to_string(),
            "https://example.com/Merge.p".to_string(),
            local.to_string_lossy().to_string(),
        ];
        let sources = discover_test_sources(&paths, Some("dev")).unwrap();
        assert_eq!(sources.len(), 3);

        assert!(matches!(
            &sources.iter().find(|(l, _)| l.starts_with("https://github.com")).unwrap().1,
            Source::Git { uri, paths, branch }
                if uri == "https://github.com/o/r.git"
                && paths == &[PathBuf::from("Problems/P.p")]
                && branch.as_deref() == Some("dev")
        ));
        assert!(matches!(
            &sources.iter().find(|(l, _)| l.starts_with("https://example.com")).unwrap().1,
            Source::Http { .. }
        ));
        assert!(matches!(
            &sources.iter().find(|(l, _)| l.contains("case.p")).unwrap().1,
            Source::Local(_)
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
