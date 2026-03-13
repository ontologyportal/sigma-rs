use std::path::{Path, PathBuf};
use log;
use inline_colorization::*;
use crate::cli::args::KbArgs;
use crate::cli::util::{build_store, maybe_save_cache};
use crate::ask::{ask as native_ask, AskOptions};
use crate::{parse_error};
use sumo_parser_core::{KnowledgeBase};
use sumo_parser_core::tokenizer::{tokenize};
use sumo_parser_core::parser::{parse, AstNode};

struct TestCase {
    file: PathBuf,
    note: String,
    timeout: u32,
    query: Option<String>,
    expected_answer: Option<bool>, // true = yes, false = no
    axioms: Vec<String>,
}

pub fn run_test(path: PathBuf, kb_args: KbArgs, keep: bool) -> bool {
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
    let base_store = match build_store(&kb_args) {
        Ok(s) => s,
        Err(..) => return false,
    };
    maybe_save_cache(&base_store, kb_args.cache.as_deref());

    let mut all_passed = true;
    let total_tests = test_files.len();
    let mut passed_count = 0;
    
    // Create a fresh KB 
    let mut kb = KnowledgeBase::new(base_store);

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

        let mut tell_ok = true;
        for axiom in &test_case.axioms {
            // Load all the axioms
            let r = kb.tell(format!("test-{}", idx).as_str(), axiom);
            log::debug!("Loaded axiom: {}", axiom);
            if !r.ok {
                log::error!("failed to add axiom to KB: {}", axiom);
                for err in r.errors {
                    log::error!("  error: {}", err);
                }
                tell_ok = false;
                break;
            }
        }

        if !tell_ok {
            all_passed = false;
            continue;
        }

        let query = match test_case.query {
            Some(q) => q,
            None => {
                log::error!("no query found in test file");
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
                ..AskOptions::default()
            },
        );

        if !result.errors.is_empty() {
            log::error!("prover error(s) for test {}:", test_case.note);
            for e in &result.errors {
                log::error!("  {}", e);
            }
            all_passed = false;
            continue;
        }

        let expected = test_case.expected_answer.unwrap_or(true);
        if result.proved == expected {
            println!("  {color_bright_green}PASSED{color_reset}");
            passed_count += 1;
        } else {
            println!("  {color_bright_red}FAILED{color_reset}");
            println!("    expected: {}, got: {}", 
                if expected { "yes" } else { "no" },
                if result.proved { "yes" } else { "no" }
            );
            log::debug!("Vampire output: {}", result.output);
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
        axioms: Vec::new(),
    };
    // Look for the special relations
    for node in nodes {
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
                                tc.expected_answer = Some(true);
                            } else if name.to_lowercase() == "no" {
                                tc.expected_answer = Some(false);
                            } else {
                                return Err("the answer must be either yes or no".to_string())
                            }
                        }
                    }
                }
                "query" => { // The conjecture to present to the prover
                    if elements.len() > 1 {
                        tc.query = Some(format!("{}", elements[1]));
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
