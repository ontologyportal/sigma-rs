//! Formula-shape syntactic diagnostics: W020 single-use variable, W021 free
//! variable in consequent, W022 existential in antecedent, E023 vacuous
//! quantifier.

use std::collections::{HashMap, HashSet};

use crate::{Element, OpKind, SentenceId};

use super::super::errors::SemanticError;
use super::SemanticValidator;

impl<'a> SemanticValidator<'a> {
    /// Walk every non-row variable reachable from `sid` (recursing into
    /// `Element::Sub` children) and call `f(name)` for each occurrence.
    ///
    /// Includes occurrences inside quantifier var-lists; callers needing a
    /// binder-vs-use distinction filter from the resulting counts.
    fn walk_vars(&self, sid: SentenceId, f: &mut dyn FnMut(&str)) {
        let Some(s) = self.layer.syntactic.sentence(sid) else { return };
        for el in &s.elements {
            match el {
                Element::Sub(sid) => self.walk_vars(*sid, f),
                Element::Variable { name, is_row: false, .. } => f(name),
                _ => {}
            }
        }
    }

    /// W020 single-use-variable.  Flags a variable that occurs exactly once in
    /// an implication consequent (the canonical typo being `?X` vs `?Y`).
    ///
    /// Single occurrences elsewhere — a top-level fact, or an antecedent
    /// "don't care" — are legitimate implicit universals and are not flagged.
    /// Consequent variables are found via the same binding-structure walk as
    /// W021.
    pub(super) fn check_single_use_variables(&self, sid: SentenceId, out: &mut Vec<SemanticError>) {
        let mut counts: HashMap<String, usize> = HashMap::new();
        self.walk_vars(sid, &mut |name: &str| {
            *counts.entry(name.to_string()).or_insert(0) += 1;
        });
        let mut bound:      HashSet<String> = HashSet::new();
        let mut consequent: HashSet<String> = HashSet::new();
        self.collect_binding_structure(sid, &mut bound, &mut consequent);
        for (var, count) in counts {
            if count == 1 && consequent.contains(&var) {
                out.push(SemanticError::SingleUseVariable { sid, var });
            }
        }
    }

    /// W021 free-var-in-consequent, computed once over the entire root formula.
    ///
    /// A variable is flagged iff it occurs in some implication consequent yet is
    /// bound nowhere in the rule — it never appears in any antecedent (at any
    /// nesting depth) and is never introduced by a `forall` / `exists`.
    /// Computing over the whole tree keeps variables bound by an enclosing
    /// antecedent or quantifier from being mistaken for free occurrences.
    pub(super) fn check_free_vars_in_consequent(&self, root: SentenceId, out: &mut Vec<SemanticError>) {
        let mut bound:      HashSet<String> = HashSet::new();
        let mut consequent: HashSet<String> = HashSet::new();
        self.collect_binding_structure(root, &mut bound, &mut consequent);

        let mut free: Vec<String> =
            consequent.into_iter().filter(|v| !bound.contains(v)).collect();
        free.sort_unstable();
        for var in free {
            out.push(SemanticError::FreeVarInConsequent { sid: root, var });
        }
    }

    /// Walk the formula tree once, partitioning variable occurrences for W021:
    ///   * `bound`      — variables that bind their scope: every antecedent's
    ///                    variables (both halves of an `<=>`, since each binds
    ///                    the other), plus every `forall` / `exists` var-list.
    ///   * `consequent` — variables occurring in an implication consequent.
    /// A variable in both sets (e.g. bound by a *nested* antecedent that sits
    /// inside an outer consequent) is therefore not free.
    fn collect_binding_structure(
        &self,
        sid:        SentenceId,
        bound:      &mut HashSet<String>,
        consequent: &mut HashSet<String>,
    ) {
        let Some(s) = self.layer.syntactic.sentence(sid) else { return };
        match s.op() {
            Some(OpKind::Implies) => {
                if let (Some(Element::Sub(a)), Some(Element::Sub(c))) =
                    (s.elements.get(1), s.elements.get(2))
                {
                    self.walk_vars(*a, &mut |n: &str| { bound.insert(n.to_string()); });
                    self.walk_vars(*c, &mut |n: &str| { consequent.insert(n.to_string()); });
                }
            }
            Some(OpKind::Iff) => {
                // Both halves bind each other — a variable on either side is
                // tied to the other, so neither half is a pure consequent.
                if let (Some(Element::Sub(a)), Some(Element::Sub(c))) =
                    (s.elements.get(1), s.elements.get(2))
                {
                    self.walk_vars(*a, &mut |n: &str| { bound.insert(n.to_string()); });
                    self.walk_vars(*c, &mut |n: &str| { bound.insert(n.to_string()); });
                }
            }
            Some(OpKind::ForAll | OpKind::Exists) => {
                if let Some(Element::Sub(varlist_sid)) = s.elements.get(1) {
                    if let Some(vl) = self.layer.syntactic.sentence(*varlist_sid) {
                        for el in &vl.elements {
                            if let Element::Variable { name, is_row: false, .. } = el {
                                bound.insert(name.clone());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        for el in &s.elements {
            if let Element::Sub(child) = el {
                self.collect_binding_structure(*child, bound, consequent);
            }
        }
    }

    /// Does the subtree rooted at `sid` contain an `exists` operator
    /// anywhere?  Used by [`Self::check_implication_shape`] to detect
    /// existentials trapped under an antecedent (W022).
    fn subtree_has_existential(&self, sid: SentenceId) -> bool {
        let Some(s) = self.layer.syntactic.sentence(sid) else { return false };
        if matches!(s.op(), Some(OpKind::Exists)) { return true; }
        for el in &s.elements {
            if let Element::Sub(sid) = el {
                if self.subtree_has_existential(*sid) { return true; }
            }
        }
        false
    }

    /// W022 existential-in-antecedent.  Called on `(=> ant cons)` and
    /// `(<=> ant cons)` sentences; an `exists` anywhere under the antecedent
    /// means the witness can't be referenced in the consequent.
    ///
    /// (W021 free-var-in-consequent is *not* checked here — it is a
    /// whole-formula property handled once at the root by
    /// [`Self::check_free_vars_in_consequent`].)
    pub(super) fn check_implication_shape(&self, sid: SentenceId, out: &mut Vec<SemanticError>) {
        let Some(s) = self.layer.syntactic.sentence(sid) else { return };
        // elements: [Op{Implies|Iff}, Sub{ant}, Sub{cons}]
        let ant_sid = match s.elements.get(1) {
            Some(Element::Sub(a)) => *a,
            _ => return,
        };

        // W022.
        if self.subtree_has_existential(ant_sid) {
            out.push(SemanticError::ExistentialInAntecedent { sid });
        }
    }

    /// E023 quantifier-vacuous.  Called on `(forall (vars...) body)` and
    /// `(exists (vars...) body)`.  Flags a variable listed in the quantifier's
    /// var-list that is never referenced in the body.
    pub(super) fn check_quantifier_vacuous(&self, sid: SentenceId, out: &mut Vec<SemanticError>) {
        let Some(s) = self.layer.syntactic.sentence(sid) else { return };
        // elements: [Op{ForAll|Exists}, Sub{varlist}, Sub{body}, ...]
        let var_list_sid = match s.elements.get(1) {
            Some(Element::Sub(sid)) => *sid,
            _ => return,
        };

        let varlist: HashSet<String> = {
            let mut out = HashSet::new();
            if let Some(vl) = self.layer.syntactic.sentence(var_list_sid) {
                for el in &vl.elements {
                    if let Element::Variable { name, is_row: false, .. } = el {
                        out.insert(name.clone());
                    }
                }
            }
            out
        };

        // KIF allows multiple body forms after the var list; accept the
        // general shape by collecting vars from every body sub.
        let mut body_vars: HashSet<String> = HashSet::new();
        for el in s.elements[2..].iter() {
            if let Element::Sub(body_sid) = el {
                self.walk_vars(*body_sid, &mut |n: &str| { body_vars.insert(n.to_string()); });
            }
        }

        for var in varlist.iter() {
            if !body_vars.contains(var) {
                out.push(SemanticError::QuantifierVacuous { sid, var: var.clone() });
            }
        }
    }
}
