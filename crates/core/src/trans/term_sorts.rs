// crates/core/src/trans/term_sorts.rs
//
// The TERM-SORT TYPING POLICY of the lowering engine — every decision about
// which TFF sort an element carries at a given occurrence, split out of
// `lower.rs` so that file stays a mechanical tree walk.  lower.rs asks; this
// module answers:
//
//   * `infer_term_sort`      — the sort an element will emit at (literal shape,
//                              constant typing, function return, variable
//                              classification, arithmetic unification).
//   * `canonicalize_number_positions` — declared-`Number` positions pin to
//                              `$real` so facts and rules share one variant.
//   * `unified_numeric` / `unify_numeric_call` — interpreted-arithmetic calls
//                              unify to their widest numeric sort.
//   * `widen_term`           — the `$to_real` / `$to_rat` coercion wrapper.
//   * `ret_sort` / `typed_constant_sort` / `class_inference_to_sort` — the
//                              cache-backed per-symbol resolutions
//                              (`sort_annotations`, `symbol_sort`,
//                              `numeric_sorts`, `inferred_class`).
//
// Everything here is a pure read of the layer's caches; nothing mutates.

use std::collections::HashMap;

use crate::{Element, Literal, SymbolId};
use crate::semantics::types::Scope;
use crate::trans::builtins::{numeric_constant_value, SumoArith};
use crate::trans::ir::{Function, Interp, Term};
use crate::trans::sort::numeric_literal_sort;
use crate::trans::{Sort, SortAnnotation, TranslationLayer};

/// Per-root variable sorts, precomputed once per sentence from
/// [`classify_formula`](crate::semantics::SemanticLayer::classify_formula).
///
/// A variable's class evidence lives ONLY in its binding formula (variable ids
/// are root-unique), so the formula walk is both necessary and sufficient — a
/// global scan can miss the root entirely (a conjecture is not a session
/// *member*, so neither the axiom index nor the session membership reaches it).
pub(in crate::trans) type VarSorts = HashMap<SymbolId, Sort>;

impl TranslationLayer {
    /// The declared return sort of a relation/function (Individual default).
    /// `Polymorphic` annotations are not consumed yet (variant selection is
    /// call-site), so they fall back to Individual.  `Base` reads the memoised
    /// `sort_annotations` cache; a session scope resolves the range directly.
    pub(in crate::trans) fn ret_sort(&self, id: SymbolId, scope: Scope) -> Sort {
        if matches!(scope, Scope::Base) {
            return match self.sort_annotation(id) {
                SortAnnotation::Relation { ret_sort, .. } => ret_sort.unwrap_or(Sort::Individual),
                SortAnnotation::Polymorphic(_) | SortAnnotation::Constant(_) => Sort::Individual,
            };
        }
        if !self.semantic.is_function_scoped(id, scope) {
            return Sort::Individual;
        }
        match self.semantic.range_scoped(id, scope) {
            crate::types::RelationRange::Range(cls) => self.sort_for_id(cls),
            _ => Sort::Individual,
        }
    }

    /// The sort at which a bare symbol is emitted as a typed numeric constant in
    /// TFF, or `None` to keep it an untyped `$i` constant.  A numeric *class*
    /// used as a term stays `$i`.
    pub(in crate::trans) fn typed_constant_sort(&self, id: SymbolId, scope: Scope) -> Option<Sort> {
        if self.numeric_sorts.get(&id).is_some() {
            return None;
        }
        match self.sort_for_symbol_scoped(id, scope) {
            Ok(s) if s != Sort::Individual => Some(s),
            _ => None,
        }
    }

    /// Best-effort static inference of the TFF sort an element will emit at,
    /// kept in lockstep with what `element_to_term` produces so equality
    /// coercion and predicate-variant naming never disagree with a term's type.
    pub(in crate::trans) fn infer_term_sort(&self, el: &Element, scope: Scope, vars: &VarSorts) -> Option<Sort> {
        match el {
            Element::Sub(sid) => {
                let sentence = self.semantic.syntactic.sentence(*sid)?;
                let head = match sentence.elements.first()? {
                    Element::Symbol(sym) if self.semantic.is_function_scoped(sym.id(), scope) => sym,
                    _ => return None,
                };
                if let Some(arith) = SumoArith::from_sumo_name(&head.name()) {
                    if !arith.is_predicate() {
                        // MUST mirror `sub_to_term` exactly: the emitted call
                        // sorts are declaration-RESOLVED before unification,
                        // so the inference unifies over the same resolved
                        // sorts — otherwise an `$int`-argued call in a
                        // `$real`-declared position emits at `$real` while
                        // inferring `$int`, and the enclosing equality types
                        // its other side wrong (ill-typed TFF).
                        let actual: Vec<Option<Sort>> = sentence.elements[1..]
                            .iter()
                            .map(|e| self.infer_term_sort(e, scope, vars))
                            .collect();
                        let resolved = self.resolve_call_sorts(head.id(), &actual, scope);
                        if let Some(unified) = unified_numeric(&resolved) {
                            return Some(unified);
                        }
                    }
                }
                Some(self.ret_sort(head.id(), scope))
            }
            Element::Symbol(sym) => {
                let id = sym.id();
                if numeric_constant_value(&sym.name()).is_some() {
                    Some(Sort::Real)
                } else if self.semantic.is_relation_scoped(id, scope) {
                    Some(Sort::Individual)
                } else {
                    Some(self.typed_constant_sort(id, scope).unwrap_or(Sort::Individual))
                }
            }
            Element::Literal(Literal::Number(n)) => {
                Some(self.collapse_numeric(numeric_literal_sort(n)))
            }
            // A variable's sort is the per-root formula classification — the
            // same map the quantifier wrap reads.
            Element::Variable { id, .. } => Some(vars.get(id).copied().unwrap_or(Sort::Individual)),
            _ => None,
        }
    }

    /// The DECLARED expected sort of each argument position of `rel`: a
    /// numeric-classed domain pins its position (the abstract `Number` maps
    /// to `$real`), everything else is Individual — "no declaration
    /// constraint".  `Base` reads the memoised `sort_annotations` cache; a
    /// session scope resolves the domains directly (mirrors `ret_sort`).
    /// Polymorphic relations return empty (no single signature — variant
    /// expansion handles them); so do constants.
    pub(in crate::trans) fn declared_arg_sorts(&self, rel: SymbolId, scope: Scope) -> Vec<Sort> {
        if matches!(scope, Scope::Base) {
            return match self.sort_annotation(rel) {
                SortAnnotation::Relation { arg_sorts, .. } => arg_sorts,
                SortAnnotation::Polymorphic(_) | SortAnnotation::Constant(_) => Vec::new(),
            };
        }
        self.semantic
            .domain_scoped(rel, scope)
            .iter()
            .map(|d| match d {
                crate::types::RelationDomain::Domain(cls) => {
                    self.numeric_sort_of_class(*cls).unwrap_or(Sort::Individual)
                }
                _ => Sort::Individual,
            })
            .collect()
    }

    /// Resolve the call sorts of `(rel args…)`: DECLARATION-DRIVEN with
    /// call-site fallback.  A declared-numeric position pins to its declared
    /// sort whenever the actual argument can carry it (equal or widenable via
    /// `$to_real`/`$to_rat`) — so every use of a declared relation lands on
    /// the same typed variant.  The call-site actual stands where the
    /// declaration is silent (Individual), the actual is an un-coercible `$i`
    /// term, or the actual is *wider* than the declaration (no narrowing
    /// coercion exists; emitting the declared sort there would be ill-typed).
    pub(in crate::trans) fn resolve_call_sorts(
        &self,
        rel:    SymbolId,
        actual: &[Option<Sort>],
        scope:  Scope,
    ) -> Vec<Sort> {
        let declared = self.declared_arg_sorts(rel, scope);
        actual
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let act = a.unwrap_or(Sort::Individual);
                match declared.get(i).copied().unwrap_or(Sort::Individual) {
                    Sort::Individual => act,
                    dec if act == dec => dec,
                    dec if act != Sort::Individual && widens_to(act, dec) => dec,
                    _ => act,
                }
            })
            .collect()
    }

    /// Collapse a [`ClassInference`](crate::types::ClassInference) to the TFF
    /// [`Sort`] it types at: the most specific numeric-sorted candidate, or
    /// `$i` when none is numeric.
    pub(in crate::trans) fn class_inference_to_sort(&self, inf: &crate::types::ClassInference) -> Sort {
        use crate::types::ClassInference;
        match inf {
            ClassInference::Single(c) => self.numeric_sort_of_class(*c).unwrap_or(Sort::Individual),
            ClassInference::Multiple(cs) => cs
                .iter()
                .filter_map(|c| self.numeric_sort_of_class(*c))
                .max()
                .unwrap_or(Sort::Individual),
            _ => Sort::Individual,
        }
    }
}

/// Does a widening coercion `from` → `to` exist?  (The boolean face of
/// [`widen_term`]: `$int→$rat`, `$int→$real`, `$rat→$real`.)
pub(in crate::trans) fn widens_to(from: Sort, to: Sort) -> bool {
    matches!(
        (from, to),
        (Sort::Integer, Sort::Real) | (Sort::Rational, Sort::Real) | (Sort::Integer, Sort::Rational)
    )
}

/// Wrap `term` with the TPTP coercion to widen `from` → `to` (`$int→$rat`,
/// `$int→$real`, `$rat→$real`); `None` when no widening applies.
pub(in crate::trans) fn widen_term(term: Term, from: Sort, to: Sort) -> Option<Term> {
    if from == to {
        return None;
    }
    let (name, interp) = match (from, to) {
        (Sort::Integer, Sort::Real) => ("$to_real", Interp::IntToReal),
        (Sort::Rational, Sort::Real) => ("$to_real", Interp::RatToReal),
        (Sort::Integer, Sort::Rational) => ("$to_rat", Interp::IntToRat),
        _ => return None,
    };
    Some(Term::apply(Function::interpreted(name, interp), vec![term]))
}

/// The widest numeric sort in `sorts` under `Integer ⊂ Rational ⊂ Real`, or
/// `None` when every position is Individual.  NOT `Sort::Ord` (that orders by
/// *specificity*, the reverse) — this is the arithmetic-widening order.
fn numeric_max(sorts: &[Sort]) -> Option<Sort> {
    let rank = |s: Sort| match s {
        Sort::Individual => 0u8,
        Sort::Integer => 1,
        Sort::Rational => 2,
        Sort::Real => 3,
    };
    sorts.iter().copied().filter(|s| rank(*s) > 0).max_by_key(|s| rank(*s))
}

/// `Some(widest)` when every position of a call is numeric — the shared
/// precondition for mapping a SUMO arithmetic relation/function to a TPTP
/// interpreted theory symbol.  `None` for empty or mixed calls.
pub(in crate::trans) fn unified_numeric(sorts: &[Sort]) -> Option<Sort> {
    if sorts.is_empty() || sorts.iter().any(|s| *s == Sort::Individual) {
        return None;
    }
    numeric_max(sorts)
}

/// Unify an all-numeric interpreted-arithmetic call to its widest sort in
/// place (narrower args then widen up via `$to_real` / `$to_rat`), returning
/// the unified sort.  No-op on mixed / non-numeric calls.
pub(in crate::trans) fn unify_numeric_call(call_sorts: &mut [Sort]) -> Option<Sort> {
    let unified = unified_numeric(call_sorts)?;
    for s in call_sorts.iter_mut() {
        *s = unified;
    }
    Some(unified)
}
