use std::collections::HashSet;

use crate::kif_store::{load_kif, KifStore};
use crate::semantic::SemanticLayer;
use crate::types::{Element, SentenceId};
use super::options::{TptpLang, TptpOptions};
use super::tff::{TffContext, translate_sort, infer_var_types};
use super::translate::{sentence_to_tptp, kb_to_tptp};

fn layer_from(kif: &str) -> SemanticLayer {
    let mut store = KifStore::default();
    load_kif(&mut store, kif, "test");
    SemanticLayer::new(store)
}

fn opts() -> TptpOptions {
    TptpOptions { hide_numbers: true, ..TptpOptions::default() }
}

#[test]
fn simple_predicate() {
    let layer = layer_from("(subclass Human Animal)");
    let sid = layer.store.roots[0];
    let tptp = sentence_to_tptp(sid, &layer, &opts());
    assert!(tptp.contains("s__holds("),   "got: {}", tptp);
    assert!(tptp.contains("s__subclass"), "got: {}", tptp);
    assert!(tptp.contains("s__Human"),    "got: {}", tptp);
    assert!(tptp.contains("s__Animal"),   "got: {}", tptp);
}

#[test]
fn free_variable_wrapper() {
    let layer = layer_from("(instance ?X Human)");
    let sid = layer.store.roots[0];
    let tptp = sentence_to_tptp(sid, &layer, &opts());
    assert!(tptp.contains("! [V__X@"), "got: {}", tptp);
}

#[test]
fn query_mode_existential() {
    let layer = layer_from("(instance ?X Human)");
    let sid = layer.store.roots[0];
    let q_opts = TptpOptions { query: true, hide_numbers: true, ..TptpOptions::default() };
    let tptp = sentence_to_tptp(sid, &layer, &q_opts);
    assert!(tptp.contains("? [V__X@"), "got: {}", tptp);
}

#[test]
fn empty_quantifier() {
    let layer = layer_from("(exists () (subclass Human Animal))");
    assert!(!layer.store.roots.is_empty());
    let sid = layer.store.roots[0];
    let tptp = sentence_to_tptp(sid, &layer, &opts());
    assert!(!tptp.contains("? []"),    "should not contain empty quantifier: {}", tptp);
    assert!( tptp.contains("s__holds("), "should contain body: {}", tptp);
}

#[test]
fn implication() {
    let layer = layer_from("(=> (instance ?X Human) (instance ?X Animal))");
    let sid = layer.store.roots[0];
    let tptp = sentence_to_tptp(sid, &layer, &opts());
    assert!(tptp.contains("=>"), "got: {}", tptp);
}

#[test]
fn mention_suffix_lowercase() {
    let layer = layer_from("(instance subclass BinaryRelation)");
    let sid = layer.store.roots[0];
    let tptp = sentence_to_tptp(sid, &layer, &opts());
    assert!(tptp.contains("s__subclass__m"), "got: {}", tptp);
}

#[test]
fn nested_predicate_as_term() {
    let layer = layer_from("(holdsDuring ?I (attribute ?X LegalPersonhood))");
    let sid = layer.store.roots[0];
    let tptp = sentence_to_tptp(sid, &layer, &opts());
    assert!(tptp.contains("s__holds(s__holdsDuring__m,"), "got: {}", tptp);
    assert!(tptp.contains("s__attribute(V__X,s__LegalPersonhood)"), "got: {}", tptp);
}

#[test]
fn nested_logical_operator() {
    let layer = layer_from("(holdsDuring ?I (and (attribute ?X LegalPersonhood) (instance ?X Human)))");
    let sid = layer.store.roots[0];
    let tptp = sentence_to_tptp(sid, &layer, &opts());
    assert!(tptp.contains("s__holds(s__holdsDuring__m,"), "got: {}", tptp);
    assert!(tptp.contains("s__and("), "missing s__and in: {}", tptp);
    assert!(!tptp.contains("&"), "found & inside term: {}", tptp);
}

#[test]
fn bare_variable_as_formula() {
    let layer = layer_from("(=> (instance ?P Proposition) ?P)");
    let sid = layer.store.roots[0];
    let tptp = sentence_to_tptp(sid, &layer, &opts());
    assert!(tptp.contains("=> s__holds(V__P))"), "got: {}", tptp);
}

#[test]
fn number_hidden_by_default() {
    let layer = layer_from("(lessThan ?X 42)");
    let sid = layer.store.roots[0];
    let tptp = sentence_to_tptp(sid, &layer, &opts());
    assert!(tptp.contains("n__42"), "got: {}", tptp);
}

// ── Tests analogous to SUMOformulaToTPTPformulaTest.java ─────────────────────
//
// Java uses direct predicate calls for FOF (s__instance(V__X, s__P));
// our implementation uses holds-encoding (s__holds(s__instance__m, V__X, s__P)).
// Both produce the same logical formula — holds() is the standard FOF
// encoding of higher-order predicates.  Tests below verify the structural
// behaviour (quantifier wrapping, operator translation, number mangling,
// function-in-term-position, mention suffixes) in our encoding.
// Java also wraps in an extra pair of parens ( ( ! [...] : (...) ) );
// our output uses a single wrapper ( ! [...] : (...) ).

/// string1 analog — free ?X universally quantified; implication present.
/// Java: (=> (instance ?X P) (instance ?X Q))
///       → ( ! [V__X] : ((s__instance(V__X,s__P) => s__instance(V__X,s__Q)) ) )
#[test]
fn fof_implication_free_var_universally_quantified() {
    let layer = layer_from("(=> (instance ?X P) (instance ?X Q))");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &opts());
    assert!(tptp.contains("! [V__X@"), "expected universal quantifier: {}", tptp);
    assert!(tptp.contains("=>"),       "expected implication: {}", tptp);
    assert!(tptp.contains("s__holds("), "FOF should use holds encoding: {}", tptp);
    assert!(tptp.contains("s__P"),     "expected s__P: {}", tptp);
    assert!(tptp.contains("s__Q"),     "expected s__Q: {}", tptp);
}

/// string2 analog — multiple free variables are all quantified and sorted.
/// Java: (=> (or (instance ?X Q) (instance ?X R)) (instance ?X ?T))
///       → ( ! [V__T,V__X] : ... )  — T before X alphabetically
#[test]
fn fof_multiple_free_vars_sorted() {
    let layer = layer_from("(=> (or (instance ?X Q) (instance ?X R)) (instance ?X ?T))");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &opts());
    // Both variables should appear in the quantifier list
    assert!(tptp.contains("V__T@"), "expected V__T in quantifier: {}", tptp);
    assert!(tptp.contains("V__X@"), "expected V__X in quantifier: {}", tptp);
    // T should appear before X (sorted; V__T < V__X lexicographically)
    let pos_t = tptp.find("V__T@").unwrap();
    let pos_x = tptp.find("V__X@").unwrap();
    assert!(pos_t < pos_x, "V__T should precede V__X in sorted list: {}", tptp);
    assert!(tptp.contains("|"),  "expected disjunction: {}", tptp);
    assert!(tptp.contains("=>"), "expected implication: {}", tptp);
}

/// string3 analog — not → ~, or → |.
/// Java: (or (not (instance ?X Q)) (instance ?X R))
///       → ( ! [V__X] : ((~(s__instance(V__X,s__Q)) | s__instance(V__X,s__R)) ) )
#[test]
fn fof_negation_in_disjunction() {
    let layer = layer_from("(or (not (instance ?X Q)) (instance ?X R))");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &opts());
    assert!(tptp.contains("~("),       "expected negation: {}", tptp);
    assert!(tptp.contains(" | "),      "expected disjunction: {}", tptp);
    assert!(tptp.contains("! [V__X@"), "expected universal quantifier: {}", tptp);
}

/// string4 analog — biconditional expands to paired implications; integer
/// literal 0 hidden as n__0 when hide_numbers is set.
/// Java: (<=> (instance ?N NegativeRealNumber) (and (lessThan ?N 0) ...))
///       → (... => ...) & (... => ...)  with n__0
#[test]
fn fof_biconditional_expands_with_hidden_integer() {
    let layer = layer_from(
        "(<=> (instance ?N NegativeRealNumber) \
              (and (lessThan ?N 0) (instance ?N RealNumber)))"
    );
    let sid  = layer.store.roots[0];
    let tptp = sentence_to_tptp(sid, &layer, &opts()); // hide_numbers: true
    // Both directions of the biconditional
    assert!(tptp.matches("=>").count() >= 2, "biconditional needs two implications: {}", tptp);
    assert!(tptp.contains(" & "),   "biconditional needs conjunction: {}", tptp);
    // Integer 0 replaced with n__0
    assert!(tptp.contains("n__0"), "integer 0 should be hidden as n__0: {}", tptp);
    assert!(!tptp.contains(",0,") && !tptp.contains(",0)"),
        "raw 0 should not appear: {}", tptp);
}

/// string5 analog — decimal literal 0.001 hidden as n__0_001 (dot → _).
#[test]
fn fof_decimal_number_hidden_with_underscore() {
    let layer = layer_from(
        "(<=> (instance ?N NegativeRealNumber) \
              (and (lessThan ?N 0.001) (instance ?N RealNumber)))"
    );
    let sid  = layer.store.roots[0];
    let tptp = sentence_to_tptp(sid, &layer, &opts()); // hide_numbers: true
    assert!(tptp.contains("n__0_001"), "decimal 0.001 should be hidden as n__0_001: {}", tptp);
    assert!(!tptp.contains("0.001"),   "raw decimal should not appear: {}", tptp);
}

/// string6 analog — function application in term position renders as
/// s__WhenFn(V__THING) not as a holds call.
/// Java: (<=> (temporalPart ?POS (WhenFn ?THING)) (time ?THING ?POS))
///       → ... s__temporalPart(V__POS, s__WhenFn(V__THING)) ...
#[test]
fn fof_function_as_term_in_argument_position() {
    let layer = layer_from("(<=> (temporalPart ?POS (WhenFn ?THING)) (time ?THING ?POS))");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &opts());
    // WhenFn appears in term position: rendered as a direct function call
    assert!(tptp.contains("s__WhenFn("), "WhenFn should be a direct function call: {}", tptp);
    // Both free variables present
    assert!(tptp.contains("V__POS@"),   "expected V__POS: {}", tptp);
    assert!(tptp.contains("V__THING@"), "expected V__THING: {}", tptp);
    // Biconditional structure
    assert!(tptp.matches("=>").count() >= 2, "biconditional needs two implications: {}", tptp);
}

/// string7 analog — biconditional where one side is an existential.
/// The existentially bound variable stays bound; the other free variable
/// gets a universal quantifier at the top.
/// Java: (<=> (exists (?BUILD) (and ...)) (instance ?ARTIFACT StationaryArtifact))
///       → ( ! [V__ARTIFACT] : ((( ? [V__BUILD] : ...) => ...) & (... => ( ? [V__BUILD] : ...))))
#[test]
fn fof_biconditional_with_existential() {
    let layer = layer_from(
        "(<=> (exists (?BUILD) \
                    (and (instance ?BUILD Constructing) (result ?BUILD ?ARTIFACT))) \
              (instance ?ARTIFACT StationaryArtifact))"
    );
    let sid  = layer.store.roots[0];
    let tptp = sentence_to_tptp(sid, &layer, &opts());
    // ARTIFACT is free → universally quantified at top
    assert!(tptp.contains("! [V__ARTIFACT@"), "expected outer universal: {}", tptp);
    // BUILD is bound by exists
    assert!(tptp.contains("? [V__BUILD@"),    "expected inner existential: {}", tptp);
    // Both directions of biconditional
    assert!(tptp.matches("=>").count() >= 2,   "biconditional needs two implications: {}", tptp);
}

/// embedded analog — predicate used as a term (argument, not head) gets __m suffix.
/// Java: (instance equal BinaryPredicate) → s__instance(s__equal__m, s__BinaryPredicate)
/// Our encoding: s__holds(s__instance__m, s__equal__m, s__BinaryPredicate)
#[test]
fn fof_predicate_as_term_gets_mention_suffix() {
    // "instance" is the head (no __m); "subclass" appears in argument position
    // (as the second arg to domain), which is a term context → __m suffix
    let layer = layer_from("(domain subclass 1 Class)");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &opts());
    // "subclass" starts lowercase, so it gets __m when used as a term
    assert!(tptp.contains("s__subclass__m"), "subclass as term should have __m: {}", tptp);
    assert!(tptp.contains("s__Class"),       "expected class name: {}", tptp);
}

/// equality analog — (equal A B) → (A = B) in both FOF and TFF.
/// Java: (equal ?VAL (ListOrderFn ...)) → (V__VAL = s__ListOrderFn(...))
#[test]
fn fof_equal_operator_produces_infix_eq() {
    let layer = layer_from("(equal ?VAL (WhenFn ?T))");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &opts());
    // equal → infix =, not s__holds
    assert!(tptp.contains(" = "),       "equal should produce infix =: {}", tptp);
    assert!(tptp.contains("s__WhenFn("), "RHS function should be a term call: {}", tptp);
    assert!(!tptp.contains("s__equal"), "equal operator should not appear as s__equal: {}", tptp);
}

/// hol analog — negated existential: (not (exists (?O2) (...))) → ~(( ? [...] : (...)))
/// Java hol test also checks that the entire complex formula is wrapped once.
#[test]
fn fof_negated_existential() {
    let layer = layer_from(
        "(not (exists (?O2) (between ?O ?O2 ?GUN)))"
    );
    let sid  = layer.store.roots[0];
    let tptp = sentence_to_tptp(sid, &layer, &opts());
    // Free variables O and GUN universally quantified
    assert!(tptp.contains("! ["),     "free vars should be universally quantified: {}", tptp);
    // Negation wraps the existential
    assert!(tptp.contains("~("),      "expected negation: {}", tptp);
    assert!(tptp.contains("? [V__O2@"), "expected bound existential variable: {}", tptp);
}

/// hol analog — deeply nested formula with conjunction and existential inside negation.
#[test]
fn fof_nested_conjunction_in_negated_existential() {
    let layer = layer_from(
        "(=> (and (instance ?GUN Gun) \
                  (not (exists (?O2) (between ?O ?O2 ?GUN)))) \
             (instance ?GUN Weapon))"
    );
    let sid  = layer.store.roots[0];
    let tptp = sentence_to_tptp(sid, &layer, &opts());
    assert!(tptp.contains("! ["),    "free vars should be quantified: {}", tptp);
    assert!(tptp.contains(" & "),    "expected conjunction: {}", tptp);
    assert!(tptp.contains("~("),     "expected negation: {}", tptp);
    assert!(tptp.contains("? [V__O2@"), "expected bound existential: {}", tptp);
    assert!(tptp.contains("=>"),     "expected outer implication: {}", tptp);
}

/// Conjunction in formula position — & connects translated sub-formulas.
#[test]
fn fof_conjunction_of_predicates() {
    let layer = layer_from("(and (instance ?X Foo) (instance ?Y Bar))");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &opts());
    assert!(tptp.contains(" & "),      "expected conjunction: {}", tptp);
    assert!(tptp.contains("s__holds("), "FOF should use holds encoding: {}", tptp);
    assert!(tptp.contains("V__X@"),    "expected V__X: {}", tptp);
    assert!(tptp.contains("V__Y@"),    "expected V__Y: {}", tptp);
}

/// Forall with bound variable — bound variable not re-quantified at top level.
#[test]
fn fof_forall_bound_var_not_double_quantified() {
    let layer = layer_from("(forall (?X) (instance ?X Human))");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &opts());
    assert!(tptp.contains("! [V__X@"), "expected forall with V__X: {}", tptp);
    // The bound variable should appear exactly once in a [ ] quantifier list
    let q_list_count = tptp.matches("[V__X@").count()
        + tptp.matches("! [V__X@").count();
    // There should NOT be two separate universal quantifiers for the same var
    assert!(!tptp.contains("! [V__X@") || tptp.matches("! [").count() == 1,
        "bound var should not get an extra outer quantifier: {}", tptp);
}

/// String literal translated with single-quote wrapping.
#[test]
fn fof_string_literal_single_quoted() {
    let layer = layer_from("(documentation Foo EnglishLanguage \"A foo.\")");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &opts());
    assert!(tptp.contains("'A foo.'"), "string literal should be single-quoted: {}", tptp);
}

#[test]
fn kb_to_tptp_contains_axiom() {
    let layer = layer_from("(subclass Human Animal)");
    let axiom_ids: HashSet<SentenceId> = layer.store.roots.iter().copied().collect();
    let tptp = kb_to_tptp(&layer, "test", &opts(), &axiom_ids, &HashSet::new());
    assert!(tptp.contains(",axiom,"), "got: {}", tptp);
}

// ── translate_sort tests ──────────────────────────────────────────────────────

#[test]
fn translate_sort_exact_integer() {
    let layer = layer_from("(subclass Integer RationalNumber)");
    assert_eq!(translate_sort("Integer", &layer), "$int");
}

#[test]
fn translate_sort_exact_rational() {
    let layer = layer_from("(subclass RationalNumber RealNumber)");
    assert_eq!(translate_sort("RationalNumber", &layer), "$rat");
}

#[test]
fn translate_sort_exact_real() {
    let layer = layer_from("(subclass RealNumber Quantity)");
    assert_eq!(translate_sort("RealNumber", &layer), "$real");
}

#[test]
fn translate_sort_subtype_of_integer() {
    // NonnegativeInteger is a subclass of Integer — taxonomy walk should map to $int
    let layer = layer_from(
        "(subclass Integer RationalNumber)\n\
         (subclass NonnegativeInteger Integer)"
    );
    assert_eq!(translate_sort("NonnegativeInteger", &layer), "$int");
}

#[test]
fn translate_sort_non_numeric() {
    let layer = layer_from("(subclass Human Animal)");
    assert_eq!(translate_sort("Human", &layer), "$i");
    assert_eq!(translate_sort("Entity", &layer), "$i");
}

#[test]
fn translate_sort_unknown_type() {
    let layer = layer_from("(subclass Human Animal)");
    assert_eq!(translate_sort("NonExistentType", &layer), "$i");
    assert_eq!(translate_sort("", &layer), "$i");
}

#[test]
fn infer_var_types_instance_pattern() {
    // (instance ?X Integer) → ?X should get $int from the instance pattern
    let layer = layer_from(
        "(subclass Integer RationalNumber)\n\
         (instance ?X Integer)"
    );
    let opts  = TptpOptions::default();
    let mut tff = TffContext::new();
    let sid = *layer.store.roots.last().unwrap();
    let types = infer_var_types(sid, &layer.store, &layer, &mut tff, &opts);
    assert!(!types.is_empty(), "no variables found");
    for sort in types.values() {
        assert_eq!(*sort, "$int", "expected $int from instance pattern, got {}", sort);
    }
}

#[test]
fn infer_var_types_literal_cooccurrence() {
    // (lessThan ?X 5) — no domain info, but integer literal → $int
    let layer = layer_from("(lessThan ?X 5)");
    let opts  = TptpOptions::default();
    let mut tff = TffContext::new();
    let sid = layer.store.roots[0];
    let types = infer_var_types(sid, &layer.store, &layer, &mut tff, &opts);
    assert!(!types.is_empty(), "no variables found");
    for sort in types.values() {
        assert_eq!(*sort, "$int", "expected $int from integer literal, got {}", sort);
    }
}

#[test]
fn infer_var_types_default_i() {
    // (subclass ?X Animal) — no numeric info → $i
    let layer = layer_from("(subclass ?X Animal)");
    let opts  = TptpOptions::default();
    let mut tff = TffContext::new();
    let sid = layer.store.roots[0];
    let types = infer_var_types(sid, &layer.store, &layer, &mut tff, &opts);
    for sort in types.values() {
        assert_eq!(*sort, "$i", "expected $i default, got {}", sort);
    }
}

#[test]
fn infer_var_types_domain_sorts() {
    // lessThan with domain RealNumber: variables should get $real
    let layer = layer_from(
        "(instance lessThan BinaryRelation)\n\
         (subclass BinaryRelation Relation)\n\
         (subclass Relation Abstract)\n\
         (subclass Abstract Entity)\n\
         (domain lessThan 1 RealNumber)\n\
         (domain lessThan 2 RealNumber)\n\
         (lessThan ?X ?Y)"
    );
    let opts  = TptpOptions::default();
    let mut tff = TffContext::new();
    let sid = *layer.store.roots.last().unwrap();
    let types = infer_var_types(sid, &layer.store, &layer, &mut tff, &opts);
    assert!(!types.is_empty(), "no variables found");
    for sort in types.values() {
        assert_eq!(*sort, "$real", "expected $real from domain declaration, got {}", sort);
    }
}

#[test]
fn infer_var_types_instance_beats_literal() {
    // instance pattern ($int) should beat float literal heuristic ($real)
    let layer = layer_from(
        "(subclass Integer RationalNumber)\n\
         (instance ?X Integer)"
    );
    let opts  = TptpOptions::default();
    let mut tff = TffContext::new();
    let sid = *layer.store.roots.last().unwrap();
    let types = infer_var_types(sid, &layer.store, &layer, &mut tff, &opts);
    for sort in types.values() {
        assert_eq!(*sort, "$int", "instance pattern should dominate, got {}", sort);
    }
}

#[test]
fn tff_ensure_declared_constant() {
    let layer = layer_from("(subclass Human Animal)");
    let opts  = TptpOptions::default();
    let mut ctx = TffContext::new();
    let id = layer.store.sym_id("Human").unwrap();
    ctx.ensure_declared(&Element::Symbol(id), &layer, &opts);
    assert!(ctx.decl_lines.iter().any(|d| d.contains("s__Human") && d.contains(": $i")),
        "declarations: {:?}", ctx.decl_lines);
}

#[test]
fn tff_ensure_declared_relation_with_domain() {
    let layer = layer_from(
        "(instance instance BinaryRelation)\n\
         (subclass BinaryRelation Relation)\n\
         (subclass Relation Abstract)\n\
         (subclass Abstract Entity)\n\
         (domain instance 1 Entity)\n\
         (domain instance 2 Class)"
    );
    let opts  = TptpOptions::default();
    let mut ctx = TffContext::new();
    let id = layer.store.sym_id("instance").unwrap();
    ctx.ensure_declared(&Element::Symbol(id), &layer, &opts);
    assert!(ctx.decl_lines.iter().any(|d| d.contains("s__instance") && d.contains("> $o")),
        "declarations: {:?}", ctx.decl_lines);
    assert!(ctx.signatures.contains_key(&id));
}

#[test]
fn tff_ensure_declared_idempotent() {
    let layer = layer_from("(subclass Human Animal)");
    let opts  = TptpOptions::default();
    let mut ctx = TffContext::new();
    let id = layer.store.sym_id("Human").unwrap();
    ctx.ensure_declared(&Element::Symbol(id), &layer, &opts);
    ctx.ensure_declared(&Element::Symbol(id), &layer, &opts);
    let count = ctx.decl_lines.iter().filter(|d| d.contains("s__Human")).count();
    assert_eq!(count, 1, "expected exactly one declaration, got: {:?}", ctx.decl_lines);
}

#[test]
fn tff_ensure_declared_skips_excluded() {
    let layer = layer_from("(documentation Human EnglishLanguage \"A human.\")");
    let opts  = TptpOptions::default(); // documentation is in opts.excluded
    let mut ctx = TffContext::new();
    if let Some(id) = layer.store.sym_id("documentation") {
        ctx.ensure_declared(&Element::Symbol(id), &layer, &opts);
    }
    assert!(!ctx.decl_lines.iter().any(|d| d.contains("s__documentation")),
        "excluded predicate leaked: {:?}", ctx.decl_lines);
}

#[test]
fn tff_ensure_declared_skips_non_symbol() {
    let layer = layer_from("(subclass Human Animal)");
    let opts  = TptpOptions::default();
    let mut ctx = TffContext::new();
    // Variables, literals, etc. should produce no declarations
    ctx.ensure_declared(
        &Element::Variable { name: "X".to_string(), id: 0, is_row: false },
        &layer, &opts,
    );
    assert!(ctx.decl_lines.is_empty(), "non-symbol leaked: {:?}", ctx.decl_lines);
}

// ── TFF integration tests ────────────────────────────────────────────────────

fn tff_opts() -> TptpOptions {
    TptpOptions { lang: TptpLang::Tff, ..TptpOptions::default() }
}

#[test]
fn tff_no_holds_encoding() {
    let layer = layer_from("(instance ?X Human)");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &tff_opts());
    assert!(!tptp.contains("s__holds"), "should not contain holds: {}", tptp);
    assert!(tptp.contains("s__instance("), "should contain direct call: {}", tptp);
}

#[test]
fn fof_still_uses_holds() {
    let layer = layer_from("(instance ?X Human)");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &opts());
    assert!(tptp.contains("s__holds("), "got: {}", tptp);
}

#[test]
fn tff_numeric_literal_passthrough() {
    let layer = layer_from("(lessThan ?X 42)");
    let sid   = layer.store.roots[0];
    let opts  = TptpOptions { lang: TptpLang::Tff, hide_numbers: false, ..TptpOptions::default() };
    let tptp  = sentence_to_tptp(sid, &layer, &opts);
    assert!(tptp.contains("42"), "got: {}", tptp);
    assert!(!tptp.contains("n__42"), "should not mangle number: {}", tptp);
}

#[test]
fn tff_lessthan_builtin() {
    let layer = layer_from("(lessThan ?X ?Y)");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &tff_opts());
    assert!(tptp.contains("$less("), "got: {}", tptp);
    assert!(!tptp.contains("s__lessThan"), "should not emit SUMO name: {}", tptp);
}

#[test]
fn tff_addition_builtin() {
    let layer = layer_from("(equal ?Z (AdditionFn ?X ?Y))");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &tff_opts());
    assert!(tptp.contains("$sum("), "got: {}", tptp);
}

#[test]
fn tff_successor_builtin() {
    let layer = layer_from("(equal ?Y (SuccessorFn ?X))");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &tff_opts());
    assert!(tptp.contains("$sum(") && tptp.contains(",1)"), "got: {}", tptp);
}

#[test]
fn tff_typed_free_variables() {
    // (instance ?X Integer) — ?X should get $int annotation in quantifier
    let layer = layer_from(
        "(subclass Integer RationalNumber)\n\
         (instance ?X Integer)"
    );
    let sid  = *layer.store.roots.last().unwrap();
    let tptp = sentence_to_tptp(sid, &layer, &tff_opts());
    // Variable should be annotated with a sort
    assert!(tptp.contains(": $"), "expected typed variable in: {}", tptp);
}

#[test]
fn tff_kb_has_type_declarations() {
    let layer = layer_from("(subclass Human Animal)\n(instance ?X Human)");
    let axiom_ids: HashSet<SentenceId> = layer.store.roots.iter().copied().collect();
    let tptp = kb_to_tptp(&layer, "test", &tff_opts(), &axiom_ids, &HashSet::new());
    assert!(tptp.contains("tff(type_"), "should contain type declarations: {}", tptp);
    assert!(tptp.contains("tff(kb_"), "should contain axioms: {}", tptp);
    assert!(!tptp.contains("s__holds"), "TFF should not use holds encoding: {}", tptp);
}

#[test]
fn tff_kb_no_decls_for_builtins() {
    let layer = layer_from("(lessThan ?X ?Y)");
    let axiom_ids: HashSet<SentenceId> = layer.store.roots.iter().copied().collect();
    let tptp = kb_to_tptp(&layer, "test", &tff_opts(), &axiom_ids, &HashSet::new());
    // $less is a TFF builtin — no type declaration should be emitted for lessThan
    assert!(!tptp.contains("type_s__lessThan"), "builtin should not have type decl: {}", tptp);
    assert!(tptp.contains("$less("), "got: {}", tptp);
}

// ── Tests analogous to SUMOtoTFATest.java ────────────────────────────────────
//
// The Java tests require a fully preloaded SUMO KB (type-constraint preprocessing,
// domain/range data for every predicate).  These tests cover the same TFF behaviours
// using minimal layer_from() KIF instead.  Exact variable names (V__X@N) are not
// pinned — we assert on structural substrings that are stable regardless of the
// interning counter.

/// test1 analog — ground atom: no quantifier wrapper, direct predicate call.
/// Java: (instance Foo Bar) → s__instance(s__Foo, s__Bar)
#[test]
fn tff_ground_atom_no_holds_no_quantifier() {
    let layer = layer_from("(instance Foo Bar)");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &tff_opts());
    // No free variables → no quantifier wrapper
    assert!(!tptp.contains('!') && !tptp.contains('?'),
        "ground atom should have no quantifier: {}", tptp);
    assert!(tptp.contains("s__instance("), "got: {}", tptp);
    assert!(tptp.contains("s__Foo"),       "got: {}", tptp);
    assert!(tptp.contains("s__Bar"),       "got: {}", tptp);
    assert!(!tptp.contains("holds"),        "TFF must not use holds: {}", tptp);
}

/// test2 analog — forall with => and two direct predicates; variable typed $i.
/// Java: (forall (?X) (=> (instance ?X Human) (attribute ?X Mortal)))
///       → ( ! [V__X:$i] : (s__instance(V__X, s__Human) => s__attribute(V__X, s__Mortal)))
#[test]
fn tff_forall_implies_direct_calls() {
    let layer = layer_from("(forall (?X) (=> (instance ?X Human) (attribute ?X Mortal)))");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &tff_opts());
    assert!(tptp.contains("! ["),        "expected forall: {}", tptp);
    assert!(tptp.contains(": $i]"),      "variable should be typed $i: {}", tptp);
    assert!(tptp.contains("=>"),         "expected implication: {}", tptp);
    assert!(tptp.contains("s__instance("), "expected direct instance call: {}", tptp);
    assert!(tptp.contains("s__attribute("), "expected direct attribute call: {}", tptp);
    assert!(!tptp.contains("holds"),     "TFF must not use holds: {}", tptp);
}

/// test2 analog — negation wraps its argument in ~(...).
/// Java uses Not in test9: ~(($less(...) & $less(...)))
#[test]
fn tff_negation_of_conjunction() {
    let layer = layer_from("(not (and (instance ?X Foo) (instance ?X Bar)))");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &tff_opts());
    assert!(tptp.contains("~("),         "expected negation: {}", tptp);
    assert!(tptp.contains(" & "),        "expected conjunction inside: {}", tptp);
    assert!(tptp.contains("s__instance("), "expected direct calls: {}", tptp);
    assert!(!tptp.contains("holds"),     "TFF must not use holds: {}", tptp);
}

/// test4 analog — existential quantifier in TFF with typed variable.
/// Java: (exists (?R) ...) → ( ? [V__R:$i] : (...))
#[test]
fn tff_existential_quantifier_typed() {
    let layer = layer_from("(exists (?R) (instance ?R Electricity))");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &tff_opts());
    assert!(tptp.contains("? ["),        "expected exists: {}", tptp);
    assert!(tptp.contains(": $i]"),      "variable should be typed $i: {}", tptp);
    assert!(tptp.contains("s__instance("), "expected direct call: {}", tptp);
    assert!(!tptp.contains("holds"),     "TFF must not use holds: {}", tptp);
}

/// test9 analog — $less used for lessThan; SuccessorFn maps to $sum(x,1).
/// Java test9 shows: ~(($less(V__INT1,V__INT2) & $less(V__INT2, SuccessorFn(INT1))))
#[test]
fn tff_lessthan_and_successor_in_negation() {
    let layer = layer_from(
        "(not (and (lessThan ?INT1 ?INT2) (lessThan ?INT2 (SuccessorFn ?INT1))))"
    );
    let sid  = layer.store.roots[0];
    let tptp = sentence_to_tptp(sid, &layer, &tff_opts());
    assert!(tptp.contains("~("),    "expected negation: {}", tptp);
    assert!(tptp.contains("$less("), "lessThan should map to $less: {}", tptp);
    assert!(tptp.contains("$sum(") && tptp.contains(",1)"),
        "SuccessorFn should map to $sum(x,1): {}", tptp);
    assert!(!tptp.contains("s__lessThan"),   "should not emit SUMO name: {}", tptp);
    assert!(!tptp.contains("s__SuccessorFn"), "should not emit SUMO name: {}", tptp);
}

/// testMult analog — MultiplicationFn maps to $product in term position.
/// Java testMult: equal(SquareRootFn(?N1), ?N2) => $product(V__N2, V__N2) = V__N1
#[test]
fn tff_multiplication_in_equality() {
    let layer = layer_from("(equal ?Z (MultiplicationFn ?X ?Y))");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &tff_opts());
    assert!(tptp.contains("$product("), "MultiplicationFn should map to $product: {}", tptp);
    assert!(tptp.contains(" = "),       "equal should produce =: {}", tptp);
    assert!(!tptp.contains("s__MultiplicationFn"), "should not emit SUMO name: {}", tptp);
}

/// testCeiling / greaterThanOrEqualTo analog.
/// Java testCeiling: $greatereq(V__OTHERINT, V__NUMBER)
#[test]
fn tff_greaterthanorequalto_builtin() {
    let layer = layer_from("(greaterThanOrEqualTo ?X 0)");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &tff_opts());
    assert!(tptp.contains("$greatereq("), "greaterThanOrEqualTo should map to $greatereq: {}", tptp);
    assert!(!tptp.contains("s__greaterThanOrEqualTo"), "should not emit SUMO name: {}", tptp);
}

/// Subtraction builtin.
#[test]
fn tff_subtraction_builtin() {
    let layer = layer_from("(equal ?Z (SubtractionFn ?X ?Y))");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &tff_opts());
    assert!(tptp.contains("$difference("), "SubtractionFn should map to $difference: {}", tptp);
}

/// Division builtin.
#[test]
fn tff_division_builtin() {
    let layer = layer_from("(equal ?Z (DivisionFn ?X ?Y))");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &tff_opts());
    assert!(tptp.contains("$quotient_e("), "DivisionFn should map to $quotient_e: {}", tptp);
}

/// greaterThan builtin.
#[test]
fn tff_greaterthan_builtin() {
    let layer = layer_from("(greaterThan ?X ?Y)");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &tff_opts());
    assert!(tptp.contains("$greater("), "greaterThan should map to $greater: {}", tptp);
}

/// lessThanOrEqualTo builtin.
#[test]
fn tff_lessthanorequalto_builtin() {
    let layer = layer_from("(lessThanOrEqualTo ?NUMBER 31)");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &tff_opts());
    assert!(tptp.contains("$lesseq("), "lessThanOrEqualTo should map to $lesseq: {}", tptp);
}

/// PredecessorFn maps to $difference(x, 1).
#[test]
fn tff_predecessor_builtin() {
    let layer = layer_from("(equal ?Y (PredecessorFn ?X))");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &tff_opts());
    assert!(tptp.contains("$difference(") && tptp.contains(",1)"),
        "PredecessorFn should map to $difference(x,1): {}", tptp);
}

/// Implication at top level (free variables) → universal quantifier with $i sorts.
/// Java test2: (=> (instance ?X Human) (attribute ?X Mortal))
///             after preprocessing adds the forall wrapper.
#[test]
fn tff_implies_free_vars_typed_i() {
    let layer = layer_from("(=> (instance ?X Human) (attribute ?X Mortal))");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &tff_opts());
    // Free variable ?X should be universally quantified with $i
    assert!(tptp.contains("! ["),          "expected universal quantifier: {}", tptp);
    assert!(tptp.contains(": $i]"),        "V__X should be $i: {}", tptp);
    assert!(tptp.contains("=>"),           "expected implication: {}", tptp);
    assert!(tptp.contains("s__instance("), "expected direct instance call: {}", tptp);
    assert!(tptp.contains("s__attribute("), "expected direct attribute call: {}", tptp);
}

/// Integer variable type from instance pattern — free var wrapped with $int.
/// Java test9: (instance ?INT1 Integer) → V__INT1 : $int
#[test]
fn tff_integer_var_type_from_instance() {
    let layer = layer_from(
        "(subclass Integer RationalNumber)\n\
         (=> (instance ?N Integer) (lessThan ?N 100))"
    );
    let sid  = *layer.store.roots.last().unwrap();
    let tptp = sentence_to_tptp(sid, &layer, &tff_opts());
    assert!(tptp.contains(": $int"), "integer variable should be typed $int: {}", tptp);
    assert!(tptp.contains("$less("),  "lessThan should map to $less: {}", tptp);
}

/// Type declaration preamble format in kb_to_tptp.
/// Java writeSorts() emits: tff(type_s__Foo, type, s__Foo: $i).
#[test]
fn tff_type_declaration_format() {
    let layer = layer_from("(instance Foo Bar)");
    let axiom_ids: HashSet<SentenceId> = layer.store.roots.iter().copied().collect();
    let tptp = kb_to_tptp(&layer, "test", &tff_opts(), &axiom_ids, &HashSet::new());
    // Declarations should follow the standard TFF type-declaration syntax
    assert!(tptp.contains("tff(type_"),          "should have type_ prefix: {}", tptp);
    assert!(tptp.contains(", type, "),            "should have type role: {}", tptp);
    assert!(tptp.contains(": $i)."),              "constant should be : $i: {}", tptp);
    assert!(tptp.contains("% Type declarations"), "should have section comment: {}", tptp);
}

/// Multiple arithmetic builtins nested — Floor inside AdditionFn.
/// Java testFloor: equal(MillionYearsAgoFn(?X), BeginFn(YearFn(FloorFn(AdditionFn(1950, ...)))))
#[test]
fn tff_nested_arithmetic_builtins() {
    let layer = layer_from("(equal ?Z (AdditionFn ?X (SubtractionFn ?Y 1)))");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &tff_opts());
    assert!(tptp.contains("$sum("),        "AdditionFn should map to $sum: {}", tptp);
    assert!(tptp.contains("$difference("), "SubtractionFn should map to $difference: {}", tptp);
    assert!(tptp.contains(" = "),          "equal should produce =: {}", tptp);
}

/// Biconditional (<=>) expands to (A => B) & (B => A) in TFF as in FOF.
#[test]
fn tff_biconditional_expands() {
    let layer = layer_from("(<=> (instance ?X Foo) (instance ?X Bar))");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &tff_opts());
    // Both directions of the biconditional should be present
    assert!(tptp.matches("=>").count() >= 2, "biconditional should produce two implications: {}", tptp);
    assert!(tptp.contains(" & "),             "biconditional should produce conjunction: {}", tptp);
    assert!(!tptp.contains("holds"),          "TFF must not use holds: {}", tptp);
}

/// Forall/exists inside an implication (nested quantifiers).
/// Java test4: (=> ... (exists (?R) ...)) → ( ? [V__R:$i] : (...))
#[test]
fn tff_nested_exists_inside_implies() {
    let layer = layer_from(
        "(=> (instance ?X Animal) (exists (?Y) (and (instance ?Y Food) (instance ?X Organism))))"
    );
    let sid  = layer.store.roots[0];
    let tptp = sentence_to_tptp(sid, &layer, &tff_opts());
    assert!(tptp.contains("! ["),          "outer free var should be universally quantified: {}", tptp);
    assert!(tptp.contains("? ["),          "inner exists should be present: {}", tptp);
    assert!(tptp.contains("=>"),           "implication should be present: {}", tptp);
    assert!(!tptp.contains("holds"),       "TFF must not use holds: {}", tptp);
}

/// Absolute value builtin.
#[test]
fn tff_absolute_value_builtin() {
    let layer = layer_from("(equal ?Y (AbsoluteValueFn ?X))");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &tff_opts());
    assert!(tptp.contains("$abs("), "AbsoluteValueFn should map to $abs: {}", tptp);
}

/// Remainder builtin.
#[test]
fn tff_remainder_builtin() {
    let layer = layer_from("(equal ?Z (RemainderFn ?X ?Y))");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &tff_opts());
    assert!(tptp.contains("$remainder_e("), "RemainderFn should map to $remainder_e: {}", tptp);
}

/// Floor builtin.
#[test]
fn tff_floor_builtin() {
    let layer = layer_from("(equal ?Y (FloorFn ?X))");
    let sid   = layer.store.roots[0];
    let tptp  = sentence_to_tptp(sid, &layer, &tff_opts());
    assert!(tptp.contains("$floor("), "FloorFn should map to $floor: {}", tptp);
}
