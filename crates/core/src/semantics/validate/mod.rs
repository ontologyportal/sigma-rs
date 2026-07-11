//! Semantic validation layer: do the sentences adhere to SUMO semantics.
//!
//!   mod.rs       -- SemanticValidator + entry points and dispatch
//!   structural   -- SUMO well-formedness (head-is-relation, arity, domain, …)
//!   diagnostics  -- formula-shape lints (W020/W021/W022/E023)

use crate::types::RelationDomain;
use crate::{Element, OpKind, SentenceId};

use super::SemanticLayer;
use super::errors::SemanticError;
use super::types::Scope;

mod structural;
mod diagnostics;

/// Semantic validation over a [`SemanticLayer`].
///
/// A thin borrow of the layer: construct one per validation pass via
/// [`SemanticLayer::validator_scoped`], then call `validate_sentence_collect`.  Holds
/// no state beyond the borrow; the layer's caches do the memoisation.
pub(crate) struct SemanticValidator<'a> {
    layer: &'a SemanticLayer,
    /// The taxonomy/type scope the validator reasons in.  `Session(_)` makes a
    /// session's transient declarations visible.  Threaded into every `*_scoped`
    /// semantic query the structural checks make.
    scope: Scope,
    /// Sub-sentences already structurally validated in the current root pass.
    /// Guards `validate_structure` against re-validating a shared sub when a
    /// root references the same sub more than once.  Reset per root.
    visited: std::cell::RefCell<std::collections::HashSet<SentenceId>>,
}

impl SemanticLayer {
    /// A [`SemanticValidator`] borrowing this layer, reasoning in an explicit
    /// [`Scope`].
    pub(crate) fn validator_scoped(&self, scope: Scope) -> SemanticValidator<'_> {
        SemanticValidator { layer: self, scope, visited: Default::default() }
    }
}

// -- Validation ------------------------------------------------------------
impl<'a> SemanticValidator<'a> {
    /// Resolve a symbol id to its name as an owned `String`, falling back to
    /// an empty string when the id is not interned.
    pub(super) fn sym_name_str(&self, id: crate::SymbolId) -> String {
        self.layer
            .syntactic
            .sym_name(id)
            .map(|s| s.name().to_string())
            .unwrap_or_default()
    }

    /// Collect every semantic finding for root sentence `sid` into `out`.
    ///
    /// Every check runs to completion and each `SemanticError` is pushed
    /// (warnings and hard errors alike).  Whether a finding is an error or a
    /// warning is decided downstream when it is rendered as a
    /// [`Diagnostic`](crate::Diagnostic).
    fn collect_root(&self, sid: SentenceId, out: &mut Vec<SemanticError>) {
        if self.layer.syntactic.sentence(sid).is_none() {
            out.push(SemanticError::Other { msg: format!("Sentence {sid} does not exist") });
            return;
        }
        // W020/W021 walk the whole formula tree once and must run at the root
        // only: `validate_structure` recurses into subs and would double-count.
        self.check_single_use_variables(sid, out);
        self.check_free_vars_in_consequent(sid, out);
        self.visited.borrow_mut().clear();
        self.validate_structure(sid, out);
    }

    /// Structural well-formedness of one sentence — head-is-a-relation, arity,
    /// per-argument domain, operator shape — recursing into nested sub-sentence
    /// arguments.  Does not re-run the whole-tree formula-local checks.
    fn validate_structure(&self, sid: SentenceId, out: &mut Vec<SemanticError>) {
        if !self.visited.borrow_mut().insert(sid) {
            return;
        }
        let Some(sentence) = self.layer.syntactic.sentence(sid) else { return; };
        if sentence.is_operator() {
            self.validate_operator_sentence(sid, out);
            return;
        }
        crate::log!(Trace, "sigmakee_rs_core::semantic", format!("validating sentence sid={}", sid));

        self.validate_element(sentence.elements.first().unwrap(), out);

        // Relation-signature checks apply only to a concrete symbol head.  A
        // predicate-variable head `(?REL ...)` is higher-order: keying
        // `is_relation`/`arity` on its scoped id would always spuriously fail.
        if let Some(Element::Symbol(head_sym)) = sentence.elements.first() {
            let head_id = head_sym.id();
            if !self.layer.is_relation_scoped(head_id, self.scope) {
                out.push(SemanticError::HeadNotRelation {
                    sid,
                    sym: self.sym_name_str(head_id),
                });
            }

            let arg_count = sentence.elements.len().saturating_sub(1);
            if let Some(ar) = self.layer.arity(head_id) {
                if ar > 0 && ar as usize != arg_count {
                    out.push(SemanticError::ArityMismatch {
                        sid,
                        rel:      self.sym_name_str(head_id),
                        expected: ar as usize,
                        got:      arg_count,
                    });
                }
            }

            let domain = self.layer.domain_scoped(head_id, self.scope);
            if !domain.is_empty() {
                for (i, (arg, dom)) in sentence.elements[1..].iter().zip(domain.iter()).enumerate() {
                    if matches!(dom, RelationDomain::Unknown) {
                        continue;
                    }
                    if !self.arg_satisfies_domain(arg, dom) {
                        out.push(SemanticError::DomainMismatch {
                            sid,
                            rel:    self.sym_name_str(head_id),
                            arg:    i + 1,
                            domain: self.sym_name_str(dom.id().unwrap_or(u64::MAX)),
                        });
                    }
                }
            }
        }

        // Recurse into nested sub-sentence arguments (e.g. function terms like
        // `(MeasureFn 35 Cm)`) so they get their own structural validation.
        for arg in &sentence.elements[1..] {
            if let Element::Sub(sub_id) = arg {
                self.validate_structure(*sub_id, out);
            }
        }
    }

    fn validate_operator_sentence(&self, sid: SentenceId, out: &mut Vec<SemanticError>) {
        let sentence = match self.layer.syntactic.sentence(sid) {
            Some(s) => s,
            None => return,
        };
        let op: OpKind = match sentence.op().cloned() {
            Some(op) => op,
            None     => return,
        };

        let arity = op.arity();
        if arity > 0 && arity != sentence.arity() {
            out.push(SemanticError::ArityMismatch {
                sid,
                rel: op.name().to_string(),
                expected: arity, got: sentence.arity()
            });
        }

        if matches!(op, OpKind::And | OpKind::Or) && sentence.arity() == 1 {
            out.push(SemanticError::SingleArity { sid });
        }

        if matches!(op, OpKind::Implies | OpKind::Iff) {
            self.check_implication_shape(sid, out);
        }
        if matches!(op, OpKind::ForAll | OpKind::Exists) {
            self.check_quantifier_vacuous(sid, out);
        }

        if op == OpKind::Equal { return; }

        let is_quantifier = matches!(op, OpKind::ForAll | OpKind::Exists);
        let args_start = if is_quantifier { 2 } else { 1 };

        let sub_ids: Vec<SentenceId> = sentence
            .elements[args_start..]
            .iter()
            .filter_map(|e| if let Element::Sub(id) = e { Some(*id) } else { None })
            .collect();

        for (idx, sub_id) in sub_ids.iter().enumerate() {
            if !self.is_logical_sentence(*sub_id) {
                out.push(SemanticError::NonLogicalArg { sid, arg: idx + 1, op: op.to_string() });
            }
            self.validate_structure(*sub_id, out);
        }
    }
    /// Validate `sid` and return every finding (warnings + hard errors).  An
    /// empty vector means the sentence is clean.  Severity is applied downstream
    /// when each `SemanticError` is converted to a [`Diagnostic`](crate::Diagnostic).
    pub(crate) fn validate_sentence_collect(&self, sid: SentenceId) -> Vec<SemanticError> {
        let mut out = Vec::new();
        self.collect_root(sid, &mut out);
        out
    }

    // -- Syntactic diagnostics (W020-E023) -----------------------------------
    //
    // Row variables (`is_row=true`, e.g. `@ARGS`) are excluded throughout: they
    // are macro placeholders, not first-order variables.

}

#[cfg(test)]
mod tests {
    use crate::semantics::SemanticLayer;
    use crate::semantics::types::Scope;
    use crate::syntactic::SyntacticLayer;

    fn kif_layer(kif_str: &str) -> SemanticLayer {
        let mut store = SyntacticLayer::default();
        store.load_kif(kif_str, "base");
        SemanticLayer::new(store)
    }

    /// Root sentence ids in ascending `SentenceId` order.
    fn roots(layer: &SemanticLayer) -> Vec<crate::SentenceId> {
        let mut r: Vec<crate::SentenceId> =
            layer.syntactic.root_sids();
        r.sort_unstable();
        r
    }

    const BASE: &str = "
        (subclass Relation Entity)
        (subclass BinaryRelation Relation)
        (subclass Predicate Relation)
        (subclass BinaryPredicate Predicate)
        (subclass BinaryPredicate BinaryRelation)
        (instance subclass BinaryRelation)
        (domain subclass 1 Class)
        (domain subclass 2 Class)
        (instance instance BinaryPredicate)
        (domain instance 1 Entity)
        (domain instance 2 Class)
        (subclass Animal Entity)
        (subclass Human Entity)
        (subclass Human Animal)
    ";

    fn base_layer() -> SemanticLayer { kif_layer(BASE) }

    #[test]
    fn validate_sentence_valid() {
        let layer = base_layer();
        let sid = *layer.syntactic.by_head("subclass").iter().next().unwrap();
        assert!(layer.validator_scoped(Scope::Base).validate_sentence_collect(sid).is_empty());
    }

    #[test]
    fn validate_collect_over_all_roots_runs() {
        let layer = base_layer();
        let roots: Vec<_> = layer.syntactic.root_sids();
        for sid in roots {
            let _ = layer.validator_scoped(Scope::Base).validate_sentence_collect(sid);
        }
    }

    #[test]
    fn validate_sentence_collect_surfaces_findings() {
        let layer = kif_layer(r#"
            (subclass Foo Entity)
            ;; `Foo` is NOT declared as a relation.
            (Foo Bar Baz)
        "#);
        let foo_sids = layer.syntactic.by_head("Foo");
        assert!(!foo_sids.is_empty(), "expected a sentence headed by Foo");
        let sid = foo_sids.iter().next().unwrap();

        let errs = layer.validator_scoped(Scope::Base).validate_sentence_collect(*sid);
        assert!(errs.iter().any(|e| e.code() == "E002"),
            "validate_sentence_collect should include HeadNotRelation (E002); got {:?}",
            errs.iter().map(|e| e.code()).collect::<Vec<_>>());
        assert!(errs.iter().find(|e| e.code() == "E002").unwrap().is_warn(),
            "HeadNotRelation is warning-severity by default");
    }

    /// Find the single root sentence headed by predicate `head`.
    fn root_by_head(layer: &SemanticLayer, head: &str) -> crate::SentenceId {
        let sids = layer.syntactic.by_head(head);
        assert_eq!(sids.len(), 1, "expected exactly one root headed by `{head}`, got {sids:?}");
        *sids.iter().next().unwrap()
    }

    /// Find the single root whose operator matches `op` (for operator-headed
    /// roots like `and` / `=>` / `forall`, which `by_head` does not index).
    fn root_by_op(layer: &SemanticLayer, op: crate::OpKind) -> crate::SentenceId {
        let mut matches: Vec<crate::SentenceId> = layer
            .syntactic
            .root_sids()
            .into_iter()
            .filter(|&sid| {
                layer.syntactic.sentence(sid)
                    .and_then(|s| s.op().cloned())
                    .is_some_and(|o| o == op)
            })
            .collect();
        matches.sort_unstable();
        assert_eq!(matches.len(), 1, "expected exactly one root with op {op:?}, got {matches:?}");
        matches[0]
    }

    /// Find the single sub-sentence (at any depth, reachable from the roots)
    /// whose operator matches `op`.
    fn sub_by_op(layer: &SemanticLayer, op: crate::OpKind) -> crate::SentenceId {
        use crate::Element;
        fn walk(layer: &SemanticLayer, sid: crate::SentenceId, op: &crate::OpKind, out: &mut Vec<crate::SentenceId>) {
            let Some(sent) = layer.syntactic.sentence(sid) else { return };
            if sent.op().is_some_and(|o| o == op) { out.push(sid); }
            for el in &sent.elements {
                if let Element::Sub(sub) = el { walk(layer, *sub, op, out); }
            }
        }
        let mut out = Vec::new();
        for r in layer.syntactic.root_sids().into_iter() {
            walk(layer, r, &op, &mut out);
        }
        out.sort_unstable();
        out.dedup();
        assert_eq!(out.len(), 1, "expected exactly one sentence with op {op:?}, got {out:?}");
        out[0]
    }

    #[test]
    fn is_logical_sentence() {
        let layer = kif_layer("
            (=> (relation A B) (relation D C))
            (instance relation Relation)
            (relation A B)
            (NotARelation A B)
        ");
        let impl_sid         = root_by_op(&layer, crate::OpKind::Implies);
        let relation_sid     = root_by_head(&layer, "relation");
        let not_relation_sid = root_by_head(&layer, "NotARelation");
        assert!(layer.validator_scoped(Scope::Base).is_logical_sentence(impl_sid));
        assert!(layer.validator_scoped(Scope::Base).is_logical_sentence(relation_sid));
        // An undeclared head is treated as logical: only a positively-declared
        // function is non-logical.
        assert!(layer.validator_scoped(Scope::Base).is_logical_sentence(not_relation_sid));
    }

    #[test]
    fn is_not_logical_sentence_for_function_head() {
        let layer = kif_layer("
            (instance AbsoluteValueFn UnaryFunction)
            (instance AbsoluteValueFn Function)
            (AbsoluteValueFn N)
        ");
        let fn_sid = root_by_head(&layer, "AbsoluteValueFn");
        assert!(!layer.validator_scoped(Scope::Base).is_logical_sentence(fn_sid),
            "a declared-function head must be non-logical");
    }

    // -- New syntactic checks: W020, W021, W022, E023 ------------------------

    fn codes_in(layer: &SemanticLayer, sid: crate::SentenceId) -> Vec<&'static str> {
        layer.validator_scoped(Scope::Base).validate_sentence_collect(sid)
            .iter().map(|e| e.code()).collect()
    }

    #[test]
    fn w020_single_use_variable_flagged() {
        let layer = kif_layer(r#"
            (instance Animal Class)
            (forall (?X) (=> (instance ?X Animal) (instance ?Y Animal)))
        "#);
        let sid = roots(&layer).last().copied().unwrap();
        let codes = codes_in(&layer, sid);
        assert!(codes.contains(&"W020"),
            "expected W020 single-use-variable, got {:?}", codes);
    }

    #[test]
    fn w020_not_flagged_for_non_consequent_single_use_var() {
        // Only consequent single-use vars are flagged; a single-use antecedent
        // var is a legitimate "don't care" universal.
        let layer = kif_layer(r#"
            (instance Object Class)
            (=> (diameter ?C ?LEN) (instance ?C Object))
        "#);
        let sid = root_by_op(&layer, crate::OpKind::Implies);
        let codes = codes_in(&layer, sid);
        assert!(!codes.contains(&"W020"),
            "W020 must not fire for a single-use *antecedent* var; got {:?}", codes);
    }

    #[test]
    fn w020_no_false_positive_when_used_twice() {
        let layer = kif_layer(r#"
            (instance Animal Class)
            (forall (?X) (=> (instance ?X Animal) (instance ?X Animal)))
        "#);
        let sid = roots(&layer).last().copied().unwrap();
        let codes = codes_in(&layer, sid);
        assert!(!codes.contains(&"W020"),
            "W020 must not fire when var is used twice; got {:?}", codes);
    }

    #[test]
    fn w021_free_var_in_consequent() {
        let layer = kif_layer(r#"
            (instance Human Class)
            (=> (instance ?X Human) (instance ?Y Human))
        "#);
        let sid = root_by_op(&layer, crate::OpKind::Implies);
        let codes = codes_in(&layer, sid);
        assert!(codes.contains(&"W021"),
            "expected W021 free-var-in-consequent, got {:?}", codes);
    }

    #[test]
    fn domain_check_accepts_class_for_superclass_of_class_domain() {
        // Every class is an instance of `Class`, hence of its superclass
        // `SetOrClass`, so a class argument satisfies a `SetOrClass` domain.
        let layer = kif_layer(r#"
            (subclass SetOrClass Entity)
            (subclass Class SetOrClass)
            (instance lexicon BinaryPredicate)
            (domain lexicon 1 SetOrClass)
            (subclass Multipole Entity)
            (subclass Twopole Multipole)
            (lexicon Twopole Multipole)
        "#);
        let sid = root_by_head(&layer, "lexicon");
        let codes = codes_in(&layer, sid);
        assert!(!codes.contains(&"E006"),
            "a class must satisfy a SetOrClass domain (superclass of Class); got {:?}", codes);
    }

    #[test]
    fn w021_not_flagged_when_consequent_var_is_existentially_bound() {
        // `?Y` appears only in the consequent, but is bound by a nested
        // `(exists …)`, so it is not free.
        let layer = kif_layer(r#"
            (instance Human Class)
            (=> (instance ?X Human)
                (exists (?Y) (instance ?Y Human)))
        "#);
        let sid = root_by_op(&layer, crate::OpKind::Implies);
        let codes = codes_in(&layer, sid);
        assert!(!codes.contains(&"W021"),
            "W021 must not fire for an exists-bound consequent var; got {:?}", codes);
    }

    #[test]
    fn domain_check_does_not_flag_variable_arguments() {
        // A variable argument carries no statically-knowable type and is
        // constrained by the domain it sits in, so it can never violate a domain.
        let layer = kif_layer(r#"
            (subclass Human Entity)
            (instance Human Class)
            (instance brother BinaryPredicate)
            (domain brother 1 Human)
            (domain brother 2 Human)
            (brother ?A ?B)
        "#);
        let sid = root_by_head(&layer, "brother");
        let codes = codes_in(&layer, sid);
        assert!(!codes.contains(&"E006"),
            "E006 domain-mismatch must not fire on variable args; got {:?}", codes);
    }

    #[test]
    fn w021_not_flagged_when_consequent_var_bound_by_enclosing_antecedent() {
        // `?A` occurs in the consequent's inner implication `(part ?C ?A)` but is
        // bound by the outer antecedent `(surface ?A ?B)`.
        let layer = kif_layer(r#"
            (instance Object Class)
            (=> (surface ?A ?B)
                (forall (?C)
                    (=> (superficialPart ?C ?B) (part ?C ?A))))
        "#);
        let sid = root_by_op(&layer, crate::OpKind::Implies);
        let codes = codes_in(&layer, sid);
        assert!(!codes.contains(&"W021"),
            "W021 must not fire for a var bound by an enclosing antecedent; got {:?}", codes);
    }

    #[test]
    fn non_logical_arg_not_flagged_for_predicate_variable_head() {
        // `(?REL ?X ?Y)` is a higher-order literal — a predicate-variable
        // application — and is logical.
        let layer = kif_layer(r#"
            (instance Relation Class)
            (=> (instance ?REL Relation)
                (and (?REL ?X ?Y) (?REL ?Y ?X)))
        "#);
        let sid = root_by_op(&layer, crate::OpKind::Implies);
        let codes = codes_in(&layer, sid);
        assert!(!codes.contains(&"E004"),
            "E004 non-logical-arg must not fire on predicate-variable heads; got {:?}", codes);
    }

    #[test]
    fn w022_existential_in_antecedent() {
        let layer = kif_layer(r#"
            (instance Human Class)
            (=> (exists (?X) (instance ?X Human)) (instance ?X Human))
        "#);
        let sid = root_by_op(&layer, crate::OpKind::Implies);
        let codes = codes_in(&layer, sid);
        assert!(codes.contains(&"W022"),
            "expected W022 existential-in-antecedent, got {:?}", codes);
    }

    #[test]
    fn e023_quantifier_vacuous() {
        // `?Y` is in the forall var-list but never used in the body.  A
        // top-level `(forall …)` is stripped at ingest, so nest it under a
        // connective to survive as its own sub-sentence.
        let layer = kif_layer(r#"
            (instance Animal Class)
            (=> (instance Animal Class) (forall (?X ?Y) (instance ?X Animal)))
        "#);
        let sid = sub_by_op(&layer, crate::OpKind::ForAll);
        let codes = codes_in(&layer, sid);
        assert!(codes.contains(&"E023"),
            "expected E023 quantifier-vacuous, got {:?}", codes);
    }
}
