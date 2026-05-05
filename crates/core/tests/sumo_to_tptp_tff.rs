//! Port of the Java `SUMOtoTFATest` test cases from
//! ontologyportal/sigmakee:
//!   test/unit/java/com/articulate/sigma/trans/SUMOtoTFATest.java
//!
//! Of the 28 Java `@Test` methods, only `test1` and `test2` translate
//! to Rust today.  The other 26 depend on TFA/TFF machinery that the
//! Rust port (`crates/core/src/vampire/converter.rs` +
//! `crates/vampire-rs/vampire/src/ir/`) does not yet implement.  See
//! `TODO.md` for the complete missing-feature roadmap (sections A–G:
//! sort-typed quantifiers, native-arithmetic mapping for SUMO
//! predicates, sort-suffixed function names, the `preProcess` step,
//! `numericConstraints`, `numericConstantValues`, and the Java
//! formula-level helpers).
//!
//! For each unportable Java test we keep a `#[test] #[ignore]` stub
//! whose `ignore` reason names the missing feature and whose body
//! contains the Java KIF input verbatim.  When the corresponding Rust
//! feature lands, removing the `#[ignore]` and filling in assertions
//! becomes the next step — the stubs are a TODO list with code-shaped
//! affordances.

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

fn single_tff_axiom(kb: &KnowledgeBase) -> String {
    let tff = emit_tff(kb);
    let lines = tff_axiom_lines(&tff);
    assert_eq!(
        lines.len(), 1,
        "expected exactly 1 tff(kb_…, axiom, …) line, got {}:\n{}",
        lines.len(), tff
    );
    lines[0].to_string()
}

// ---------------------------------------------------------------------------
// ported tests (Java tests that map to current Rust capabilities)
// ---------------------------------------------------------------------------

/// Java `test1`: `(instance Foo Bar)` → `s__instance(s__Foo, s__Bar)`
///
/// Rust output (with TFF preamble):
///   `tff(pred_s__instance_2, type, s__instance: ($i * $i) > $o).`
///   `tff(kb_N, axiom, s__instance(s__Foo,s__Bar)).`
///
/// Apart from whitespace around the comma, the axiom body matches Java
/// verbatim — `instance` is a KB-declared binary predicate, so it is
/// emitted directly (not holds-reified) in TFF mode.
#[test]
fn test1() {
    let kb = load_kif("(instance Foo Bar)");
    let line = single_tff_axiom(&kb);

    // Body, stripped of the `tff(kb_N, axiom, ` wrapper and trailing `).`.
    assert!(line.contains("s__instance(s__Foo,s__Bar)"),
        "expected direct s__instance(s__Foo,s__Bar) in body: {}", line);
    // No quantifiers — ground formula.
    assert!(!line.contains("![") && !line.contains("?["),
        "ground formula must have no quantifiers: {}", line);
    // No holds-reification needed for a known binary predicate.
    assert!(!line.contains("s__holds("),
        "instance is TFF-declared and should not be holds-reified: {}", line);

    // The TFF preamble must declare s__instance as a binary i-predicate.
    let tff = emit_tff(&kb);
    assert!(
        tff.contains("s__instance: ($i * $i) > $o"),
        "expected TFF type declaration for s__instance: {}", tff
    );
}

/// Java `test2`: `(forall (?X) (=> (instance ?X Human) (attribute ?X Mortal)))`
///
/// Java expected:
///   `( ! [V__X:$i] : (s__instance(V__X, s__Human) => s__attribute(V__X, s__Mortal)))`
///
/// Rust output (after Phase 1.2 — TODO.md §A's default-typed-quantifier fix):
///   `tff(kb_N, axiom, ![X0: $i] : (s__instance(X0,s__Human) => s__attribute(X0,s__Mortal))).`
///
/// Difference from Java still in flight:
/// - Variables are fresh indices `X0` not preserved KIF names `V__X`.
///
/// What we assert (semantic structure that matches Java):
/// - One top-level universal **with `:$i` sort annotation**.
/// - `s__instance(?, s__Human)` antecedent.
/// - `s__attribute(?, s__Mortal)` consequent.
/// - Implication operator ` => `.
/// - Both predicates have TFF type declarations in the preamble.
#[test]
fn test2() {
    let kb = load_kif("(forall (?X) (=> (instance ?X Human) (attribute ?X Mortal)))");
    let tff = emit_tff(&kb);
    let line = single_tff_axiom(&kb);

    // Top-level universal with exactly one chained `![…]` bracket and the
    // `:$i` sort annotation Phase 1.2 introduced.
    assert!(line.contains("![X"),
        "expected top-level universal: {}", line);
    let universal_count = line.matches("![X").count();
    assert_eq!(universal_count, 1,
        "expected one top-level universal, got {} in {}", universal_count, line);
    assert!(line.contains(": $i]"),
        "expected typed `:$i` annotation on TFF quantifier (Phase 1.2): {}", line);

    // Antecedent / consequent / connective.
    assert!(line.contains("s__instance(") && line.contains("s__Human"),
        "missing s__instance(?, s__Human) antecedent: {}", line);
    assert!(line.contains("s__attribute(") && line.contains("s__Mortal"),
        "missing s__attribute(?, s__Mortal) consequent: {}", line);
    assert!(line.contains(" => "), "missing implication: {}", line);

    // Predicates declared in preamble.
    assert!(tff.contains("s__instance: ($i * $i) > $o"),
        "missing TFF declaration for s__instance: {}", tff);
    assert!(tff.contains("s__attribute: ($i * $i) > $o"),
        "missing TFF declaration for s__attribute: {}", tff);
}

// ---------------------------------------------------------------------------
// unportable Java tests, captured as #[ignore]d stubs
//
// Each stub names the Java test it mirrors, lists the missing Rust
// feature(s) (cross-referenced to TODO.md §-letters), keeps the Java
// KIF input verbatim, and stops short of asserting on the Java
// expected output.  Removing the `#[ignore]` and filling in
// assertions is the next step once the cited Rust feature lands.
// ---------------------------------------------------------------------------

/// Java `testBuildConstraints`: queries
/// `SUMOtoTFAform.numericConstraints.get("NonnegativeRealNumber")`
/// and asserts the constraint string
///   `(or (equal (SignumFn ?NUMBER) 1) (equal (SignumFn ?NUMBER) 0))`.
///
/// Missing Rust feature: TODO.md §E (numeric range constraints).
/// `SemanticLayer::numeric_char_cache` exists but is not exposed at
/// the public crate API and is not consulted during TFF emission.
#[test]
#[ignore = "missing TODO.md §E: numeric range constraints not surfaced at public API"]
fn test_build_constraints() {
    // No assertion possible until §E lands.
}

/// Java `test3`–`test7`: rely on `SUMOtoTFAform.fp.preProcess(formula, false, kb)`
/// to add `(instance ?VAR <Class>)` type-hypotheses inferred from
/// predicate signatures (e.g. `subProcess: (Process, Process)` →
/// `(instance ?S1 Process)` is added before the body).
///
/// Missing Rust feature: TODO.md §D (preProcess type-hypothesis step).
/// Without it, the Rust converter does not synthesise the
/// `s__instance(?, s__Process)` antecedents the Java expected
/// strings include, so the structural assertions can't be written.
#[test]
#[ignore = "missing TODO.md §D: preProcess does not add type-hypotheses"]
fn test3() {
    let _kif = "(=>\n\
                (and\n\
                    (subProcess ?S1 ?P)\n\
                    (subProcess ?S2 ?P))\n\
                (relatedEvent ?S1 ?S2))";
}

#[test]
#[ignore = "missing TODO.md §D: preProcess does not add type-hypotheses"]
fn test4() {
    let _kif = "(=>\n\
                (and\n\
                    (instance ?DEV ElectricDevice)\n\
                    (instance ?EV Process)\n\
                    (instrument ?EV ?DEV))\n\
                (exists (?R)\n\
                    (and\n\
                        (instance ?R Electricity)\n\
                        (resource ?EV ?R))))";
}

#[test]
#[ignore = "missing TODO.md §D: preProcess does not add type-hypotheses"]
fn test5() {
    let _kif = "(=>\n\
                (and\n\
                    (instance ?PROC Process)\n\
                    (eventLocated ?PROC ?LOC)\n\
                    (subProcess ?SUB ?PROC))\n\
                (eventLocated ?SUB ?LOC))";
}

#[test]
#[ignore = "missing TODO.md §D: preProcess does not add type-hypotheses"]
fn test6() {
    let _kif = "(=> (and (equal (PathWeightFn ?PATH) ?SUM) (graphPart ?ARC1 ?PATH) \
                (graphPart ?ARC2 ?PATH) (arcWeight ?ARC1 ?NUMBER1) (arcWeight ?ARC2 ?NUMBER2) \
                (forall (?ARC3) (=> (graphPart ?ARC3 ?PATH) (or (equal ?ARC3 ?ARC1) (equal ?ARC3 ?ARC2))))) \
                (equal (PathWeightFn ?PATH) (AdditionFn ?NUMBER1 ?NUMBER2)))";
}

#[test]
#[ignore = "missing TODO.md §D: preProcess does not add type-hypotheses"]
fn test7() {
    let _kif = "(exists (?ARC1 ?ARC2 ?PATH) (and (graphPart ?ARC1 ?PATH) \
                (graphPart ?ARC2 ?PATH) (arcWeight ?ARC1 ?NUMBER1)))";
}

// Java `test8` is `@Ignore`d in the source ("includes ListFn") — we
// don't carry it across.  Same for `testInList` and `testLeastCommon`.

/// Java `test9`: integers + `lessThan` + `SuccessorFn`.
///
/// Java expected uses native `$less(...)` for `lessThan` and the
/// sort-suffixed `s__SuccessorFn__0In1InFn` form.  Rust today emits
/// `s__lessThan(X, Y)` (uninterpreted predicate) and
/// `s__holds_app_2(s__SuccessorFn__m, X)` (holds-reified function).
///
/// Missing Rust features: TODO.md §B (native-arithmetic mapping for
/// SUMO predicates) and §C (sort-suffixed function names).  `test9`
/// also expects sort-typed quantifiers (`![X:$int]`) — TODO.md §A.
#[test]
#[ignore = "missing TODO.md §A,§B,§C: sort-typed quantifiers + native arithmetic + sort suffixes"]
fn test9() {
    let _kif = "(=> (and (instance ?INT1 Integer) (instance ?INT2 Integer)) \
                (not (and (lessThan ?INT1 ?INT2) (lessThan ?INT2 (SuccessorFn ?INT1)))))";
}

/// Java `testElimLogops`: tests the `SUMOtoTFAform.elimUnitaryLogops`
/// helper (strips degenerate unary `(=> nil X)`-shaped wrappers).
///
/// Missing Rust feature: TODO.md §G (no such helper exists).
#[test]
#[ignore = "missing TODO.md §G: elimUnitaryLogops helper not implemented"]
fn test_elim_logops() {}

#[test]
#[ignore = "missing TODO.md §C,§D,§A: sort suffixes + preProcess + sort-typed quantifiers"]
fn test_temporal_comp() {
    let _kif = "(=> (and (instance ?MONTH Month) (duration ?MONTH (MeasureFn ?NUMBER DayDuration))) \
                (equal (CardinalityFn (TemporalCompositionFn ?MONTH Day)) ?NUMBER))";
}

#[test]
#[ignore = "missing TODO.md §C,§D,§A: sort suffixes + preProcess + sort-typed quantifiers"]
fn test_big_number() {
    let _kif = "(=> (and (instance ?UNIT UnitOfMeasure) (equal ?TERAUNIT (TeraFn ?UNIT))) \
                (equal (MeasureFn 1 ?TERAUNIT) (MeasureFn 1000000000 (KiloFn ?UNIT))))";
}

#[test]
#[ignore = "missing TODO.md §C,§D,§A: sort suffixes + preProcess + sort-typed quantifiers"]
fn test_number() {
    let _kif = "(=> (diameter ?CIRCLE ?LENGTH) (exists (?HALF) \
                (and (radius ?CIRCLE ?HALF) (equal (MultiplicationFn ?HALF 2) ?LENGTH))))";
}

/// Java `testMostSpecific`: asserts
///   `mostSpecificType(["RealNumber", "LengthMeasure"]) == "RealNumber"`
///
/// Rust port: `KnowledgeBase::most_specific_class(&[&str]) -> Option<String>`
/// returns the class that has every other candidate as an ancestor
/// (i.e. the deepest descendant in the loaded subclass taxonomy).
///
/// Java's exact semantics returns `"RealNumber"` for the pair
/// `["RealNumber", "LengthMeasure"]`, which doesn't match a pure
/// "subclass-deepest" interpretation in vanilla SUMO (LengthMeasure is
/// not a subclass of RealNumber there); Java's helper appears to be
/// picking the *most-specific numeric sort* via a separate ranking, not
/// the deepest taxonomy descendant.  The Rust helper implements the
/// taxonomy-based semantics that's actually well-defined from the
/// loaded data — driven by `SemanticLayer::has_ancestor` — and that's
/// what we test here.  See TODO.md §G.
#[test]
fn test_most_specific() {
    // Build a small subclass chain: Mammal ⊂ Animal ⊂ Organism.
    let kif = "(subclass Animal Organism)\n(subclass Mammal Animal)";
    let kb  = load_kif(kif);

    // All three candidates: Mammal is the most specific (descends from
    // both Animal and Organism).
    assert_eq!(
        kb.most_specific_class(&["Organism", "Animal", "Mammal"]).as_deref(),
        Some("Mammal"),
        "Mammal is the deepest descendant",
    );
    // Order shouldn't matter.
    assert_eq!(
        kb.most_specific_class(&["Mammal", "Organism", "Animal"]).as_deref(),
        Some("Mammal"),
    );

    // Two-element case: parent + child returns the child.
    assert_eq!(
        kb.most_specific_class(&["Organism", "Animal"]).as_deref(),
        Some("Animal"),
    );

    // Single-element case: returns that element.
    assert_eq!(
        kb.most_specific_class(&["Mammal"]).as_deref(),
        Some("Mammal"),
    );

    // Empty input: None.
    assert_eq!(kb.most_specific_class(&[]), None);

    // Antichain (no class dominates the other) returns None.
    let kif2 = "(subclass Animal Organism)\n(subclass Plant Organism)";
    let kb2  = load_kif(kif2);
    assert_eq!(
        kb2.most_specific_class(&["Animal", "Plant"]),
        None,
        "Animal and Plant are siblings; neither dominates",
    );
}

#[test]
#[ignore = "missing TODO.md §C,§D,§A: sort suffixes + preProcess + sort-typed quantifiers"]
fn test_temporal_comp2() {
    // Identical input to testTemporalComp; the Java suite duplicates it.
    let _kif = "(=> (and (instance ?MONTH Month) (duration ?MONTH (MeasureFn ?NUMBER DayDuration))) \
                (equal (CardinalityFn (TemporalCompositionFn ?MONTH Day)) ?NUMBER))";
}

#[test]
#[ignore = "missing TODO.md §B,§C,§D,§A: native arithmetic + sort suffixes + preProcess + sort-typed quantifiers"]
fn test_ceiling() {
    let _kif = "(=> (equal (CeilingFn ?NUMBER) ?INT) (not (exists (?OTHERINT) \
                (and (instance ?OTHERINT Integer) \
                     (greaterThanOrEqualTo ?OTHERINT ?NUMBER) (lessThan ?OTHERINT ?INT)))))";
}

#[test]
#[ignore = "missing TODO.md §B,§C,§A: native $product + sort suffixes + sort-typed quantifiers"]
fn test_mult() {
    let _kif = "(=> (equal (SquareRootFn ?NUMBER1) ?NUMBER2) \
                (equal (MultiplicationFn ?NUMBER2 ?NUMBER2) ?NUMBER1))";
}

#[test]
#[ignore = "missing TODO.md §B,§C,§D: native $lesseq + sort suffixes + preProcess"]
fn test_day() {
    let _kif = "(=> (instance ?DAY (DayFn ?NUMBER ?MONTH)) (lessThanOrEqualTo ?NUMBER 31))";
}

#[test]
#[ignore = "missing TODO.md §C,§D,§A: sort suffixes + preProcess + sort-typed quantifiers"]
fn test_exponent() {
    let _kif = "(=> (instance ?NUMBER Quantity) \
                (equal (ReciprocalFn ?NUMBER) (ExponentiationFn ?NUMBER -1)))";
}

#[test]
#[ignore = "missing TODO.md §B,§D,§E: native $greater + preProcess + NonnegativeInteger constraint"]
fn test_instance() {
    let _kif = "(=> (instance ?SET FiniteSet) \
                (exists (?NUMBER) (and (instance ?NUMBER NonnegativeInteger) \
                (equal ?NUMBER (CardinalityFn ?SET)))))";
}

#[test]
#[ignore = "missing TODO.md §B,§C,§F,§A: native $product/$quotient + sort suffixes + Pi resolution + sort-typed quantifiers"]
fn test_radian() {
    let _kif = "(equal (MeasureFn ?NUMBER AngularDegree) \
                (MeasureFn (MultiplicationFn ?NUMBER (DivisionFn Pi 180)) Radian))";
}

#[test]
#[ignore = "missing TODO.md §B (no $to_int, $sum, $product wiring): floor + arithmetic"]
fn test_floor() {
    let _kif = "(equal (MillionYearsAgoFn ?X) (BeginFn (YearFn (FloorFn \
                (AdditionFn 1950 (MultiplicationFn ?X -1000000))))))";
}

#[test]
#[ignore = "missing TODO.md §C,§A: sort suffixes (RemainderFn__0In1In2InFn) + sort-typed quantifiers"]
fn test_prime() {
    let _kif = "(=> (instance ?PRIME PrimeNumber) (forall (?NUMBER) (=> \
                (equal (RemainderFn ?PRIME ?NUMBER) 0) \
                (or (equal ?NUMBER 1) (equal ?NUMBER ?PRIME)))))";
}

// Java `testAvgWork` is `@Ignore`d in the source ("requires loading an
// unavailable kif file"); not carried across.

/// Java `testComposeSuffix`: tests the `SUMOtoTFAform.composeSuffix`
/// helper — a string operation used to compose sort-suffix tags.
///
/// Missing Rust feature: TODO.md §G (no `composeSuffix` helper, and
/// the entire sort-suffix system §C does not exist).
#[test]
#[ignore = "missing TODO.md §G: composeSuffix helper not implemented (and §C: sort-suffix system absent)"]
fn test_compose_suffix() {}
