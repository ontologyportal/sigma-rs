// crates/core/src/trans/poly_expand.rs
//
// POLYMORPHIC VARIANT EXPANSION — the consumer of
// [`SortAnnotation::Polymorphic`] and the `poly_variant_symbols` cache.
//
// A relation whose domain position is declared at a numeric *ancestor*
// (`Quantity`, …) has no single declared sort: a call can legitimately carry
// `$i` or any numeric sort, so `resolve_call_sorts` falls back to call-site
// inference and different uses land on DIFFERENT typed variants.  A fact
// `(P 40 …)` emits at the `__1In` variant while a rule whose variable is not
// numerically classified emits at the bare variant — two distinct symbols the
// prover can never join (incompleteness, not unsoundness).
//
// The expansion closes the gap at problem-assembly time: for each selected
// rule that uses a poly relation at a *flexible* position (one whose
// `Polymorphic` variants disagree) with an unclassified variable, the rule is
// re-lowered once per plausible numeric sort of that variable
// ([`TranslationLayer::lower_axiom_variant`] pins the binder), and the copies
// join the problem as extra axioms.  Bounded per rule by
// [`MAX_ASSIGNMENTS_PER_RULE`]; the base ($i) copy always remains.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::types::Element;
use crate::semantics::types::Scope;
use crate::trans::term_sorts::VarSorts;
use crate::trans::{Sort, SortAnnotation, TranslationLayer};
use crate::{SentenceId, SymbolId};

/// Cap on per-variant copies of one rule (3 sorts ^ 2 vars = 9 covers every
/// realistic schema; anything past this is logged and dropped).
const MAX_ASSIGNMENTS_PER_RULE: usize = 9;

impl TranslationLayer {
    /// The variant-expansion specs for a problem's selected `sids` (TFF only):
    /// `(sid, variable-sort overrides)` pairs, each of which lowers to one
    /// additional axiom via [`Self::lower_axiom_variant`].
    ///
    /// A sid produces specs when it mentions a `poly_variant_symbols` relation
    /// whose flexible position holds a variable the per-root classification
    /// leaves at `$i`.  Each such variable ranges over the position's numeric
    /// candidate sorts (from the `Polymorphic` enumeration); the cartesian
    /// assignment set is capped at [`MAX_ASSIGNMENTS_PER_RULE`] per sid.
    pub(crate) fn poly_expansions(&self, sids: &[SentenceId]) -> Vec<(SentenceId, VarSorts)> {
        let polys = self.poly_variant_symbols.get(self);
        if polys.is_empty() {
            return Vec::new();
        }

        let mut out: Vec<(SentenceId, VarSorts)> = Vec::new();
        for &sid in sids {
            // Cheap prefilter: skip sids that mention no poly relation.
            if self
                .semantic
                .syntactic
                .sentence_symbols(sid)
                .is_disjoint(&polys)
            {
                continue;
            }
            let scope = self.scope_of(sid);
            // The same variable typing the lowering used for the base copy.
            let var_sorts: VarSorts = self
                .semantic
                .classify_formula_scoped(sid, scope)
                .into_iter()
                .map(|(k, sc)| (k, self.class_inference_to_sort(&sc.class)))
                .collect();

            // candidate var -> its numeric sort options (union across the
            // flexible positions it occupies).  BTree for determinism.
            let mut candidates: BTreeMap<SymbolId, BTreeSet<Sort>> = BTreeMap::new();
            self.collect_poly_candidates(sid, &polys, &var_sorts, scope, &mut candidates);
            if candidates.is_empty() {
                continue;
            }

            // Cartesian sort assignments over the candidate vars, capped.
            let vars: Vec<SymbolId> = candidates.keys().copied().collect();
            let mut assignments: Vec<VarSorts> = vec![VarSorts::new()];
            for v in &vars {
                let opts = &candidates[v];
                let mut next = Vec::with_capacity(assignments.len() * opts.len());
                for a in &assignments {
                    for &sort in opts {
                        let mut a2 = a.clone();
                        a2.insert(*v, sort);
                        next.push(a2);
                    }
                }
                assignments = next;
                if assignments.len() > MAX_ASSIGNMENTS_PER_RULE {
                    crate::log!(Debug, "sigmakee_rs_core::trans", format!(
                        "poly_expansions: sid {sid} capped at {MAX_ASSIGNMENTS_PER_RULE} \
                         of {} variant assignments", assignments.len()));
                    assignments.truncate(MAX_ASSIGNMENTS_PER_RULE);
                }
            }
            for a in assignments {
                out.push((sid, a));
            }
        }
        out
    }

    /// Walk `sid`'s tree; for every atom/application headed by a poly
    /// relation, record each *flexible-position* variable the classification
    /// left at `$i`, with the position's numeric sort options.
    fn collect_poly_candidates(
        &self,
        sid:        SentenceId,
        polys:      &HashSet<SymbolId>,
        var_sorts:  &VarSorts,
        scope:      Scope,
        candidates: &mut BTreeMap<SymbolId, BTreeSet<Sort>>,
    ) {
        let mut stack = vec![sid];
        let mut seen: HashSet<SentenceId> = HashSet::new();
        while let Some(s) = stack.pop() {
            if !seen.insert(s) {
                continue;
            }
            let Some(sentence) = self.semantic.syntactic.sentence(s) else { continue };
            for el in sentence.elements.iter() {
                if let Element::Sub(sub) = el {
                    stack.push(*sub);
                }
            }
            let Some(Element::Symbol(head)) = sentence.elements.first() else { continue };
            if !polys.contains(&head.id()) {
                continue;
            }
            // Flexible positions + their numeric options, from the
            // `Polymorphic` enumeration: a position is flexible when the
            // variants disagree there; its options are the distinct
            // non-Individual sorts across variants.
            let SortAnnotation::Polymorphic(variants) = self.sort_annotation(head.id()) else {
                continue;
            };
            let args = &sentence.elements[1..];
            for (i, arg) in args.iter().enumerate() {
                let Element::Variable { id, .. } = arg else { continue };
                if var_sorts.get(id).copied().unwrap_or(Sort::Individual) != Sort::Individual {
                    continue; // already numerically classified — base copy has it
                }
                let opts: BTreeSet<Sort> = variants
                    .iter()
                    .filter_map(|v| v.arg_sorts().get(i).copied())
                    .filter(|s| *s != Sort::Individual)
                    .collect();
                if opts.is_empty() {
                    continue; // fixed (non-flexible) position
                }
                candidates.entry(*id).or_default().extend(opts);
            }
        }
        let _ = scope; // scope is carried by the caller's classification
    }
}
