//! Java comparison tests for TPTP output.
//!
//! Mirrors `test/tptp-java-comparison.test.js`.  Each test invokes the Java
//! `SUMOformulaToTPTPformula` CLI and compares its output (after normalisation)
//! against the Rust TPTP generator.
//!
//! Prerequisites:
//!   - `SIGMA_CP`   — Java classpath for the sigmakee JAR(s)  (required)
//!   - `SIGMA_HOME` — sigmakee installation root               (optional, defaults to ~/projects/sigmakee)
//!   - `java` must be on PATH
//!
//! Tests self-skip when prerequisites are absent — they print a SKIP line and
//! return without failing, exactly as the JS suite does.
//!
//! Run only these tests:
//!   SIGMA_CP=<cp> cargo test -p sumo-parser-core --test java_comparison

use std::env;
use std::path::Path;
use std::process::Command;

use sumo_parser_core::{KifStore, KnowledgeBase, TptpOptions, kb_to_tptp, load_kif, sentence_to_tptp};

// ── Prerequisites ─────────────────────────────────────────────────────────────

fn sigma_cp() -> Option<String> {
    let cp = env::var("SIGMA_CP").ok()?;
    if cp.is_empty() { None } else { Some(cp) }
}

fn java_available() -> bool {
    Command::new("java").arg("-version").output().is_ok()
}

/// Returns `Some(reason)` when tests should be skipped, `None` when ready.
fn skip_reason() -> Option<String> {
    if !java_available() {
        return Some("java not available".into());
    }
    if sigma_cp().is_none() {
        return Some("SIGMA_CP not set".into());
    }
    None
}

// ── Java invocation ───────────────────────────────────────────────────────────

fn invoke_java_converter(formula: &str) -> Result<String, String> {
    let cp = sigma_cp().ok_or_else(|| "SIGMA_CP not set".to_owned())?;
    let sigma_home = env::var("SIGMA_HOME")
        .unwrap_or_else(|_| "/home/iggy/projects/sigmakee".into());

    let out = Command::new("java")
        .args([
            "-Xmx8g",
            "-classpath", &cp,
            "com.articulate.sigma.trans.SUMOformulaToTPTPformula",
            "-g", formula,
        ])
        .env("SIGMA_HOME", &sigma_home)
        .output()
        .map_err(|e| format!("failed to run java: {}", e))?;

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();

    // Pick the first line that looks like a TPTP formula (starts with `(` and
    // contains `s__` — the symbol prefix our generator also uses).
    for line in stdout.lines() {
        let t = line.trim();
        if t.starts_with('(') && t.contains("s__") {
            return Ok(t.to_owned());
        }
    }
    // Fallback: last non-empty line
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .last()
        .map(|l| l.trim().to_owned())
        .ok_or_else(|| format!("no usable output from java for: {}", formula))
}

// ── Rust TPTP helpers ─────────────────────────────────────────────────────────

fn default_opts() -> TptpOptions {
    TptpOptions { hide_numbers: true, ..TptpOptions::default() }
}

/// Parse a single KIF formula and convert to a TPTP formula string.
fn to_tptp(kif_str: &str) -> String {
    let mut store = KifStore::default();
    load_kif(&mut store, kif_str, "test");
    let kb = KnowledgeBase::new(store);
    let sid = kb.store.roots[0];
    sentence_to_tptp(sid, &kb, &default_opts())
}

/// Extract axiom bodies `FORMULA` from `fof(name,role,(FORMULA)).` lines.
///
/// The format is `LANG(kb_NAME_N,role,(FORMULA)). `.  Two extra closing parens
/// appear at the end: one closes the `(FORMULA)` wrapper, one closes `fof(...)`.
/// `rfind(").")` lands on the fof-closing paren; `fof_close - 1` is the
/// formula-wrapper paren.  The body is the slice `[body_start .. fof_close - 1]`.
fn extract_axiom_bodies(tptp: &str) -> Vec<String> {
    tptp.lines()
        .filter_map(|line| {
            if !line.contains(",axiom,") { return None; }
            // `,(' that wraps the formula body follows the role token.
            let comma_paren = line.find(",(")?;
            let body_start  = comma_paren + 2;          // skip past `,(`
            // rfind(").")  →  fof-closing `)`
            // fof_close-1  →  formula-wrapper `)`; body ends just before it.
            let fof_close = line.rfind(").")?;
            if fof_close < body_start + 1 { return None; }
            Some(line[body_start..fof_close - 1].trim().to_owned())
        })
        .collect()
}

// ── TPTP normaliser (mirrors JS normalizeTPTP) ────────────────────────────────

fn normalize_tptp(s: &str) -> String {
    if s.is_empty() { return String::new(); }

    // 1. Collapse all whitespace runs to a single space.
    let mut n: String = s.split_whitespace().collect::<Vec<_>>().join(" ");

    // 2. Sort variable lists inside [...] so `[V__Y,V__X]` == `[V__X,V__Y]`.
    n = sort_var_lists(&n);

    // 3. Remove spaces immediately after '(' and before ')'.
    //    Java emits `( expr )` while Rust emits `(expr)`.
    loop {
        let before = n.clone();
        n = n.replace("( ", "(").replace(" )", ")");
        if n == before { break; }
    }

    // 4. Strip redundant outermost single-pair wrapping, matching JS behaviour.
    loop {
        let before = n.clone();
        if n.starts_with('(') && n.ends_with(')') && is_fully_wrapped(&n) {
            n = n[1..n.len() - 1].trim().to_owned();
        }
        if n == before { break; }
    }

    n
}

/// Return true iff the entire string is wrapped in one matching `( … )`.
fn is_fully_wrapped(s: &str) -> bool {
    if !s.starts_with('(') || !s.ends_with(')') { return false; }
    let mut depth = 0i32;
    for (i, ch) in s.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return i == s.len() - 1;
                }
            }
            _ => {}
        }
    }
    false
}

/// Sort comma-separated items inside every `[…]` in `s`.
fn sort_var_lists(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        result.push_str(&rest[..=open]); // include the '['
        rest = &rest[open + 1..];
        if let Some(close) = rest.find(']') {
            let mut vars: Vec<&str> = rest[..close].split(',').map(str::trim).collect();
            vars.sort();
            result.push_str(&vars.join(","));
            result.push(']');
            rest = &rest[close + 1..];
        } else {
            result.push_str(rest); // no matching ']' — append remainder
            return result;
        }
    }
    result.push_str(rest);
    result
}

// ── Comparison helper ─────────────────────────────────────────────────────────

fn compare(kif: &str) {
    if let Some(reason) = skip_reason() {
        eprintln!("SKIP ({}): {}", kif, reason);
        return;
    }
    let rust_out = to_tptp(kif);
    let java_out = match invoke_java_converter(kif) {
        Ok(s)  => s,
        Err(e) => { eprintln!("SKIP ({}): java error: {}", kif, e); return; }
    };
    let rust_norm = normalize_tptp(&rust_out);
    let java_norm = normalize_tptp(&java_out);
    println!("KIF:  {}\nRust: {}\nJava: {}", kif, rust_norm, java_norm);
    assert_eq!(rust_norm, java_norm,
        "\nKIF:  {}\nRust: {}\nJava: {}", kif, rust_norm, java_norm);
}

// ── Simple formulas ───────────────────────────────────────────────────────────

#[test] fn simple_instance()  { compare("(instance Foo Bar)"); }
#[test] fn simple_subclass()  { compare("(subclass Human Animal)"); }
#[test] fn implication()      { compare("(=> (instance ?X P) (instance ?X Q))"); }
#[test] fn conjunction()      { compare("(and (instance ?X A) (instance ?X B))"); }
#[test] fn disjunction()      { compare("(or (instance ?X A) (instance ?X B))"); }
#[test] fn negation()         { compare("(not (instance ?X A))"); }

// ── Complex formulas ──────────────────────────────────────────────────────────

#[test] fn nested_implication()  { compare("(=> (and (instance ?X A) (instance ?Y B)) (related ?X ?Y))"); }
#[test] fn biconditional()       { compare("(<=> (instance ?X A) (instance ?X B))"); }
#[test] fn forall_quantifier()   { compare("(forall (?X) (instance ?X Entity))"); }
#[test] fn exists_quantifier()   { compare("(exists (?X) (instance ?X Entity))"); }
#[test] fn nested_quantifiers()  { compare("(forall (?X) (exists (?Y) (related ?X ?Y)))"); }
#[test] fn equality()            { compare("(equal ?X ?Y)"); }
#[test] fn function_term()       { compare("(instance (WhenFn ?E) TimeInterval)"); }

// ── Mention suffix (embedded relation names) ──────────────────────────────────

#[test] fn lowercase_pred_as_arg() { compare("(instance subclass BinaryRelation)"); }
#[test] fn relation_as_arg()       { compare("(domain instance 1 Entity)"); }
#[test] fn function_name_as_arg()  { compare("(instance AdditionFn BinaryFunction)"); }

// ── Numbers ───────────────────────────────────────────────────────────────────

#[test] fn integer()         { compare("(lessThan ?X 0)"); }
#[test] fn decimal()         { compare("(lessThan ?X 3.14)"); }
#[test] fn negative_number() { compare("(lessThan -5 ?X)"); }

// ── Java unit-test strings (string1–string6 + embedded) ──────────────────────

#[test] fn string1()  { compare("(=> (instance ?X P)(instance ?X Q))"); }
#[test] fn string2()  { compare("(=> (or (instance ?X Q)(instance ?X R))(instance ?X ?T))"); }
#[test] fn string3()  { compare("(or (not (instance ?X Q))(instance ?X R))"); }
#[test] fn string4()  { compare("(<=> (instance ?NUMBER NegativeRealNumber) (and (lessThan ?NUMBER 0) (instance ?NUMBER RealNumber)))"); }
#[test] fn string6()  { compare("(<=> (temporalPart ?POS (WhenFn ?THING)) (time ?THING ?POS))"); }
#[test] fn embedded() { compare("(instance subclass BinaryRelation)"); }

// ── KB structural tests (no Java needed) ─────────────────────────────────────

const KB_FORMULAS: &[&str] = &[
    "(instance Foo Bar)",
    "(subclass Human Animal)",
    "(=> (instance ?X P) (instance ?X Q))",
    "(forall (?X) (exists (?Y) (related ?X ?Y)))",
    "(instance (WhenFn ?E) TimeInterval)",
    "(and (instance ?X A) (instance ?X B))",
    "(instance subclass BinaryRelation)",
];

#[test]
fn kb_axiom_count_matches_formula_count() {
    let kif_text = KB_FORMULAS.join("\n");
    let mut store = KifStore::default();
    load_kif(&mut store, &kif_text, "test");
    let kb = KnowledgeBase::new(store);
    let tptp = kb_to_tptp(&kb, "TestKB", &default_opts(), None);
    let axiom_count = tptp.lines().filter(|l| l.contains(",axiom,")).count();
    assert_eq!(axiom_count, KB_FORMULAS.len(),
        "Expected {} axioms, got {}\n{}", KB_FORMULAS.len(), axiom_count, tptp);
}

#[test]
fn kb_all_roles_are_axiom() {
    let kif_text = KB_FORMULAS.join("\n");
    let mut store = KifStore::default();
    load_kif(&mut store, &kif_text, "test");
    let kb = KnowledgeBase::new(store);
    let tptp = kb_to_tptp(&kb, "TestKB", &default_opts(), None);
    assert!(!tptp.contains(",hypothesis,"), "unexpected hypothesis role");
    assert!(tptp.lines().any(|l| l.contains(",axiom,")));
}

#[test]
fn kb_deduplication() {
    let content = "
        (instance Foo Bar)
        (instance Foo Bar)
        (subclass Human Animal)
        (instance Foo Bar)
        (subclass Human Animal)
    ";
    let mut store = KifStore::default();
    load_kif(&mut store, content, "test");
    let kb = KnowledgeBase::new(store);
    let tptp = kb_to_tptp(&kb, "DedupKB", &default_opts(), None);
    let count = tptp.lines().filter(|l| l.contains(",axiom,")).count();
    assert_eq!(count, 2, "Expected 2 unique axioms, got {}", count);
}

#[test]
fn kb_header_includes_credit() {
    let mut store = KifStore::default();
    load_kif(&mut store, "(instance Foo Bar)", "test");
    let kb = KnowledgeBase::new(store);
    let tptp = kb_to_tptp(&kb, "StructKB", &default_opts(), None);
    assert!(tptp.contains("% Articulate Software"), "missing credit header");
    assert!(tptp.contains("www.ontologyportal.org"),  "missing URL in header");
}

#[test]
fn kb_sequential_axiom_naming() {
    let content = "
        (instance Foo Bar)
        (subclass Human Animal)
        (=> (instance ?X A) (instance ?X B))
    ";
    let mut store = KifStore::default();
    load_kif(&mut store, content, "test");
    let kb = KnowledgeBase::new(store);
    let tptp = kb_to_tptp(&kb, "StructKB", &default_opts(), None);
    let axiom_lines: Vec<&str> = tptp.lines()
        .filter(|l| l.contains(",axiom,"))
        .collect();
    assert_eq!(axiom_lines.len(), 3);
    for (i, line) in axiom_lines.iter().enumerate() {
        let expected = format!("kb_StructKB_{}", i + 1);
        assert!(line.contains(&expected),
            "axiom {} should be named {}, got: {}", i + 1, expected, line);
    }
}

// ── KB-level Java comparison ──────────────────────────────────────────────────

#[test]
fn kb_axiom_bodies_match_java() {
    if let Some(reason) = skip_reason() {
        eprintln!("SKIP (kb_axiom_bodies_match_java): {}", reason);
        return;
    }
    let opts = default_opts();
    let mut failures: Vec<String> = Vec::new();

    for &formula in KB_FORMULAS {
        let mut store = KifStore::default();
        load_kif(&mut store, formula, "test");
        let kb = KnowledgeBase::new(store);
        let tptp = kb_to_tptp(&kb, "CmpKB", &opts, None);
        let bodies = extract_axiom_bodies(&tptp);
        let rust_body = match bodies.first() {
            Some(b) => b.clone(),
            None => { failures.push(format!("no axiom extracted for: {}", formula)); continue; }
        };

        let java_out = match invoke_java_converter(formula) {
            Ok(s)  => s,
            Err(e) => { eprintln!("SKIP {}: java error: {}", formula, e); continue; }
        };

        let rust_norm = normalize_tptp(&rust_body);
        let java_norm = normalize_tptp(&java_out);
        println!("KIF:       {}\nRust body: {}\nJava:      {}", formula, rust_norm, java_norm);

        if rust_norm != java_norm {
            failures.push(format!(
                "KIF: {}\n  Rust: {}\n  Java: {}", formula, rust_norm, java_norm
            ));
        }
    }
    assert!(failures.is_empty(), "Mismatches:\n{}", failures.join("\n\n"));
}

// ── tinySUMO.kif ─────────────────────────────────────────────────────────────

/// Verifies that `sentence_to_tptp` called individually produces the same body
/// as the formula embedded in the full `kb_to_tptp` output — mirrors the
/// tinySUMO consistency test in the JS suite.
#[test]
fn tiny_sumo_individual_vs_kb_bodies_consistent() {
    const TINY_SUMO: &str = "/home/iggy/.sigmakee/KBs/tinySUMO.kif";
    if !Path::new(TINY_SUMO).exists() {
        eprintln!("SKIP (tiny_sumo): {} not found", TINY_SUMO);
        return;
    }

    let text = std::fs::read_to_string(TINY_SUMO).expect("read tinySUMO.kif");
    let mut store = KifStore::default();
    load_kif(&mut store, &text, TINY_SUMO);
    let kb = KnowledgeBase::new(store);
    let opts = default_opts();

    let tptp = kb_to_tptp(&kb, "tinySUMO", &opts, None);
    let kb_bodies = extract_axiom_bodies(&tptp);
    assert!(!kb_bodies.is_empty(), "No axioms found in tinySUMO TPTP output");

    let limit = kb_bodies.len().min(10);
    let root_ids: Vec<_> = kb.store.roots.iter().copied().take(limit).collect();
    let mut compared = 0;

    for (i, &sid) in root_ids.iter().enumerate() {
        let individual = sentence_to_tptp(sid, &kb, &opts);
        let kb_body    = &kb_bodies[i];
        assert_eq!(
            normalize_tptp(&individual),
            normalize_tptp(kb_body),
            "Mismatch on axiom {} (sentence id {})", i + 1, sid
        );
        compared += 1;
    }
    assert!(compared > 0, "Should have compared at least one axiom");
    println!("Compared {} axiom bodies from tinySUMO.kif", compared);
}
