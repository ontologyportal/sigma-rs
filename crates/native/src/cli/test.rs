use std::path::{Path, PathBuf};
use log;
use inline_colorization::*;
use crate::cli::args::KbArgs;
use crate::cli::util::open_or_build_kb;
use crate::ask::{ask as native_ask, AskOptions, Binding};
use crate::cli::util::parse_lang;
use crate::{parse_error};
use sumo_kb::{tokenize, parse, AstNode, Pretty};

struct TestCase {
    file: PathBuf,
    note: String,
    timeout: u32,
    query: Option<String>,
    expected_proof: Option<bool>, // true = yes, false = no
    expected_answer: Option<Vec<String>>, // List of expect symbol resolutions
    axioms: Vec<String>,
    /// KIF files referenced by `(file F)` directives (informational; the
    /// caller is responsible for ensuring these are loaded in the base KB).
    extra_files: Vec<String>,
}

pub fn run_test(path: PathBuf, kb_args: KbArgs, keep: bool, backend: String, lang: String) -> bool {
    log::trace!("run_test(path={:?}, kb_args={:#?})", path, kb_args);
    log::debug!("Test subcommand selected");

    let mut test_files = Vec::new();
    if path.is_dir() {
        match std::fs::read_dir(&path) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_file() && p.to_string_lossy().ends_with(".kif.tq") {
                        log::debug!("Found test file: {}", p.to_str().unwrap());
                        test_files.push(p);
                    }
                }
            }
            Err(e) => {
                log::error!("failed to read directory {}: {}", path.display(), e);
                return false;
            }
        }
    } else {
        log::debug!("Found test file: {}", path.to_str().unwrap());
        test_files.push(path);
    }
    test_files.sort();

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
        let test_case = match parse_test_file(&test_file) {
            Ok(tc) => tc,
            Err(e) => {
                log::error!("failed to parse test file {}: {}", test_file.display(), e);
                all_passed = false;
                continue;
            }
        };
        log::debug!("Running test from file: {}", test_case.file.to_str().unwrap());
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

        // Bulk-load all test axioms without per-sentence validation, then
        // validate together at the end (mirrors how whole KIF files are
        // processed, avoiding false positives from forward references).
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
                keep_tmp_file: keep,
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
        log::debug!("Vampire output: {}", result.raw_output);
        log::debug!("Vampire result: {}", result.proved);
        log::debug!("Vampire inferences: {}", result.inference.iter().map(| i | format!("{}", i)).collect::<Vec<String>>().join(", "));
        if result.proved == expected {
            if !test_case.expected_answer.is_none() {
                let expected_answers = test_case.expected_answer.unwrap();
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

fn parse_test_file(path: &Path) -> Result<TestCase, String> {
    // Test files differ slightly from run of the mill kif files, so they can't be 
    //  simply inserted into the KB
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read file: {}", e))?;
    
    // First tokenize the test file
    let (tokens, tok_errs) = tokenize(&content, &path.to_string_lossy());
    if !tok_errs.is_empty() {
        // Collection and report errors
        for (span, e) in tok_errs {
            parse_error!(span, e, content);
            return Err("failed to tokenize test file".to_string());
        }
    }

    let (nodes, parse_errs) = parse(tokens, &path.to_string_lossy());
    if !parse_errs.is_empty() {
        for (span, e) in parse_errs {
            parse_error!(span, e, content);
            return Err("failed to parse test file".to_string());
        }
    }

    let mut tc = TestCase {
        file: path.to_path_buf(),
        note: path.file_stem().unwrap_or_default().to_string_lossy().to_string(),
        timeout: 30,
        query: None,
        expected_answer: None,
        expected_proof: None,
        axioms: Vec::new(),
        extra_files: Vec::new(),
    };
    // Look for the special relations
    for node in nodes {
        log::debug!("testing: {}", Pretty(&node));
        let node_str = node.to_string();
        if let AstNode::List { elements, .. } = node {
            if elements.is_empty() { continue; } // Skip empty sentences (should have been caught by tokenizer)
            let head_name = match &elements[0] {
                AstNode::Symbol { name, .. } => name.as_str(), // Only grab the symbols, ignore operators etc
                _ => {
                    tc.axioms.push(node_str.clone());
                    continue;
                }
            };
            // Match special test relations
            match head_name {
                "note" => { // Name of the test
                    if elements.len() > 1 {
                        tc.note = match &elements[1] {
                            AstNode::Str { value, .. } => {
                                // Strip quotes
                                if value.starts_with('"') && value.ends_with('"') {
                                    value[1..value.len()-1].to_string()
                                } else {
                                    value.clone()
                                }
                            }
                            AstNode::Symbol { name, .. } => name.clone(),
                            _ => format!("{}", elements[1]),
                        };
                    }
                }
                "time" => { // Timeout to give the test
                    if elements.len() > 1 {
                        if let AstNode::Number { value, .. } = &elements[1] {
                            tc.timeout = value.parse::<u32>().unwrap_or(30);
                        } else {
                            return Err("the time directive must contain a number".to_string())
                        }
                    }
                }
                "answer" => { // The answer to expect from the tests
                    if elements.len() > 1 {
                        if let AstNode::Symbol { name, .. } = &elements[1] {
                            if name.to_lowercase() == "yes" {
                                tc.expected_proof = Some(true);
                                continue;
                            } else if name.to_lowercase() == "no" {
                                tc.expected_proof = Some(false);
                                continue;
                            } 
                            
                            let mut expected_answers: Vec<String> = Vec::new();
                            tc.expected_proof = Some(true);
                            expected_answers.push(name.to_string());
                            for el in 2..elements.len() {
                                if let AstNode::Symbol { name, .. } = &elements[el] {
                                    expected_answers.push(name.to_string());
                                } else {
                                    return Err("answer predicates can only contain symbols".to_string());
                                }
                            }
                            tc.expected_answer = Some(expected_answers);
                        } else {
                            return Err("answer predicates can only contain symbols".to_string());
                        }
                    } else {
                        return Err("answer predicate either yes/no or includes symbol(s) to infer".to_string());
                    }
                }
                "query" => { // The conjecture to present to the prover
                    if elements.len() > 1 {
                        tc.query = Some(elements[1].to_string());
                    }
                }
                "file" => { // (file F) — KB file dependency; must be loaded via -f
                    if elements.len() > 1 {
                        let fname = match &elements[1] {
                            AstNode::Symbol { name, .. } => name.clone(),
                            AstNode::Str { value, .. } => value.trim_matches('"').to_string(),
                            _ => elements[1].to_string(),
                        };
                        tc.extra_files.push(fname);
                    }
                }
                _ => { // everything else is an assertion
                    tc.axioms.push(format!("{}", node_str));
                }
            }
        } else {
            // Ignore non-sentences
        }
    }

    Ok(tc)
}
