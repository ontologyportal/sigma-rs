use std::path::PathBuf;
use log;
use inline_colorization::*;
use crate::cli::args::KbArgs;
use crate::cli::util::open_or_build_kb;
use crate::ask::{ask as native_ask, AskOptions, Binding};
use crate::cli::util::parse_lang;
use sumo_kb::parse_test_content;

pub fn run_test(paths: Vec<PathBuf>, kb_args: KbArgs, keep: Option<PathBuf>, backend: String, lang: String, timeout_override: Option<u32>) -> bool {
    log::trace!("run_test(paths={:?}, kb_args={:#?})", paths, kb_args);
    log::debug!("Test subcommand selected");

    let mut test_files = Vec::new();
    for path in &paths {
        if path.is_dir() {
            match std::fs::read_dir(path) {
                Ok(entries) => {
                    for entry in entries.flatten() {
                        let p = entry.path();
                        if p.is_file() && p.to_string_lossy().ends_with(".kif.tq") {
                            log::debug!("Found test file: {}", p.display());
                            test_files.push(p);
                        }
                    }
                }
                Err(e) => {
                    log::error!("failed to read directory {}: {}", path.display(), e);
                    return false;
                }
            }
        } else if path.is_file() {
            log::debug!("Found test file: {}", path.display());
            test_files.push(path.clone());
        } else {
            log::error!("path not found: {}", path.display());
            return false;
        }
    }
    test_files.sort();
    test_files.dedup();

    if test_files.is_empty() {
        log::error!("no .kif.tq files found");
        return false;
    }

    // 1. Build the base KB once
    log::debug!("Building base KB");
    let mut all_passed = true;
    let total_tests = test_files.len();
    let mut passed_count = 0;

    let mut kb = match open_or_build_kb(&kb_args) {
        Ok(k)   => k,
        Err(()) => return false,
    };

    for (idx, test_file) in test_files.iter().enumerate() {
        let content = match std::fs::read_to_string(test_file) {
            Ok(c) => c,
            Err(e) => {
                log::error!("failed to read test file {}: {}", test_file.display(), e);
                all_passed = false;
                continue;
            }
        };

        let mut test_case = match parse_test_content(&content, &test_file.to_string_lossy()) {
            Ok(tc) => tc,
            Err(e) => {
                // We don't have the span here easily from KbError without more work, 
                // but we can at least log it.
                log::error!("failed to parse test file {}: {}", test_file.display(), e);
                all_passed = false;
                continue;
            }
        };
        log::debug!("Running test from file: {}", test_case.file_name);
        println!("Running test: {} ({})", test_case.note, test_file.display());

        // Each test gets its own session so axioms don't leak between tests.
        let session = format!("test-{}", idx);

        if !test_case.extra_files.is_empty() {
            log::debug!(
                "test {} references extra files (should be in base KB): {}",
                test_case.note,
                test_case.extra_files.join(", ")
            );
        }

        let axiom_text = test_case.axioms.join("\n");
        let load_tag = format!("test-src-{}", idx);
        let load_result = kb.load_kif(&axiom_text, &load_tag, Some(&session));
        if !load_result.ok {
            for e in &load_result.errors {
                log::error!("parse error in test axioms: {}", e);
            }
            kb.flush_session(&session);
            all_passed = false;
            continue;
        }

        let semantic_errors = kb.validate_session(&session);
        if !semantic_errors.is_empty() {
            for (_, e) in &semantic_errors {
                log::error!("semantic error in test axioms: {}", e);
            }
            kb.flush_session(&session);
            all_passed = false;
            continue;
        }

        if let Some(t) = timeout_override {
            test_case.timeout = t;
        }

        let query = match test_case.query {
            Some(q) => q,
            None => {
                log::error!("no query found in test file");
                kb.flush_session(&session);
                all_passed = false;
                continue;
            }
        };

        log::debug!("Found query for testing: {}", query);

        let result = native_ask(
            &mut kb,
            &query,
            AskOptions {
                vampire_path: kb_args.vampire.clone(),
                timeout_secs: Some(test_case.timeout),
                tptp_dump_path: keep.clone(),
                session: Some(session.clone()),
                backend: backend.clone(),
                lang: parse_lang(&lang),
            },
        );

        kb.flush_session(&session);

        if !result.errors.is_empty() {
            log::error!("prover error(s) for test {}:", test_case.note);
            for e in &result.errors {
                log::error!("  {}", e);
            }
            all_passed = false;
            continue;
        }

        let expected = test_case.expected_proof.unwrap_or(true);
        if result.proved == expected {
            if let Some(expected_answers) = test_case.expected_answer {
                let found_answers: &Vec<Binding> = result.inference.as_ref();
                let paired_answers: Vec<(&String, bool)> = expected_answers.iter().map(| e | {
                    return (e, found_answers.iter().any(|f| *e == f.value))
                }).collect();

                if !paired_answers.iter().all(|p| p.1) {
                    println!("  {color_bright_yellow}INCOMPLETE{color_reset}");
                    println!("    the query was proven but only some answers could be inferred");
                    println!("    inferred answers: {}", paired_answers.iter().filter_map(| p | if p.1 {Some(p.0.clone())} else {None}).collect::<Vec<String>>().join(", "));
                    println!("    missing answers: {}", paired_answers.iter().filter_map(| p | if !p.1 {Some(p.0.clone())} else {None}).collect::<Vec<String>>().join(", "));
                    all_passed = false;
                    continue
                }
            }
            println!("  {color_bright_green}PASSED{color_reset}");
            passed_count += 1;
        } else {
            println!("  {color_bright_red}FAILED{color_reset}");
            println!("    expected: {}, got: {}", 
                if expected { "yes" } else { "no" },
                if result.proved { "yes" } else { "no" }
            );
            all_passed = false;
        }
    }

    println!("
Test Summary: {} / {} passed", passed_count, total_tests);
    all_passed
}
