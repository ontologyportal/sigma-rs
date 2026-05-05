//! Port of the Java integration tests from
//! ontologyportal/sigmakee:
//!   test/integration/java/com/articulate/sigma/trans/SUMOtoTFAformTest.java
//!
//! Of 26 Java `@Test` methods, 5 are portable to Rust today as
//! semantic-property tests, and 21 depend on TFA/TFF machinery the
//! Rust port hasn't yet implemented.  See `TODO.md` for the missing-
//! feature roadmap (FOF section + TFA §A–§G).
//!
//! **Positive surprises during porting** (no TODO entries needed):
//! - **Row variable expansion (`@ROW`)** *is* implemented.  The Rust
//!   converter emits one axiom per arity for formulas containing row
//!   variables; see `testVariableArity` below for an explicit check.
//! - **Predicate variables in head position** (`(?REL @ROW ?ITEM)`)
//!   *are* handled — the variable becomes the first argument to a
//!   variadic `s__holds_app(...)` term.
//!
//! Helper-function tests in the Java suite (`testExtractSig`,
//! `testElimUnitaryLogops`, etc.) test pure utility functions whose
//! Rust equivalents do not exist; they're carried as `#[ignore]`
//! stubs only as a checklist.
//!
//! All tests load via `KnowledgeBase::load_kif` and emit with TFF mode.

use sigmakee_rs_core::{KnowledgeBase, TptpLang, TptpOptions};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn load_kif(kif: &str) -> KnowledgeBase {
    const SESSION: &str = "test.kif";
    let mut kb = KnowledgeBase::new();
    let r = kb.load_kif(kif, SESSION, Some(SESSION));
    assert!(r.ok, "failed to load KIF: {:?}", r.errors);
    kb.make_session_axiomatic(SESSION);
    kb
}

fn emit_tff(kb: &KnowledgeBase) -> String {
    kb.to_tptp(
        &TptpOptions { lang: TptpLang::Tff, hide_numbers: false, ..Default::default() },
        None,
    )
}

fn tff_axiom_lines(tptp: &str) -> Vec<&str> {
    tptp.lines()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("tff(kb_") && t.contains(", axiom,")
        })
        .collect()
}

fn count_top_level_universals(line: &str) -> usize {
    // Walks the chain `![X0] : ![X1] : …`.
    let mut count = 0;
    let mut s = match line.find('!') {
        Some(i) => &line[i..],
        None => return 0,
    };
    loop {
        if !s.starts_with('!') { break; }
        let after = s[1..].trim_start();
        if !after.starts_with('[') { break; }
        let body = &after[1..];
        let close = match body.find(']') { Some(i) => i, None => break };
        count += body[..close].split(',').filter(|v| !v.trim().is_empty()).count();
        let after_close = body[close + 1..].trim_start();
        if !after_close.starts_with(':') { break; }
        s = after_close[1..].trim_start();
    }
    count
}

// ---------------------------------------------------------------------------
// ported tests (Java tests with weak-enough or structural-enough
// assertions that survive Rust's surface differences)
// ---------------------------------------------------------------------------

/// Java `testTransNum`: `(=> (instance ?X NegativeInteger) (greaterThan 0 ?X))`
/// — Java only asserts the result is non-empty.  Same assertion in Rust.
#[test]
fn test_trans_num() {
    let kb = load_kif("(=> (instance ?X NegativeInteger) (greaterThan 0 ?X))");
    let tff = emit_tff(&kb);
    let lines = tff_axiom_lines(&tff);
    assert!(!lines.is_empty(), "expected non-empty TFF output, got:\n{}", tff);
}

/// Java `test1`: `(equal ?X (AdditionFn 1 2))`
///
/// Java expected: `! [V__X : $int] : (V__X = $sum(1 ,2))`
///
/// Rust output (no native arithmetic mapping for `AdditionFn`,
/// no sort annotation on quantifier):
///   `tff(kb_N, axiom, ![X0] : X0 = s__holds_app_3(s__AdditionFn__m,1,2)).`
///
/// Asserted properties:
/// - One top-level universal.
/// - TPTP equality `=` is the top-level connective body.
/// - The right-hand side reifies the function call: either as
///   native `$sum(1,2)` (Java parity, future) or as
///   `s__holds_app_3(s__AdditionFn__m,1,2)` (Rust today).
#[test]
fn test1() {
    let kb = load_kif("(equal ?X (AdditionFn 1 2))");
    let tff = emit_tff(&kb);
    let lines = tff_axiom_lines(&tff);
    assert_eq!(lines.len(), 1, "expected 1 axiom, got {}:\n{}", lines.len(), tff);
    let line = lines[0];

    assert_eq!(count_top_level_universals(line), 1,
        "expected 1 top-level universal: {}", line);
    assert!(line.contains(" = "), "missing TPTP equality: {}", line);

    let java_native = line.contains("$sum(1,2)") || line.contains("$sum(1 ,2)");
    let rust_holds  = line.contains("s__AdditionFn__m") && line.contains("s__holds_app_");
    assert!(java_native || rust_holds,
        "expected either native $sum or s__holds_app_*(s__AdditionFn__m,…): {}", line);

    // Constants 1 and 2 must appear.
    assert!(line.contains(",1,2") || line.contains(",1 ,2") || line.contains("1,2)"),
        "expected literals 1 and 2 in the function call: {}", line);
}

/// Java `testParents`:
///   `(=> (instance ?X Human) (parents ?X (AdditionFn 1 1)))`
///
/// Java expected:
///   `! [V__X : $i] : ((s__instance(V__X, s__Human) => s__parents(V__X, $sum(1 ,1))))`
///
/// The Java test mutates `kb.kbCache.signatures` to register a fake
/// `parents: (Human, Integer)` signature so the converter knows to
/// emit `$sum`.  We don't have signature plumbing for that in Rust
/// tests, and the converter doesn't translate `AdditionFn` to `$sum`
/// regardless.  Rust today reifies the function as
/// `s__holds_app_3(s__AdditionFn__m, 1, 1)`.
///
/// Asserted: instance/parents predicates, AdditionFn-as-term, single
/// top-level universal, implication.
#[test]
fn test_parents() {
    let kb = load_kif("(=> (instance ?X Human) (parents ?X (AdditionFn 1 1)))");
    let tff = emit_tff(&kb);
    let lines = tff_axiom_lines(&tff);
    assert_eq!(lines.len(), 1, "expected 1 axiom, got:\n{}", tff);
    let line = lines[0];

    assert!(line.contains("s__instance(") && line.contains("s__Human"),
        "missing s__instance(?, s__Human): {}", line);
    assert!(line.contains("s__parents("),
        "missing s__parents: {}", line);
    assert!(line.contains(" => "),
        "missing implication: {}", line);

    let java_native = line.contains("$sum(1,1)") || line.contains("$sum(1 ,1)");
    let rust_holds  = line.contains("s__AdditionFn__m");
    assert!(java_native || rust_holds,
        "expected either native $sum or s__AdditionFn__m: {}", line);

    assert_eq!(count_top_level_universals(line), 1,
        "expected 1 top-level universal: {}", line);

    // TFF preamble must declare these binary predicates.
    assert!(tff.contains("s__instance: ($i * $i) > $o"));
    assert!(tff.contains("s__parents: ($i * $i) > $o"));
}

/// Java `testPropertyFn`:
///   `(<=> (instance ?OBJ (PropertyFn ?PERSON)) (possesses ?PERSON ?OBJ))`
///
/// Java expected (biconditional expanded into two implications):
///   `! [V__OBJ : $i,V__PERSON : $i] :
///       (((s__instance(V__OBJ, s__PropertyFn(V__PERSON)) => s__possesses(V__PERSON, V__OBJ)) &
///         (s__possesses(V__PERSON, V__OBJ) => s__instance(V__OBJ, s__PropertyFn(V__PERSON)))))`
///
/// Rust output (uses ` <=> ` directly, no biconditional expansion;
/// PropertyFn reified through holds_app):
///   `tff(kb_N, axiom, ![X0] : ![X1] :
///       (s__instance(X0,s__holds_app_2(s__PropertyFn__m,X1)) <=> s__possesses(X1,X0))).`
#[test]
fn test_property_fn() {
    let kb = load_kif("(<=> (instance ?OBJ (PropertyFn ?PERSON)) (possesses ?PERSON ?OBJ))");
    let tff = emit_tff(&kb);
    let lines = tff_axiom_lines(&tff);
    assert_eq!(lines.len(), 1, "expected 1 axiom, got:\n{}", tff);
    let line = lines[0];

    assert!(line.contains("s__instance("),  "missing s__instance: {}", line);
    assert!(line.contains("s__possesses("), "missing s__possesses: {}", line);

    // Either ` <=> ` (Rust) or two implications joined by ` & ` (Java parity).
    let has_biconditional = line.contains(" <=> ");
    let has_two_imps = line.matches(" => ").count() >= 2 && line.contains(" & ");
    assert!(has_biconditional || has_two_imps,
        "expected either ` <=> ` or two implications joined by `&`: {}", line);

    // PropertyFn appears as a term (function reification).
    let rust_holds = line.contains("s__PropertyFn__m");
    let java_direct = line.contains("s__PropertyFn(");
    assert!(rust_holds || java_direct,
        "expected PropertyFn as a term (either reified or direct): {}", line);

    assert_eq!(count_top_level_universals(line), 2,
        "expected 2 top-level universals (?OBJ, ?PERSON): {}", line);
}

/// Java `testVariableArity`: row-variable formula.
/// Java expects exactly one expanded form (the relevant @ROW arity).
/// Rust **expands `@ROW` to multiple arities** automatically and
/// emits one axiom per arity (this is a positive parity item — the
/// expansion mechanism is already in place).
///
/// Asserted: more than one axiom is emitted (proves expansion
/// happened); each axiom contains the predicates we expect; no
/// crash on the predicate-variable head `(?REL @ROW ?ITEM)`.
#[test]
fn test_variable_arity() {
    let kif = "(<=> (and (instance ?REL TotalValuedRelation) (instance ?REL Predicate)) \
               (exists (?VALENCE) (and (instance ?REL Relation) (valence ?REL ?VALENCE) \
               (=> (forall (?NUMBER ?ELEMENT ?CLASS) \
                       (=> (and (lessThan ?NUMBER ?VALENCE) (domain ?REL ?NUMBER ?CLASS) \
                              (equal ?ELEMENT (ListOrderFn (ListFn @ROW) ?NUMBER))) \
                           (instance ?ELEMENT ?CLASS))) \
                   (exists (?ITEM) (?REL @ROW ?ITEM))))))";
    let kb = load_kif(kif);
    let tff = emit_tff(&kb);
    let lines = tff_axiom_lines(&tff);

    assert!(lines.len() >= 2,
        "row-variable expansion should produce multiple axioms (one per arity), got {}: \n{}",
        lines.len(), tff);

    // Each axiom should contain the core predicate signature.
    for line in &lines {
        assert!(line.contains("s__instance("),
            "missing s__instance in axiom: {}", line);
        assert!(line.contains("s__valence("),
            "missing s__valence in axiom: {}", line);
        assert!(line.contains("s__domain("),
            "missing s__domain in axiom: {}", line);
        assert!(line.contains("s__lessThan("),
            "missing s__lessThan in axiom: {}", line);
    }

    // The predicate-variable head `(?REL @ROW ?ITEM)` must reify to
    // a variadic holds_app — at least one of the emitted axioms
    // should mention `s__holds_app(` or `s__holds_app_<N>(` with the
    // predicate variable as first argument.
    let any_holds_app = lines.iter().any(|l| l.contains("s__holds_app"));
    assert!(any_holds_app,
        "expected predicate-variable head to be reified via s__holds_app: {}", tff);
}

// ---------------------------------------------------------------------------
// unportable Java tests, captured as #[ignore]d stubs.
// Each ignore reason names the missing Rust feature(s), cross-referenced
// to TODO.md sections.
// ---------------------------------------------------------------------------

/// `testExtractSig`: parses a sort-suffixed name like
/// `"ListFn__6Fn__0Ra1Ra2Ra3Ra4Ra5Ra6Ra"` into a list of class names.
///
/// Missing Rust feature: TODO.md §C (sort-suffix system) and the
/// helper `relationExtractSigFromName` does not exist in the Rust
/// crate.
#[test]
#[ignore = "missing TODO.md §C: sort-suffix parsing helper not implemented"]
fn test_extract_sig() {}

#[test]
#[ignore = "missing TODO.md §C: sort-suffix parsing helper not implemented"]
fn test_extract_update_sig() {}

#[test]
#[ignore = "missing TODO.md §C: sort-suffix parsing helper not implemented"]
fn test_extract_update_sig_2() {}

/// `testSorts`: queries `kb.kbCache.getSignature("AbsoluteValueFn__0Re1ReFn")`.
/// Missing: §C (sort-suffix system) plus a public KB-cache signature API.
#[test]
#[ignore = "missing TODO.md §C: sort-suffix system not implemented"]
fn test_sorts() {}

/// `test1_5`: `(equal ?X (SubtractionFn 2 1))` — same shape as `test1`
/// but for `SubtractionFn` and Java parity needs native `$difference`.
/// Rust today reifies as `s__holds_app_3(s__SubtractionFn__m,2,1)`.
///
/// Could be ported with a relaxed Rust-actual assertion identical to
/// `test1`, but skipping for now to avoid duplicate coverage of the
/// same converter behaviour `test1` already verifies.
#[test]
#[ignore = "missing TODO.md §B: native $difference mapping for SubtractionFn"]
fn test1_5() {
    let _kif = "(equal ?X (SubtractionFn 2 1))";
}

#[test]
#[ignore = "missing TODO.md §C,§E: sort suffixes + numericConstraints expansion"]
fn test2() {
    let _kif = "(=> (and (equal (AbsoluteValueFn ?NUMBER1) ?NUMBER2) \
                (instance ?NUMBER1 RealNumber) (instance ?NUMBER2 RealNumber)) \
                (or (and (instance ?NUMBER1 NonnegativeRealNumber) (equal ?NUMBER1 ?NUMBER2)) \
                (and (instance ?NUMBER1 NegativeRealNumber) (equal ?NUMBER2 (SubtractionFn 0 ?NUMBER1)))))";
}

#[test]
#[ignore = "missing TODO.md §B: native $remainder_t/$floor/$quotient_e/$sum/$product"]
fn test3() {
    let _kif = "(<=> (equal (RemainderFn ?NUMBER1 ?NUMBER2) ?NUMBER) \
                (equal (AdditionFn (MultiplicationFn (FloorFn (DivisionFn ?NUMBER1 ?NUMBER2)) ?NUMBER2) ?NUMBER) ?NUMBER1))";
}

#[test]
#[ignore = "missing TODO.md §B,§A: native $greatereq/$greater + sort-typed quantifiers"]
fn test4() {
    let _kif = "(<=> (greaterThanOrEqualTo ?NUMBER1 ?NUMBER2) \
                (or (equal ?NUMBER1 ?NUMBER2) (greaterThan ?NUMBER1 ?NUMBER2)))";
}

/// `test5`: tests `SUMOtoTFAform.modifyTypesToConstraints` directly —
/// it rewrites `(instance ?X NumericClass)` to the class's
/// `numericConstraints` body before TFF emission.
#[test]
#[ignore = "missing TODO.md §E,§G: modifyTypesToConstraints helper + numericConstraints map"]
fn test5() {
    let _kif = "(=>\n(measure ?QUAKE (MeasureFn ?VALUE RichterMagnitude))\n\
                (instance ?VALUE PositiveRealNumber))";
}

/// `testFloorFn`: identical input to `test3`.  Same gaps.
#[test]
#[ignore = "missing TODO.md §B: native $remainder_t/$floor/$quotient_e/$sum/$product"]
fn test_floor_fn() {}

/// `testNumericSubclass`: identical input to `test2`.  Same gaps.
#[test]
#[ignore = "missing TODO.md §C,§E: sort suffixes + numericConstraints expansion"]
fn test_numeric_subclass() {}

/// `testElimUnitaryLogops`: tests the `elimUnitaryLogops` helper.
#[test]
#[ignore = "missing TODO.md §G: elimUnitaryLogops helper not implemented"]
fn test_elim_unitary_logops() {}

#[test]
#[ignore = "missing TODO.md §C,§A,§B: sort suffixes + sort-typed quantifiers + native arithmetic"]
fn test_variable_arity_2() {
    let _kif = "(<=> (and (instance stringLength TotalValuedRelation) \
                (instance stringLength Predicate)) (exists (?VALENCE) \
                (and (instance stringLength Relation) (valence stringLength ?VALENCE) \
                (=> (forall (?NUMBER ?ELEMENT ?CLASS) \
                    (=> (and (lessThan ?NUMBER ?VALENCE) (domain stringLength ?NUMBER ?CLASS) \
                            (equal ?ELEMENT (ListOrderFn (ListFn @ROW) ?NUMBER))) \
                        (instance ?ELEMENT ?CLASS))) \
                    (exists (?ITEM) (stringLength @ROW ?ITEM))))))";
}

#[test]
#[ignore = "missing TODO.md §C,§A,§B: sort suffixes + sort-typed quantifiers + native arithmetic"]
fn test_pred_var_arity() {
    // Java input uses sort-suffixed predicate name `greaterThan__1Ra2Ra`.
}

#[test]
#[ignore = "missing TODO.md §C,§A: sort suffixes + sort-typed quantifiers + $to_real"]
fn test_remove_num_inst() {}

/// `testInstNum`: input `(instance equal RelationExtendedToQuantities)` —
/// the Rust KIF parser rejects `equal` in argument position
/// (`OperatorOutOfPosition`).  Same blocker as the unit-test
/// `embedded` case; see the FOF section of TODO.md.
#[test]
#[ignore = "blocked by KIF parser's `equal` reservation; see TODO.md FOF §8"]
fn test_inst_num() {}

/// `testTypeConflict*`: rely on a type-inference / type-conflict
/// detection pass (Java's `inconsistentVarTypes`,
/// `findAllTypeRestrictions`, `missingSorts`, `typeConflict`) that
/// has no Rust equivalent.
#[test]
#[ignore = "missing type-conflict detection pass (no Rust equivalent of inconsistentVarTypes)"]
fn test_type_conflict() {}

#[test]
#[ignore = "missing TODO.md §C,§A: sort suffixes + sort-typed quantifiers; type-conflict pass also missing"]
fn test_type_conflict_2() {}

#[test]
#[ignore = "missing type-conflict detection pass (Rust accepts the formula and universally binds free ?Y)"]
fn test_type_conflict_3() {
    // Java expects this to be rejected because ?Y is unbound.  Rust
    // currently emits ![X1]:![X0]:(s__instance(X0,s__Table) => s__agent(X1,X0))
    // — i.e. it universally quantifies the free variable rather than
    // signalling a conflict.
}

#[test]
#[ignore = "missing type-conflict detection pass + sort-suffix parsing"]
fn test_type_conflict_4() {}

#[test]
#[ignore = "missing TODO.md §A: sort-typed quantifiers ($int annotations on V__NUMBER)"]
fn test_member_type_count() {
    let _kif = "(=> (and (memberTypeCount ?GROUP ?TYPE ?NUMBER) (equal ?NUMBER 0)) \
                (not (exists (?ITEM) (and (instance ?ITEM ?TYPE) (member ?ITEM ?GROUP)))))";
}
