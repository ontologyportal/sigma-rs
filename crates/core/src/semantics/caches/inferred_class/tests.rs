//! Unit tests for scope-aware SUMO class inference (`compute_infer_class` and helpers).

use crate::semantics::caches::test_support::kif_layer as layer;
use crate::semantics::types::{ClassInference, ClassScope};

// -- classify_formula (variable inference) --------------------------------

#[test]
fn classify_formula_infers_variable_classes() {
    // The motivating case: `?X` is an Animal (instance guard) and `?M` is a
    // Mother (`mother`'s arg-1 domain), both embedded in a rule.
    let l = layer("
        (domain mother 1 Mother)
        (domain mother 2 Animal)
        (subclass Mother Animal)
        (=> (instance ?X Animal) (exists (?M) (mother ?M ?X)))
    ");
    let animal = l.syntactic.sym_id("Animal").unwrap();
    let mother = l.syntactic.sym_id("Mother").unwrap();

    // The rule is the only operator-headed root.
    let root = l.syntactic.root_sids().into_iter()
        .find(|&sid| l.syntactic.sentence(sid).map(|s| s.is_operator()).unwrap_or(false))
        .expect("implication root");

    let classes = l.classify_formula(root);
    // Exactly the two bound variables get classified (Animal / Mother values
    // and the relation heads are not argument targets).
    assert_eq!(classes.len(), 2, "both variables classified, got {classes:?}");

    let mut saw_animal = false;
    let mut saw_mother = false;
    for sc in classes.values() {
        match &sc.class {
            ClassInference::Single(id) if *id == animal => saw_animal = true,
            ClassInference::Single(id) if *id == mother => saw_mother = true,
            other => panic!("unexpected class {other:?}"),
        }
        // Every classification here comes from inside the rule → Local to it.
        assert!(matches!(sc.scope, ClassScope::Local(r) if r == root),
            "expected Local({root}) scope, got {:?}", sc.scope);
    }
    assert!(saw_animal, "?X should resolve to Animal");
    assert!(saw_mother, "?M should resolve to Mother");
}

#[test]
fn classify_formula_global_scope_for_ground_atoms() {
    // Ground root atoms classify their arguments at *Global* scope (and a
    // constant's taxonomy class falls out via the fold-in).
    let l = layer("
        (subclass Human Animal)
        (instance Bob Human)
        (domain mother 1 Mother)
        (mother Mary Jesus)
    ");
    let human    = l.syntactic.sym_id("Human").unwrap();
    let bob      = l.syntactic.sym_id("Bob").unwrap();
    let mother_c = l.syntactic.sym_id("Mother").unwrap();
    let mary     = l.syntactic.sym_id("Mary").unwrap();

    // (instance Bob Human) → Bob : Human, Global.
    let inst_root = *l.syntactic.by_head("instance").iter().next().unwrap();
    let bsc = l.classify_formula(inst_root).get(&bob).cloned().expect("Bob classified");
    assert!(matches!(&bsc.class, ClassInference::Single(id) if *id == human));
    assert!(matches!(bsc.scope, ClassScope::Global), "ground instance at root is Global");

    // (mother Mary Jesus) → Mary : Mother (mother's arg-1 domain), Global.
    let m_root = *l.syntactic.by_head("mother").iter().next().unwrap();
    let msc = l.classify_formula(m_root).get(&mary).cloned().expect("Mary classified");
    assert!(matches!(&msc.class, ClassInference::Single(id) if *id == mother_c));
    assert!(matches!(msc.scope, ClassScope::Global));
}

#[test]
fn classify_formula_equality_takes_function_range() {
    // (equal Joseph (FatherOfFn Jesus)) → Joseph takes FatherOfFn's range.
    let l = layer("
        (range FatherOfFn Man)
        (domain FatherOfFn 1 Human)
        (equal Joseph (FatherOfFn Jesus))
    ");
    let man    = l.syntactic.sym_id("Man").unwrap();
    let joseph = l.syntactic.sym_id("Joseph").unwrap();

    let root = l.syntactic.root_sids().into_iter()
        .find(|&sid| l.syntactic.sentence(sid)
            .and_then(|s| s.op().cloned())
            .map(|o| matches!(o, crate::OpKind::Equal)).unwrap_or(false))
        .expect("equality root");

    let jsc = l.classify_formula(root).get(&joseph).cloned().expect("Joseph classified");
    assert!(matches!(&jsc.class, ClassInference::Single(id) if *id == man),
        "Joseph = (FatherOfFn …) takes range Man, got {:?}", jsc.class);
    assert!(matches!(jsc.scope, ClassScope::Global), "ground equality at root → Global");
}

#[test]
fn classify_formula_symbol_equality_propagates_class() {
    // `(equal GeorgeWashington FirstPresident)` — `FirstPresident` inherits
    // whatever `GeorgeWashington` already is.
    let l = layer("
        (subclass Human Entity)
        (instance GeorgeWashington Human)
        (equal GeorgeWashington FirstPresident)
    ");
    let human = l.syntactic.sym_id("Human").unwrap();
    let gw    = l.syntactic.sym_id("GeorgeWashington").unwrap();
    let fp    = l.syntactic.sym_id("FirstPresident").unwrap();

    let root = l.syntactic.root_sids().into_iter()
        .find(|&sid| l.syntactic.sentence(sid)
            .and_then(|s| s.op().cloned())
            .map(|o| matches!(o, crate::OpKind::Equal)).unwrap_or(false))
        .expect("equality root");

    let fp_sc = l.classify_formula(root).get(&fp).cloned().expect("FirstPresident classified");
    assert!(matches!(&fp_sc.class, ClassInference::Single(id) if *id == human),
        "FirstPresident inherits GeorgeWashington's class, got {:?}", fp_sc.class);
    // …and it equals GeorgeWashington's own inferred class.
    assert!(matches!(l.infer_class(gw), ClassInference::Single(id) if id == human));
}

#[test]
fn classify_formula_skips_negated_atoms() {
    // `?X` is positively constrained to Foo, but the `(not (instance ?X Bar))`
    // guard must NOT classify it as Bar.
    let l = layer("
        (subclass Foo Entity)
        (subclass Bar Entity)
        (=> (and (instance ?X Foo) (not (instance ?X Bar))) (baz ?X))
    ");
    let foo = l.syntactic.sym_id("Foo").unwrap();

    let root = l.syntactic.root_sids().into_iter()
        .find(|&sid| l.syntactic.sentence(sid).map(|s| s.is_operator()).unwrap_or(false))
        .expect("implication root");

    let classes = l.classify_formula(root);
    assert_eq!(classes.len(), 1, "only ?X is classified (Bar is negated), got {classes:?}");
    let sc = classes.values().next().unwrap();
    assert!(matches!(&sc.class, ClassInference::Single(id) if *id == foo),
        "?X should be Foo only, got {:?}", sc.class);
}

#[test]
fn infer_class_sees_through_double_negation() {
    // `(not (not (instance ?X Foo)))` is cancelled at ingest, so the atom is
    // positive again and ?X classifies as Foo.
    let l = layer("
        (subclass Foo Entity)
        (=> (not (not (instance ?X Foo))) (bar ?X))
    ");
    let foo  = l.syntactic.sym_id("Foo").unwrap();
    let root = l.syntactic.root_sids().into_iter()
        .find(|&sid| !l.syntactic.sentence_vars(sid).is_empty())
        .expect("a root with variables");
    let (x, _) = l.syntactic.sentence_vars(root).into_iter().next().unwrap();
    assert!(matches!(l.infer_class(x), ClassInference::Single(id) if id == foo),
        "double-negated instance → ?X : Foo, got {:?}", l.infer_class(x));
}

#[test]
fn infer_class_drops_singly_negated_instance() {
    // ?X is positively Bar; the `(not (instance ?X Foo))` guard must not type
    // it Foo.
    let l = layer("
        (subclass Foo Entity)
        (subclass Bar Entity)
        (=> (and (instance ?X Bar) (not (instance ?X Foo))) (baz ?X))
    ");
    let bar  = l.syntactic.sym_id("Bar").unwrap();
    let root = l.syntactic.root_sids().into_iter()
        .find(|&sid| !l.syntactic.sentence_vars(sid).is_empty())
        .expect("a root with variables");
    let (x, _) = l.syntactic.sentence_vars(root).into_iter().next().unwrap();
    assert!(matches!(l.infer_class(x), ClassInference::Single(id) if id == bar),
        "?X should be Bar only (Foo is negated), got {:?}", l.infer_class(x));
}

#[test]
fn infer_class_not_and_yields_unknown() {
    // (not (and (instance ?X Foo) (instance ?X Bar))) ≡ (or (not Foo) (not Bar)):
    // neither class is asserted, so ?X is Unknown.  De Morgan drives the `not`
    // onto each atom, and the direct-`(not <atom>)` guard then drops both.
    let l = layer("
        (subclass Foo Entity)
        (subclass Bar Entity)
        (=> (not (and (instance ?X Foo) (instance ?X Bar))) (baz ?X))
    ");
    // Check every variable across the (CAF-split) roots — none may be classified.
    let mut checked = 0;
    for root in l.syntactic.root_sids().into_iter() {
        for (x, _) in l.syntactic.sentence_vars(root) {
            assert!(matches!(l.infer_class(x), ClassInference::Unknown),
                "?X under (not (and …)) must be Unknown, got {:?}", l.infer_class(x));
            checked += 1;
        }
    }
    assert!(checked > 0, "expected at least one variable to check");
}

#[test]
fn infer_class_equality_is_order_independent() {
    // (equal A B), A is a Human.  infer_class(B) must be Human whether B is
    // asked about before or after A — the equality component resolves the
    // same regardless of evaluation order.
    let mk = || layer("(subclass Human Entity)(instance A Human)(equal A B)");

    let l1 = mk();
    let human1 = l1.syntactic.sym_id("Human").unwrap();
    let b1     = l1.syntactic.sym_id("B").unwrap();
    assert!(matches!(l1.infer_class(b1), ClassInference::Single(id) if id == human1),
        "B-first must be Human, got {:?}", l1.infer_class(b1));

    let l2 = mk();
    let human2 = l2.syntactic.sym_id("Human").unwrap();
    let a2     = l2.syntactic.sym_id("A").unwrap();
    let b2     = l2.syntactic.sym_id("B").unwrap();
    let _ = l2.infer_class(a2);  // compute A first
    assert!(matches!(l2.infer_class(b2), ClassInference::Single(id) if id == human2),
        "B-after-A must ALSO be Human (order-independent), got {:?}", l2.infer_class(b2));
}

#[test]
fn infer_class_equality_chains_transitively() {
    // A = B = C, only C is taxonomy-classed → A, B, C all resolve to Human.
    let l = layer("
        (subclass Human Entity)
        (instance C Human)
        (equal A B)
        (equal B C)
    ");
    let human = l.syntactic.sym_id("Human").unwrap();
    for name in ["A", "B", "C"] {
        let id = l.syntactic.sym_id(name).unwrap();
        assert!(matches!(l.infer_class(id), ClassInference::Single(c) if c == human),
            "{name} should be Human via the equality chain, got {:?}", l.infer_class(id));
    }
}

#[test]
fn infer_class_domain_reads_all_positions() {
    // `Foo` is arg-1 (domain Region) AND arg-2 (domain District) of one
    // reflexive atom — both positions contribute, not just the first.
    let l = layer("
        (instance adjacent BinaryRelation)
        (domain adjacent 1 Region)
        (domain adjacent 2 District)
        (adjacent Foo Foo)
    ");
    let region   = l.syntactic.sym_id("Region").unwrap();
    let district = l.syntactic.sym_id("District").unwrap();
    let foo      = l.syntactic.sym_id("Foo").unwrap();
    match l.infer_class(foo) {
        ClassInference::Multiple(v) =>
            assert!(v.contains(&region) && v.contains(&district),
                "both arg positions' domains expected, got {v:?}"),
        other => panic!("expected Multiple(Region, District), got {other:?}"),
    }
}



// -- most_specific_class --------------------------------------------------

#[test]
fn most_specific_single_element() {
    let l = layer("(subclass Dog Animal)");
    let dog = l.syntactic.sym_id("Dog").unwrap();
    assert_eq!(l.most_specific_class(&[dog]), Some(dog));
}

#[test]
fn most_specific_chain_returns_deepest() {
    // Dog subclass Animal — Dog is more specific.
    let l = layer("(subclass Dog Animal)");
    let dog    = l.syntactic.sym_id("Dog").unwrap();
    let animal = l.syntactic.sym_id("Animal").unwrap();
    assert_eq!(l.most_specific_class(&[animal, dog]), Some(dog));
    assert_eq!(l.most_specific_class(&[dog, animal]), Some(dog));
}

// -- taxonomy path --------------------------------------------------------

#[test]
fn infer_class_from_taxonomy() {
    let l = layer("(instance Fido Dog)");
    let fido = l.syntactic.sym_id("Fido").unwrap();
    let dog  = l.syntactic.sym_id("Dog").unwrap();
    assert!(matches!(l.infer_class(fido), ClassInference::Single(id) if id == dog));
}

#[test]
fn infer_class_class_symbol_returns_class() {
    let l = layer("(subclass Dog Animal)");
    // No instance edge for Dog or Animal.
    let dog = l.syntactic.sym_id("Dog").unwrap();
    assert!(matches!(l.infer_class(dog), ClassInference::Class));
}

// -- occurrence (instance atom) path --------------------------------------

#[test]
fn infer_class_variable_in_antecedent() {
    // The (instance ?X Dog) sub-sentence is buried inside an implication.
    // The occurrence index records ?X at idx=1 of the (instance ...) sub-sentence.
    let l = layer("(=> (instance ?X Dog) (barks ?X))");
    // Find the scoped variable id for ?X.  Root ids are content hashes (no
    // load order), so locate the root that actually carries variables.
    let root_sid = l.syntactic.root_sids().into_iter()
        .find(|&sid| !l.syntactic.sentence_vars(sid).is_empty())
        .expect("a root with variables");
    let vars = l.syntactic.sentence_vars(root_sid);
    let (x_id, _) = vars.iter().next().expect("should have a variable");
    let dog = l.syntactic.sym_id("Dog").unwrap();
    assert!(matches!(l.infer_class(*x_id), ClassInference::Single(id) if id == dog));
}

#[test]
fn infer_class_most_specific_two_instance_atoms() {
    // Two (instance ?X ...) atoms in a conjunction — take most specific.
    // The `and` is wrapped in an implication so it is NOT a top-level
    // conjunction (which would be split into separate roots, giving each
    // conjunct its own scoped `?X`); here `?X` stays unified across both.
    let l = layer("
        (subclass SpecialDog Dog)
        (subclass Dog Animal)
        (=> (and (instance ?X Dog) (instance ?X SpecialDog)) (foo ?X))
    ");
    // The implication is the only root carrying variables.
    let root_sid = l.syntactic.root_sids().into_iter()
        .find(|&sid| !l.syntactic.sentence_vars(sid).is_empty())
        .expect("a root with variables");
    let vars = l.syntactic.sentence_vars(root_sid);
    let (x_id, _) = vars.iter().next().unwrap();
    let special_dog = l.syntactic.sym_id("SpecialDog").unwrap();
    assert!(matches!(l.infer_class(*x_id), ClassInference::Single(id) if id == special_dog));
}

// -- domain path ----------------------------------------------------------

#[test]
fn infer_class_from_domain() {
    let l = layer("
        (subclass Relation Entity)
        (subclass BinaryRelation Relation)
        (instance greaterThan BinaryRelation)
        (domain greaterThan 1 RealNumber)
        (greaterThan ?X 5)
    ");
    let root_sid = l.syntactic.root_sids().into_iter()
        .find(|&sid| !l.syntactic.sentence_vars(sid).is_empty())
        .expect("a root with variables");
    let vars = l.syntactic.sentence_vars(root_sid);
    let (x_id, _) = vars.iter().next().unwrap();
    let real_number = l.syntactic.sym_id("RealNumber").unwrap();
    assert!(matches!(l.infer_class(*x_id), ClassInference::Single(id) if id == real_number));
}

// -- equality path --------------------------------------------------------

#[test]
fn infer_class_equality_from_range() {
    let l = layer("
        (subclass Relation Entity)
        (subclass Function Relation)
        (subclass UnaryFunction Function)
        (instance SomeFn UnaryFunction)
        (range SomeFn Foo)
        (equal ?X (SomeFn a))
    ");
    let root_sid = l.syntactic.root_sids().into_iter()
        .find(|&sid| !l.syntactic.sentence_vars(sid).is_empty())
        .expect("a root with variables");
    let vars = l.syntactic.sentence_vars(root_sid);
    let (x_id, _) = vars.iter().next().unwrap();
    let foo = l.syntactic.sym_id("Foo").unwrap();
    assert!(matches!(l.infer_class(*x_id), ClassInference::Single(id) if id == foo));
}

#[test]
fn infer_class_relations() {
    let l = layer("
        (subclass Abstract Entity)
        (subclass Relation Abstract)
        (instance color Relation)
        (subrelation sheen color)
    ");
    let relation = l.syntactic.sym_id("Relation").unwrap();
    let color = l.syntactic.sym_id("color").unwrap();
    let sheen = l.syntactic.sym_id("sheen").unwrap();
    assert!(matches!(l.infer_class(color), ClassInference::Single(id) if id == relation));
    assert!(matches!(l.infer_class(sheen), ClassInference::Single(id) if id == relation));
}

#[test]
fn infer_class_multi() {
    let l = layer("
        (subclass Abstract Entity)
        (subclass Relation Abstract)
        (instance color Relation)
        (subrelation sheen color)
    ");
    let relation = l.syntactic.sym_id("Relation").unwrap();
    let color = l.syntactic.sym_id("color").unwrap();
    let sheen = l.syntactic.sym_id("sheen").unwrap();
    assert!(matches!(l.infer_class(color), ClassInference::Single(id) if id == relation));
    assert!(matches!(l.infer_class(sheen), ClassInference::Single(id) if id == relation));
}

#[test]
fn infer_class_unknown() {
    let l = layer("
        (=> (color ?X Red) (equals Red (ColorFn ?X)))
    ");
    let color = l.syntactic.sym_id("color").unwrap();
    let red = l.syntactic.sym_id("Red").unwrap();
    let color_fn = l.syntactic.sym_id("ColorFn").unwrap();
    assert!(matches!(l.infer_class(color), ClassInference::Unknown));
    assert!(matches!(l.infer_class(red), ClassInference::Unknown));
    assert!(matches!(l.infer_class(color_fn), ClassInference::Unknown));

    let root_sid = l.syntactic.root_sids().into_iter()
        .find(|&sid| !l.syntactic.sentence_vars(sid).is_empty())
        .expect("a root with variables");
    let vars = l.syntactic.sentence_vars(root_sid);
    let (x_id, _) = vars.iter().next().unwrap();
    assert!(matches!(l.infer_class(*x_id), ClassInference::Unknown));
}

#[test]
#[ignore = "superseded: the three-pass now folds the `performs` domain (PopStar) \
            into Bob's class, so the result is {Doctor, PopStar}, not {Singer, Doctor} \
            — this test asserts the retired 'domain ignored' behaviour"]
fn infer_class_multi_result_not_domain() {
    let l = layer("
        (subclass Relation Entity)
        (subclass Physical Entity)
        (subclass Animal Physical)
        (subclass Mammal Animal)
        (subclass Primate Mammal)
        (subclass Human Primate)
        (subclass Doctor Human)
        (subclass Singer Human)
        (subclass PopStar Singer)
        (instance Bob Primate)
        (instance Bob Doctor)
        (instance Bob Singer)
        (instance performs Relation)
        (domain performs 1 PopStar)
        (domain performs 2 Physical)
        (performs Bob ?X)
    ");
    let singer = l.syntactic.sym_id("Singer").unwrap();
    let doctor = l.syntactic.sym_id("Doctor").unwrap();
    let bob = l.syntactic.sym_id("Bob").unwrap();

    let bob_types = l.infer_class(bob);

    let ClassInference::Multiple(types) = bob_types else { panic!("Bob should have multiple class inferences") };
    assert_eq!(types.len(), 2);
    assert!(types.contains(&singer));
    assert!(types.contains(&doctor));
}

#[test]
fn infer_class_multi_result_domain() {
    // Bob's only classes come from being arg-1 of `performs` (→ PopStar)
    // and arg-1 of `brother` (→ HumanMale) — i.e. domain inference over
    // the relation occurrences.
    let l = layer("
        (subclass Relation Entity)
        (instance performs Relation)
        (domain performs 1 PopStar)
        (domain performs 2 Physical)
        (domain brother 1 HumanMale)
        (domain brother 2 HumanMale)
        (performs Bob ?X)
        (brother Bob George)
    ");
    let male = l.syntactic.sym_id("HumanMale").unwrap();
    let star = l.syntactic.sym_id("PopStar").unwrap();
    let bob = l.syntactic.sym_id("Bob").unwrap();

    let bob_types = l.infer_class(bob);

    let ClassInference::Multiple(types) = bob_types else { panic!("Bob should have multiple class inferences") };
    assert_eq!(types.len(), 2);
    assert!(types.contains(&male));
    assert!(types.contains(&star));
}
