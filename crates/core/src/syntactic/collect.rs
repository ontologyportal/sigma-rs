use std::collections::{HashMap, HashSet};

use crate::syntactic::SyntacticLayer;
use crate::{Element, OpKind, SentenceId, SymbolId};

impl SyntacticLayer {
     /// Collect every symbol id mentioned in `sent_id` (recursing into subs).
    pub(crate) fn collect_symbols(&self, sent_id: SentenceId, out: &mut HashSet<SymbolId>) {
        let Some(sentence) = self.sentence(sent_id) else { return };
        for el in &sentence.elements {
            match el {
                Element::Sub(sid) => { self.collect_symbols(*sid, out); },
                Element::Symbol(sym) => { out.insert(sym.id()); },
                _ => continue
            };
        }
    }

    /// Collect all the variable mentioned in a sentence (recurse if needed)
    pub(crate) fn collect_vars(&self, sent_id: SentenceId, out: &mut HashMap<SymbolId, u32>) {
        let Some(sentence) = self.sentence(sent_id) else { return };
        for el in &sentence.elements {
            match el {
                Element::Sub(sid) => { self.collect_vars(*sid, out); },
                Element::Variable { id, var_index, .. } => { out.insert(*id, *var_index); },
                _ => continue
            };
        }
    }

    /// Collect the variables that are bound by a FOL quantifier in the formula.
    /// Unbound variables are collected into a top level quanitifier
    ///
    /// If `in_formula_pos` if specified, variables bound in a formula which 
    /// appears nested inside a non-logical relation — 
    /// e.g. `(hasPurpose ?X (exists (?Y) ...))` — are returned as first order
    /// translation reifies the existential into a pseudo-relation and therefore 
    /// `?Y` becomes unbound in the process
    pub(crate) fn collect_bound_vars(
        &self,
        sid: SentenceId,
        in_formula_pos: bool,
        out: &mut HashSet<SymbolId>,
    ) {
        let Some(sentence) = self.sentence(sid) else { return };

        if in_formula_pos {
            if let Some(op) = sentence.op() {
                if matches!(op, OpKind::ForAll | OpKind::Exists) {
                    if let Some(Element::Sub(vl_sid)) = sentence.elements.get(1) {
                        if let Some(sub_sent) = self.sentence(*vl_sid) {
                            for e in &sub_sent.elements {
                                if let Element::Variable { id, .. } = e {
                                    out.insert(*id);
                                }
                            }   
                        }
                    }
                }
            }
        }

        // Dispatch sub-sentence positions by the current operator/head. The
        // rules mirror what `sid_to_formula` vs `sid_to_term` actually do
        // when they recurse, so `bound` stays in lockstep with where real
        // FOL binders end up in the IR output.
        let op = sentence.op();
        let sub_in_formula_pos = match op {
            // Logical connectives keep their children in formula position.
            Some(OpKind::And) | Some(OpKind::Or) | Some(OpKind::Not)
            | Some(OpKind::Implies) | Some(OpKind::Iff) => in_formula_pos,

            // Quantifiers are formula-level when we're at a formula site;
            // their body (and the var-list sub, which collect_all_var_ids
            // already indexes) inherit that position.
            Some(OpKind::ForAll) | Some(OpKind::Exists) => in_formula_pos,

            // `Equal` emits `IrF::eq(term, term)` — both sides are terms.
            Some(OpKind::Equal) => false,

            // Non-operator heads / atomic predicate applications:
            // `atomic_sid_to_formula` processes their args as terms
            // (`element_to_term`).  Inside a term context, everything
            // nested stays a term.
            None => false,
        };

        for elem in &sentence.elements {
            if let Element::Sub(sub) = elem {
                self.collect_bound_vars(*sub, sub_in_formula_pos, out);
            }
        }
    }
}