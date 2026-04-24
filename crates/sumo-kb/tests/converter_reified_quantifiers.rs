//! Regression tests for the FOF converter when KIF sentences contain
//! quantifiers nested inside a relation's argument position
//! (the SUMO "reified formula" idiom, used e.g. by `hasPurpose`).
//!
//! These quantifiers are translated as ground function terms
//! (`s__exists_op(?V, ...)`), so the variables inside them remain free
//! in the surrounding FOF sentence and must receive a *top-level*
//! universal from `wrap_free_vars`.  A pre-fix bug counted those
//! variables as "bound" and produced formulas like
//!     `![X1] : (... => hasPurpose(X1, s__exists_op(X3, p(X3))))`
//! which Vampire rejects with "unquantified variable detected".

use sumo_kb::{KnowledgeBase, TptpOptions, TptpLang};

fn load_kif(kif: &str) -> KnowledgeBase {
    const SESSION: &str = "test.kif";
    let mut kb = KnowledgeBase::new();
    let r = kb.load_kif(kif, SESSION, Some(SESSION));
    assert!(r.ok, "failed to load KIF: {:?}", r.errors);
    kb.make_session_axiomatic(SESSION);
    kb
}

fn emit_fof(kb: &KnowledgeBase) -> String {
    kb.to_tptp(
        &TptpOptions { lang: TptpLang::Fof, ..Default::default() },
        None,
    )
}

/// Count the distinct `X<N>` variable names used in `formula`.
fn xvars_used(formula: &str) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let bytes = formula.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'X' {
            // only lexically isolated names — skip suffixes in identifiers
            // like `s__sk_X3_foo` by requiring the preceding char to be a
            // non-identifier.
            let prev_ok = i == 0
                || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() { j += 1; }
            if prev_ok && j > i + 1 {
                out.insert(formula[i..j].to_string());
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// Count the `X<N>` names *declared* by a FOF quantifier
/// (`![X<N>]` or `?[X<N>]`).
fn xvars_quantified(formula: &str) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let mut start = 0;
    while let Some(rel) = formula[start..].find(|c: char| c == '!' || c == '?') {
        let at = start + rel;
        let rest = &formula[at + 1..];
        if !rest.starts_with('[') { start = at + 1; continue; }
        if let Some(close) = rest.find(']') {
            let vars_chunk = &rest[1..close];
            for v in vars_chunk.split(',') {
                let v = v.trim();
                // strip optional sort annotation like `X1: $int`
                let name = v.split(':').next().unwrap_or("").trim();
                if name.starts_with('X')
                    && name.len() > 1
                    && name[1..].chars().all(|c| c.is_ascii_digit())
                {
                    out.insert(name.to_string());
                }
            }
            start = at + 1 + close + 1;
        } else {
            break;
        }
    }
    out
}

/// Every `X<N>` that appears in the formula must have a matching
/// `![X<N>]` or `?[X<N>]` somewhere in scope.  Since FOF quantifiers
/// in the converter always sit at the top of the sentence (or inside
/// a real quantifier), "in scope" collapses to "quantified anywhere
/// in the formula text" — any X<N> used but not declared is a free
/// variable and Vampire will reject it.
fn assert_no_free_variables(formula: &str) {
    let used = xvars_used(formula);
    let quantified = xvars_quantified(formula);
    let free: Vec<_> = used.difference(&quantified).cloned().collect();
    assert!(
        free.is_empty(),
        "formula has free variables {:?} — these would trigger Vampire's \
         'unquantified variable detected' error.\n\
         used = {:?}\nquantified = {:?}\nformula = {}",
        free, used, quantified, formula
    );
}

#[test]
fn reified_exists_in_relation_arg_leaves_no_free_vars() {
    // Mimics the SUMO axiom that produced kb_35565:
    //   "(=> (instance ?D Device)
    //        (hasPurpose ?D
    //                    (exists (?USE) (instance ?USE Process))))"
    // Before the fix, ?USE was collected as "bound" by the walk even
    // though it ends up reified inside s__exists_op(...).
    let kif = r#"
        (=> (instance ?D Device)
            (hasPurpose ?D
                (exists (?USE) (instance ?USE Process))))
    "#;
    let kb = load_kif(kif);
    let tptp = emit_fof(&kb);
    assert!(
        tptp.contains("exists_op"),
        "expected reified exists in output: {}", tptp
    );
    for line in tptp.lines() {
        if line.starts_with("fof(") {
            assert_no_free_variables(line);
        }
    }
}

#[test]
fn multiple_reified_quantifiers_all_quantified_at_top() {
    // Two reified quantifiers under a non-logical relation, with
    // disjoint inner variables — all of them must be quantified at
    // the FOL top level, alongside the genuine outer universals.
    let kif = r#"
        (=> (and (instance ?A Agent) (instance ?P Process))
            (hasPurpose ?A
                (and
                    (exists (?B) (agent ?B ?A))
                    (exists (?C ?D) (and (instance ?C Event) (instance ?D Time))))))
    "#;
    let kb = load_kif(kif);
    let tptp = emit_fof(&kb);
    for line in tptp.lines() {
        if line.starts_with("fof(") {
            assert_no_free_variables(line);
        }
    }
}

#[test]
fn reified_implication_uses_safe_tptp_name() {
    // `(hasPurpose ?X (=> A B))` reifies the implication as a function
    // term.  If the emitter used `OpKind::name()` directly the result
    // would be `s__=>_op(...)`, and Vampire's TPTP parser would split
    // around `=>` and report "Non-boolean term X<n> of sort $i used
    // in a formula context".  The safe-name helper must produce
    // `s__imp_op(...)` instead.  Same story for `<=>` → `s__iff_op`.
    let kif = r#"
        (=> (instance ?X Agent)
            (hasPurpose ?X
                (=> (instance ?Y Process) (agent ?Y ?X))))
    "#;
    let kb = load_kif(kif);
    let tptp = emit_fof(&kb);
    assert!(
        tptp.contains("s__imp_op"),
        "expected reified `=>` as `s__imp_op`: {}",
        tptp
    );
    assert!(
        !tptp.contains("s__=>_op"),
        "raw `=>` leaked into identifier — Vampire will reject: {}",
        tptp
    );
    for line in tptp.lines() {
        if line.starts_with("fof(") {
            assert_no_free_variables(line);
        }
    }
}

#[test]
fn reified_iff_uses_safe_tptp_name() {
    let kif = r#"
        (=> (instance ?X Object)
            (hasPurpose ?X (<=> (attribute ?X Red) (attribute ?X Colored))))
    "#;
    let kb = load_kif(kif);
    let tptp = emit_fof(&kb);
    assert!(
        tptp.contains("s__iff_op"),
        "expected reified `<=>` as `s__iff_op`: {}",
        tptp
    );
    assert!(
        !tptp.contains("s__<=>_op"),
        "raw `<=>` leaked into identifier — Vampire will reject: {}",
        tptp
    );
}

#[test]
fn genuine_top_level_exists_still_binds_locally() {
    // Sanity check: a real `(exists ...)` in formula position must NOT
    // be pulled to the top — it should compile to an inner `?[X<N>]`
    // with the variable used only inside.
    let kif = r#"
        (=> (instance ?X Animal)
            (exists (?Y) (attribute ?X ?Y)))
    "#;
    let kb = load_kif(kif);
    let tptp = emit_fof(&kb);
    // Still must pass the no-free-variable invariant.
    for line in tptp.lines() {
        if line.starts_with("fof(") {
            assert_no_free_variables(line);
        }
    }
    // And the `?[...]` quantifier for ?Y should appear inside the
    // implication, not on the outermost layer before the first `!`.
    let has_exists_inside = tptp.lines().any(|l| {
        if !l.starts_with("fof(") { return false; }
        l.find('?').map(|q| l[..q].contains("=>")).unwrap_or(false)
    });
    assert!(has_exists_inside, "genuine exists should stay nested: {}", tptp);
}
