//! Port of the Java `SUMOformulaToTPTPformulaTest` test cases from
//! ontologyportal/sigmakee:
//!   test/unit/java/com/articulate/sigma/trans/SUMOformulaToTPTPformulaTest.java
//!
//! Each Java `@Test` is mirrored 1:1 here.  The Rust converter emits a
//! different surface syntax than the Java translator, so we assert
//! *semantic properties* rather than literal byte-for-byte equality:
//!
//!   * Variables are fresh indices (`X0, X1, …`), not the preserved
//!     KIF names (`V__X, V__NUMBER`) Java uses.
//!   * Each axiom is wrapped as `fof(kb_<sid>, axiom, body).`, not
//!     Java's bracket-only multi-paren body.
//!   * The Rust FOF mode uses **holds-reification**: every predicate
//!     atom `(P a b)` becomes `s__holds(s__P__m, a, b)`, and every
//!     function term `(F a b)` becomes `s__holds_app_N(s__F__m, a, b)`.
//!     Java emits the predicate / function name directly.
//!   * Top-level universals are emitted as a chain
//!     `![X0] : ![X1] : ...`, not as a shared bracket `![X0,X1]`.
//!   * Without typing signatures loaded, predicate names appearing as
//!     arguments (e.g. the second `minValue` in the `equality` test, or
//!     `instrument` in the `hol` test) are emitted as bare `s__name`
//!     rather than the `__m` mention form Java uses.  The properties we
//!     assert track Rust's actual behaviour.
//!
//! All tests load via `KnowledgeBase::load_kif` and emit with
//! `hide_numbers: true`, matching the Java `@Before init` that calls
//! `setHideNumbers(true)` and `setLang("fof")`.

use std::collections::BTreeSet;
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

fn emit_fof_hide_numbers(kb: &KnowledgeBase) -> String {
    kb.to_tptp(
        &TptpOptions { lang: TptpLang::Fof, hide_numbers: true, ..Default::default() },
        None,
    )
}

/// Distinct `X<N>` names that appear lexically in `formula`.
fn xvars_used(formula: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let bytes = formula.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'X' {
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

/// `X<N>` names declared by *any* `![...]` or `?[...]` in `formula`.
fn xvars_quantified(formula: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut start = 0;
    while let Some(rel) = formula[start..].find(|c: char| c == '!' || c == '?') {
        let at = start + rel;
        let rest = &formula[at + 1..];
        let trimmed = rest.trim_start();
        if !trimmed.starts_with('[') { start = at + 1; continue; }
        let bracket_off = (rest.len() - trimmed.len()) + 1;
        let after_bracket = &rest[bracket_off..];
        if let Some(close) = after_bracket.find(']') {
            for v in after_bracket[..close].split(',') {
                let name = v.split(':').next().unwrap_or("").trim();
                if name.starts_with('X')
                    && name.len() > 1
                    && name[1..].chars().all(|c| c.is_ascii_digit())
                {
                    out.insert(name.to_string());
                }
            }
            start = at + 1 + bracket_off + close + 1;
        } else {
            break;
        }
    }
    out
}

fn assert_no_free_variables(formula: &str) {
    let used       = xvars_used(formula);
    let quantified = xvars_quantified(formula);
    let free: Vec<_> = used.difference(&quantified).cloned().collect();
    assert!(
        free.is_empty(),
        "formula has free variables {:?} — Vampire would reject:\n\
         used = {:?}\nquantified = {:?}\nformula = {}",
        free, used, quantified, formula
    );
}

/// Variables declared by the chain of outermost `![...]` quantifiers.
/// The Rust converter emits one variable per quantifier and chains them
/// with `:`, e.g. `![X0] : ![X1] : ![X2] : (body)`.  This walks the chain
/// and returns every variable name in textual order.  Stops at the first
/// non-`!` (typically `(`, the body, or an inner `?[…]` existential).
fn top_level_universal_vars(formula: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut s = match formula.find('!') {
        Some(i) => &formula[i..],
        None => return out,
    };
    loop {
        // Expect `!` then an optional space then `[`.
        if !s.starts_with('!') { break; }
        let after_bang = s[1..].trim_start();
        if !after_bang.starts_with('[') { break; }
        let bracket_body = &after_bang[1..];
        let close = match bracket_body.find(']') {
            Some(i) => i,
            None => break,
        };
        for v in bracket_body[..close].split(',') {
            let name = v.split(':').next().unwrap_or("").trim();
            if !name.is_empty() { out.push(name.to_string()); }
        }
        // Walk past `]`, then the `:`, then optional whitespace.
        let after_close = bracket_body[close + 1..].trim_start();
        if !after_close.starts_with(':') { break; }
        s = after_close[1..].trim_start();
    }
    out
}

fn fof_lines(tptp: &str) -> impl Iterator<Item = &str> {
    tptp.lines().filter(|l| l.trim_start().starts_with("fof("))
}

/// Convenience: emit FOF, return the single fof(...) line.
fn single_fof_line(kb: &KnowledgeBase) -> String {
    let tptp = emit_fof_hide_numbers(kb);
    let lines: Vec<&str> = fof_lines(&tptp).collect();
    assert_eq!(
        lines.len(), 1,
        "expected exactly 1 fof(...) line, got {}:\n{}", lines.len(), tptp
    );
    lines[0].to_string()
}

// ---------------------------------------------------------------------------
// ported tests
// ---------------------------------------------------------------------------

/// Java `string1`: `(=> (instance ?X P)(instance ?X Q))`
///
/// Java expected:
///   `( ( ! [V__X] : ((s__instance(V__X,s__P) => s__instance(V__X,s__Q)) ) ) )`
///
/// Rust holds-reified output:
///   `fof(kb_N, axiom, ![X0] : (s__holds(s__instance__m,X0,s__P) =>
///                              s__holds(s__instance__m,X0,s__Q))).`
#[test]
fn string1() {
    let kif = "(=> (instance ?X P)(instance ?X Q))";
    let kb  = load_kif(kif);
    let line = single_fof_line(&kb);

    assert!(line.contains("s__holds(s__instance__m,"),
        "missing holds-reified s__instance__m: {}", line);
    assert!(line.contains("s__P"), "missing s__P: {}", line);
    assert!(line.contains("s__Q"), "missing s__Q: {}", line);
    assert!(line.contains(" => "), "missing implication: {}", line);

    let top = top_level_universal_vars(&line);
    assert_eq!(top.len(), 1,
        "expected 1 top-level universal, got {:?} in {}", top, line);

    // Both literals must mention the same single variable (only `?X`).
    let used = xvars_used(&line);
    assert_eq!(used.len(), 1,
        "expected exactly one variable used, got {:?} in {}", used, line);

    assert_no_free_variables(&line);
}

/// Java `string2`: `(=> (or (instance ?X Q) (instance ?X R)) (instance ?X ?T))`
///
/// Java expected lists `[V__T,V__X]` — two top-level universals.
#[test]
fn string2() {
    let kif = "(=> (or (instance ?X Q) (instance ?X R)) (instance ?X ?T))";
    let kb  = load_kif(kif);
    let line = single_fof_line(&kb);

    assert!(line.contains("s__holds(s__instance__m,"),
        "missing holds-reified s__instance__m: {}", line);
    assert!(line.contains("s__Q"), "missing s__Q: {}", line);
    assert!(line.contains("s__R"), "missing s__R: {}", line);
    assert!(line.contains(" | "), "missing disjunction: {}", line);
    assert!(line.contains(" => "), "missing implication: {}", line);

    let top = top_level_universal_vars(&line);
    assert_eq!(top.len(), 2,
        "expected 2 top-level universals (?T and ?X), got {:?} in {}", top, line);

    assert_no_free_variables(&line);
}

/// Java `string3`: `(or (not (instance ?X Q)) (instance ?X R))`
///
/// Java expected: `(~(s__instance(V__X,s__Q)) | s__instance(V__X,s__R))`
#[test]
fn string3() {
    let kif = "(or (not (instance ?X Q)) (instance ?X R))";
    let kb  = load_kif(kif);
    let line = single_fof_line(&kb);

    assert!(line.contains("~"),    "missing negation: {}", line);
    assert!(line.contains(" | "),  "missing disjunction: {}", line);
    assert!(line.contains("s__holds(s__instance__m,"),
        "missing holds-reified s__instance__m: {}", line);

    let top = top_level_universal_vars(&line);
    assert_eq!(top.len(), 1, "expected 1 top-level universal, got {:?}", top);

    assert_no_free_variables(&line);
}

/// Java `string4`: biconditional with `0` literal.
/// `setHideNumbers(true)` → `0` encodes as `n__0`.
#[test]
fn string4() {
    let kif = "(<=>\n\
               (instance ?NUMBER NegativeRealNumber)\n\
               (and\n\
                   (lessThan ?NUMBER 0)\n\
                   (instance ?NUMBER RealNumber)))";
    let kb  = load_kif(kif);
    let line = single_fof_line(&kb);

    assert!(line.contains("s__NegativeRealNumber"),
        "missing class symbol s__NegativeRealNumber: {}", line);
    assert!(line.contains("s__RealNumber"),
        "missing class symbol s__RealNumber: {}", line);
    assert!(line.contains("s__lessThan__m"),
        "missing holds-reified s__lessThan__m: {}", line);
    assert!(line.contains("n__0"),
        "missing hidden-number encoding n__0: {}", line);
    assert!(line.contains(" & "),
        "missing conjunction: {}", line);
    // The Rust IR renders biconditionals as ` <=> ` directly.
    assert!(line.contains(" <=> "),
        "expected biconditional ` <=> `: {}", line);

    let top = top_level_universal_vars(&line);
    assert_eq!(top.len(), 1, "expected 1 top-level universal, got {:?}", top);

    assert_no_free_variables(&line);
}

/// Java `string5`: same as string4 with `0.001` instead of `0`.
/// The `.` in the literal must be encoded as `_` so the resulting
/// identifier is TPTP-safe (`n__0_001`).
#[test]
fn string5() {
    let kif = "(<=>\n\
               (instance ?NUMBER NegativeRealNumber)\n\
               (and\n\
                   (lessThan ?NUMBER 0.001)\n\
                   (instance ?NUMBER RealNumber)))";
    let kb  = load_kif(kif);
    let line = single_fof_line(&kb);

    assert!(line.contains("n__0_001"),
        "expected hidden-number n__0_001 (dot → underscore): {}", line);
    // The bare numeric literal must NOT leak through.
    assert!(!line.contains("0.001"),
        "bare 0.001 leaked into output: {}", line);
    // And no half-mangled forms.
    assert!(!line.contains("n__0_dot_001"),
        "unexpected encoding of dot: {}", line);

    let top = top_level_universal_vars(&line);
    assert_eq!(top.len(), 1, "expected 1 top-level universal, got {:?}", top);

    assert_no_free_variables(&line);
}

/// Java `string6`: function applied inside biconditional.
/// `(<=> (temporalPart ?POS (WhenFn ?THING)) (time ?THING ?POS))`
/// `WhenFn` appears as a function term — Rust reifies via
/// `s__holds_app_2(s__WhenFn__m, …)`.
#[test]
fn string6() {
    let kif = "(<=> (temporalPart ?POS (WhenFn ?THING)) (time ?THING ?POS))";
    let kb  = load_kif(kif);
    let line = single_fof_line(&kb);

    assert!(line.contains("s__temporalPart__m"),
        "missing holds-reified s__temporalPart__m: {}", line);
    assert!(line.contains("s__WhenFn__m"),
        "missing function-as-term s__WhenFn__m: {}", line);
    assert!(line.contains("s__time__m"),
        "missing holds-reified s__time__m: {}", line);
    assert!(line.contains(" <=> "),
        "expected biconditional ` <=> `: {}", line);

    let top = top_level_universal_vars(&line);
    assert_eq!(top.len(), 2, "expected 2 top-level universals, got {:?}", top);

    assert_no_free_variables(&line);
}

/// Java `hol`: deeply-nested formula with KappaFn and a negated
/// existential.  Java expected list: `[V__GUN,V__KILLING,V__LM,V__LM1,V__O]`
/// (5 outer universals).  The negated `(exists (?O2) ...)` becomes an
/// inner `~?[X<n>] : ...` (genuine quantifier, not reified).
///
/// Note (Java vs. Rust): Java emits `s__instrument__m` for `instrument`
/// in argument position (it knows `instrument` is a relation symbol).
/// Rust without a loaded SUMO signature emits the bare `s__instrument`,
/// which we assert here.
#[test]
fn hol() {
    let kif = "(=> (and (instance ?GUN Gun) (effectiveRange ?GUN ?LM) \
               (distance ?GUN ?O ?LM1) (instance ?O Organism) (not (exists (?O2) \
               (between ?O ?O2 ?GUN))) (lessThanOrEqualTo ?LM1 ?LM)) \
               (capability (KappaFn ?KILLING (and (instance ?KILLING Killing) \
               (patient ?KILLING ?O))) instrument ?GUN))";
    let kb  = load_kif(kif);
    let line = single_fof_line(&kb);

    // Function-as-term: KappaFn reified through holds_app.
    assert!(line.contains("s__KappaFn__m"),
        "missing function-as-term s__KappaFn__m: {}", line);
    // The capability head predicate.
    assert!(line.contains("s__capability__m"),
        "missing holds-reified s__capability__m: {}", line);
    // Negation + inner existential for `(not (exists (?O2) ...))`.
    assert!(line.contains("~"), "missing negation: {}", line);
    let has_inner_exists = line.contains("?[") || line.contains("? [");
    assert!(has_inner_exists,
        "expected inner existential `?[X<n>]` for `(exists (?O2) ...)`: {}", line);

    let top = top_level_universal_vars(&line);
    assert_eq!(
        top.len(), 5,
        "expected 5 top-level universals (matching Java [GUN,KILLING,LM,LM1,O]); got {:?} in {}",
        top, line,
    );

    assert_no_free_variables(&line);
}

/// Java `string7`: biconditional with an existential subformula.
/// `(<=> (exists (?BUILD) ...) (instance ?ARTIFACT StationaryArtifact))`
/// `?BUILD` must compile to a real inner `?[X<n>]` (not be reified
/// through `s__exists_op`).  Only `?ARTIFACT` is universally bound at
/// the top.
#[test]
fn string7() {
    let kif = "(<=> (exists (?BUILD) (and (instance ?BUILD Constructing) \
               (result ?BUILD ?ARTIFACT))) (instance ?ARTIFACT StationaryArtifact))";
    let kb  = load_kif(kif);
    let line = single_fof_line(&kb);

    assert!(line.contains("s__Constructing"),
        "missing class symbol s__Constructing: {}", line);
    assert!(line.contains("s__StationaryArtifact"),
        "missing class symbol s__StationaryArtifact: {}", line);
    assert!(line.contains("s__result__m"),
        "missing holds-reified s__result__m: {}", line);

    // Inner existential must be a real quantifier, not reified.
    let has_inner_exists = line.contains("?[") || line.contains("? [");
    assert!(has_inner_exists,
        "expected inner existential `?[X<n>]` for `(exists (?BUILD) …)`: {}", line);
    assert!(!line.contains("s__exists_op"),
        "?BUILD must compile to a genuine ? quantifier, not be reified: {}", line);

    let top = top_level_universal_vars(&line);
    assert_eq!(top.len(), 1,
        "expected only ?ARTIFACT bound at top, got {:?} in {}", top, line);

    assert_no_free_variables(&line);
}

/// Java `embedded`: `(instance equal BinaryPredicate)` — predicate name
/// `equal` used as a *term* (an argument).
///
/// The Rust KIF parser reserves `equal` as a head-only operator, so we
/// substitute the non-reserved predicate name `member`, which exercises
/// the same property: a SUMO predicate symbol appearing in argument
/// position.  Java emits the `__m` mention form for such a symbol;
/// Rust without typing signatures emits the bare `s__member`.  The
/// holds-reification still produces `s__instance__m` for the head
/// predicate, which is the property this test ultimately guards.
#[test]
fn embedded() {
    let kif = "(instance member BinaryPredicate)";
    let kb  = load_kif(kif);
    let line = single_fof_line(&kb);

    // Ground formula → no quantifiers.
    assert!(!line.contains("![") && !line.contains("! ["),
        "no top-level universal expected for ground formula: {}", line);
    assert!(!line.contains("?[") && !line.contains("? ["),
        "no existential expected for ground formula: {}", line);

    // Holds-reified head predicate uses mention form.
    assert!(line.contains("s__holds(s__instance__m,"),
        "expected holds-reified s__instance__m: {}", line);
    assert!(line.contains("s__BinaryPredicate"),
        "missing s__BinaryPredicate: {}", line);
    // `member` in argument position is bare in Rust (no `__m`).
    assert!(line.contains("s__member"),
        "missing s__member arg: {}", line);
}

/// Java `equality`: tests TPTP equality emission and predicate-as-term.
///
/// Input:
///   `(=> (and (minValue minValue ?ARG ?N) (minValue ?ARGS2)
///             (equal ?VAL (ListOrderFn (List__Fn__1Fn ?ARGS2) ?ARG)))
///        (greaterThan ?VAL ?N))`
///
/// Java expected list: 4 vars `[V__ARG,V__ARGS2,V__N,V__VAL]`.
/// Java emits `s__minValue(s__minValue__m,…)` for the second `minValue`.
/// Rust without typing signatures emits a bare `s__minValue` for the
/// second one (predicate-as-argument is bare); the holds-reified head
/// is `s__minValue__m`.  TPTP equality is rendered as ` = `.
#[test]
fn equality() {
    let kif = "(=> (and (minValue minValue ?ARG ?N) (minValue ?ARGS2) \
               (equal ?VAL (ListOrderFn (List__Fn__1Fn ?ARGS2) ?ARG))) \
               (greaterThan ?VAL ?N))";
    let kb  = load_kif(kif);
    let line = single_fof_line(&kb);

    // Head-position `minValue` reified via holds.
    assert!(line.contains("s__holds(s__minValue__m,"),
        "missing holds-reified head s__minValue__m: {}", line);
    // Argument-position `minValue` appears bare (Rust convention).
    assert!(line.contains(",s__minValue,") || line.contains(",s__minValue)"),
        "expected bare s__minValue argument: {}", line);

    // TPTP equality ` = ` for the (equal ?VAL …) literal.
    assert!(line.contains(" = "),
        "expected TPTP equality ` = ` for (equal …): {}", line);

    // Functions as terms reify through s__holds_app_<arity>.
    assert!(line.contains("s__holds_app_") && line.contains("s__ListOrderFn__m"),
        "missing function-as-term s__ListOrderFn__m via s__holds_app_*: {}", line);
    assert!(line.contains("s__List__Fn__1Fn__m"),
        "missing nested function-as-term s__List__Fn__1Fn__m: {}", line);

    // Final consequent.
    assert!(line.contains("s__greaterThan__m"),
        "missing holds-reified s__greaterThan__m: {}", line);

    let top = top_level_universal_vars(&line);
    assert_eq!(top.len(), 4,
        "expected 4 top-level universals (ARG, ARGS2, N, VAL); got {:?} in {}", top, line);

    assert_no_free_variables(&line);
}

/// Java `testGenerateQList`: validates the free-variable list extraction.
/// Java's helper returns a comma-separated string; the Rust equivalent
/// (`wrap_free_vars`) is materialised as the chain of outermost `![…]`
/// quantifiers.  We assert cardinality on both inputs Java tests.
#[test]
fn test_generate_q_list() {
    // Input 1: Java asserts Qlist == "V__NUMBER" (1 element).
    let kif1 = "(<=> (instance ?NUMBER NegativeRealNumber) \
                (and (lessThan ?NUMBER 0) (instance ?NUMBER RealNumber)))";
    let kb1  = load_kif(kif1);
    let line1 = single_fof_line(&kb1);
    let top1 = top_level_universal_vars(&line1);
    assert_eq!(
        top1.len(), 1,
        "Java asserts Qlist=\"V__NUMBER\" (1 var); Rust top-level universals = {:?} in {}",
        top1, line1,
    );

    // Input 2: Java asserts Qlist == "V__GUN,V__KILLING,V__LM,V__LM1,V__O"
    // (5 elements).  Same input as the `hol` test.
    let kif2 = "(=> (and (instance ?GUN Gun) (effectiveRange ?GUN ?LM) \
                (distance ?GUN ?O ?LM1) (instance ?O Organism) (not (exists (?O2) \
                (between ?O ?O2 ?GUN))) (lessThanOrEqualTo ?LM1 ?LM)) \
                (capability (KappaFn ?KILLING (and (instance ?KILLING Killing) \
                (patient ?KILLING ?O))) instrument ?GUN))";
    let kb2  = load_kif(kif2);
    let line2 = single_fof_line(&kb2);
    let top2 = top_level_universal_vars(&line2);
    assert_eq!(
        top2.len(), 5,
        "Java asserts Qlist with 5 vars; Rust top-level universals = {:?} in {}",
        top2, line2,
    );
}

/// Phase 1.1: predicate-as-argument mention form (TODO.md FOF §7).
///
/// When a SUMO predicate name appears in *argument* position **and** the
/// taxonomy chain to `Predicate` is loaded, the converter should emit
/// it in mention form (`s__name__m`) — matching Java's behavior with
/// signatures available.  Without the taxonomy chain, `is_predicate`
/// returns `false` and the converter falls back to bare `s__name` (the
/// `equality` and `hol` tests above cover that fallback path).
///
/// Verified semantics:
/// - `member` is `(instance member BinaryPredicate)` and
///   `BinaryPredicate` is a subclass of `Predicate` → `is_predicate(member) == true`
/// - In `(instance member SetOrClass)`, the second `member` is in
///   argument position → emitted as `s__member__m`.
/// - Class symbols (`BinaryPredicate`, `SetOrClass`) are *not* themselves
///   predicates, so they stay bare.
#[test]
fn predicate_as_argument_uses_mention_form_when_classified() {
    let kif = "(subclass BinaryPredicate Predicate)\n\
               (instance member BinaryPredicate)\n\
               (instance member SetOrClass)";
    let kb  = load_kif(kif);
    let tff = emit_fof_hide_numbers(&kb);

    // Find the line that has `member` in argument position — i.e. the
    // `(instance member SetOrClass)` axiom.
    let target = fof_lines(&tff)
        .find(|l| l.contains("s__SetOrClass"))
        .expect("no axiom contains s__SetOrClass");

    assert!(target.contains("s__member__m"),
        "expected predicate `member` in arg position to use mention form: {}", target);
    assert!(!target.contains(",s__member,") && !target.contains(",s__member)"),
        "did not expect bare s__member when taxonomy classifies it as a Predicate: {}", target);
    // Class names (BinaryPredicate, SetOrClass) are not predicates and
    // must stay in bare form.
    assert!(target.contains("s__SetOrClass"),
        "class symbols stay bare: {}", target);
    assert!(!target.contains("s__SetOrClass__m"),
        "class symbol must not be reified: {}", target);
}
