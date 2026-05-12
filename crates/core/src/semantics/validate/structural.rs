// crates/core/src/semantics/validate/structural.rs
//
// SUMO well-formedness: a sentence's head must be a relation with satisfied
// arity/domain; functions need a single range; symbol-case conventions.

use crate::{Element, SentenceId};

use super::super::consts::ROOT_SYMBOL;
use super::super::errors::SemanticError;
use super::super::types::{RelationDomain, RelationRange};
use super::SemanticValidator;

impl<'a> SemanticValidator<'a> {
    /// Validate a single element, pushing any findings into `out`.
    pub(super) fn validate_element(&self, el: &Element, out: &mut Vec<SemanticError>) {
        let id = match el {
            Element::Variable { is_row: false, .. } => return,
            Element::Symbol(sym) => sym.id(),
            Element::Sub(sid)    => { self.validate_structure(*sid, out); return; }
            _                    => return,
        };
        if !self.layer.has_ancestor_by_name_scoped(id, &ROOT_SYMBOL.name(), self.scope) {
            out.push(SemanticError::NoEntityAncestor { sym: self.sym_name_str(id) });
        }
        if self.layer.is_relation_scoped(id, self.scope) {
            // Each declared argument position must name a domain class; an
            // `Unknown` gap (`rd.id() == None`) means none was declared there.
            for (idx, rd) in self.layer.domain_scoped(id, self.scope).iter().enumerate() {
                if rd.id().is_none() {
                    out.push(SemanticError::MissingDomain { sym: self.sym_name_str(id), idx });
                }
            }

            // A relation must declare its arity (via its `BinaryRelation` / … ancestry).
            if self.layer.arity(id).is_none() {
                out.push(SemanticError::MissingArity { sym: self.sym_name_str(id) });
            }

            if self.layer.is_function_scoped(id, self.scope) {
                // A function needs a declared range.  `Unknown` covers both "no
                // range" and "conflicting range/rangeSubclass"; the latter is
                // additionally surfaced as a `DoubleRange` diagnostic by the
                // `semantic::range` cache reactor on ingest, so the validator only
                // flags the missing case here.
                if matches!(self.layer.range_scoped(id, self.scope), RelationRange::Unknown) {
                    out.push(SemanticError::MissingRange { sym: self.sym_name_str(id) });
                }

                let fun_name = self.sym_name_str(id);
                if !fun_name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                    out.push(SemanticError::FunctionCase { sym: fun_name });
                }
            } else if self.layer.is_predicate_scoped(id, self.scope) {
                let rel_name = self.sym_name_str(id);
                if rel_name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                    out.push(SemanticError::PredicateCase { sym: rel_name });
                }
            }
        }
    }

    pub(crate) fn is_logical_sentence(&self, sid: SentenceId) -> bool {
        let Some(sentence) = &self.layer.syntactic.sentence(sid) else { return false };
        if sentence.is_operator() { return true; }
        let head_id = match sentence.elements.first() {
            Some(Element::Symbol(sym)) => sym.id(),
            // A predicate-variable head `(?REL ?x ?y)` is a higher-order literal
            // — always logical.  Its relation-hood can't be checked statically
            // (the scoped id is just the variable name), and these are pervasive
            // in SUMO meta-axioms, so treating them as non-logical floods false
            // positives.
            Some(Element::Variable { .. }) => return true,
            _ => return false,
        };
        // A sentence is logical unless its head is *positively* a function.
        // Unknown symbols (e.g. a relation declared in a not-yet-loaded
        // constituent) are assumed logical — unknown ≠ not-a-relation.  A
        // misused non-relation symbol head is already surfaced as
        // `HeadNotRelation` (E002) by `validate_structure`, so it needs no
        // second flag here.
        !self.layer.is_function_scoped(head_id, self.scope)
    }

    pub(super) fn arg_satisfies_domain(&self, arg: &Element, dom: &RelationDomain) -> bool {
        match arg {
            Element::Symbol(sym) => {
                let sym_id = sym.id();
                match dom {
                    RelationDomain::Domain(dom_id) => {
                        let dom_name = self.sym_name_str(*dom_id);
                        if dom_name == &*ROOT_SYMBOL.name() { return true; }
                        // A class is an instance of `Class`, hence of every
                        // *superclass* of Class (SetOrClass, Abstract, Entity, …).
                        // So a class argument satisfies a `Domain(C)` constraint
                        // whenever C is Class or one of its ancestors — e.g.
                        // `(lexicon Twopole …)` with `(domain lexicon 1 SetOrClass)`
                        // and `Twopole` a class.  This subsumes the old literal
                        // `dom_name == "Class"` special-case (Class is an
                        // ancestor-or-self of Class) and is taxonomy-driven rather
                        // than name-hardcoded.
                        if self.layer.is_class_scoped(sym_id, self.scope) {
                            if let Some(class_id) = self.layer.syntactic.sym_id("Class") {
                                if class_id == *dom_id
                                    || self.layer.has_ancestor_scoped(class_id, *dom_id, self.scope)
                                {
                                    return true;
                                }
                            }
                        }
                        self.layer.is_instance_scoped(sym_id, self.scope) && self.layer.has_ancestor_scoped(sym_id, *dom_id, self.scope)
                    }
                    RelationDomain::DomainSubclass(dom_id) => {
                        let dom_name = self.sym_name_str(*dom_id);
                        if dom_name == &*ROOT_SYMBOL.name() { return true; }
                        // `domainSubclass R N Class` means "the argument must be a
                        // class".  Any symbol that IS a class satisfies this, even
                        // if it is not itself a subclass of `Class` in the hierarchy
                        // (e.g. SetOrClass is a superclass of Class, not a subclass,
                        // yet it is a class and is a valid range for rangeSubclass).
                        if dom_name == "Class"  { return self.layer.is_class_scoped(sym_id, self.scope); }
                        self.layer.is_class_scoped(sym_id, self.scope) && self.layer.has_ancestor_scoped(sym_id, *dom_id, self.scope)
                    }
                    // No declared domain for this position — no constraint to fail.
                    RelationDomain::Unknown => true,
                }
            }
            // A variable carries no statically-knowable type: it is *constrained*
            // by the very domain declaration we're checking against (SUMO types its
            // variables through predicate domains), so it can never *violate* one.
            // The old code asked `is_class`/`is_instance` of the variable's scoped
            // id (`?NUM` → symbol `NUM__<scope>`), but that id is undeclared, and
            // `is_class` treats any edge-less symbol as a root class — which made
            // *every* variable spuriously fail a `Domain` position. (see
            // semantics/caches/is_class.rs "no incoming edges → class").
            Element::Variable { is_row: false, .. }
            | Element::Variable { is_row: true, .. }
            | Element::Sub(_)
            | Element::Literal(_) => true,
            Element::Op(_) => false,
        }
    }
    
    // -- Batch validation ------------------------------------------------------

}
