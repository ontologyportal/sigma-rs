//! Per-sentence formula caches and problem assembly on `TranslationLayer`.
//!
//! Two lazy-memoising formula accessors are provided: `formula_tff` (TFF mode,
//! `$int(42)` syntax, memoised with preamble declarations in `formulas_tff`)
//! and `formula_fof` (FOF mode, `n__42` opaque constants, no type declarations,
//! memoised in `formulas_fof`). `prime_formula_cache` eagerly warms both caches
//! for every emittable `SentenceId`, and `build_problem` assembles a complete
//! `ir::Problem` from the cached formulas, deduplicating TFF preamble
//! declarations in one pass.

use std::collections::HashSet;

use crate::types::{Element, SentenceId};
use crate::trans::{TranslationError, ir, CachedFormula};
#[cfg(feature = "ask")]
use crate::trans::lower::QueryVarMap;
use crate::parse::tptp::syntax::TptpLang;

use super::TranslationLayer;

/// Set of declaration keys already emitted while assembling one
/// [`ir::Problem`], shared between the axiom loop and the conjecture merge.
#[derive(Default)]
struct DeclSeen {
    sorts: HashSet<String>,
    fns:   HashSet<(String, u32)>,
    preds: HashSet<(String, u32)>,
}

impl DeclSeen {
    /// Declare `cf`'s TFF preamble entries on `problem`, skipping keys
    /// already declared.
    fn merge_decls(&mut self, problem: &mut ir::Problem, cf: &CachedFormula) {
        for s in &cf.sort_decls {
            if self.sorts.insert(s.tptp_name().to_string()) {
                problem.declare_sort(s.clone());
            }
        }
        for f in &cf.fn_decls {
            if self.fns.insert((f.name().to_string(), f.arity())) {
                problem.declare_function(f.clone());
            }
        }
        for p in &cf.pred_decls {
            if self.preds.insert((p.name().to_string(), p.arity())) {
                problem.declare_predicate(p.clone());
            }
        }
    }
}

impl TranslationLayer {
    // -------------------------------------------------------------------------
    // Emittable SID enumeration
    // -------------------------------------------------------------------------

    /// All SentenceIds that should appear in formula caches and TPTP output:
    /// root sentences plus **top-level** rewrite-pass synthetics, minus
    /// suppressed ones and minus intermediate fragments.
    fn all_emittable_sids(&self) -> Vec<SentenceId> {
        let syn = &self.semantic.syntactic;
        let children = self.synthetic_child_sids();
        let roots: Vec<SentenceId> = syn.root_sids();
        let suppressed = self.suppressed.read().unwrap();
        let mut sids: Vec<SentenceId> = roots
            .iter()
            .chain(syn.synthetic_origin.keys())
            .copied()
            .filter(|sid| !suppressed.contains(sid) && !children.contains(sid))
            .collect();
        sids.sort_unstable();
        sids.dedup();
        sids
    }

    /// The set of synthetic sentence ids that appear as a `Sub` child of some
    /// other synthetic sentence — intermediate building blocks (an augmented
    /// antecedent, a substituted conjunct, …) that the rewrite /
    /// predicate-variable passes assemble into a final synthetic.
    ///
    /// These must never be emitted as standalone axioms: an augmented
    /// antecedent emitted alone is a universally-quantified bare conjunction,
    /// which is contradictory. They are still converted inline as part of their
    /// parent (the converter recurses through `Sub`), so excluding them from
    /// the top-level set is lossless.
    fn synthetic_child_sids(&self) -> HashSet<SentenceId> {
        let syn = &self.semantic.syntactic;
        let mut children: HashSet<SentenceId> = HashSet::new();
        for &synth in syn.synthetic_origin.keys() {
            let Some(s) = syn.sentence(synth) else { continue };
            for e in &s.elements {
                if let Element::Sub(sid) = e {
                    if syn.synthetic_origin.contains_key(sid) {
                        children.insert(*sid);
                    }
                }
            }
        }
        children
    }

    // -------------------------------------------------------------------------
    // Bulk prime
    // -------------------------------------------------------------------------

    /// Eagerly warm both formula caches for all emittable SentenceIds.
    ///
    /// Should be called after [`TranslationLayer::prime_caches`] and the
    /// rewrite pass have run, so that `suppressed` is up-to-date and
    /// `all_emittable_sids` includes any synthetic sentences.
    ///
    /// When `formulas_fof` is disabled in the shared `CacheConfig`, the FOF
    /// loop performs on-the-fly conversions whose results are discarded.
    pub fn prime_formula_cache(&self) -> Result<(), TranslationError> {
        let sids = self.all_emittable_sids();

        #[cfg(feature = "parallel")]
        {
            use rayon::prelude::*;
            sids.par_iter().for_each(|&sid| {
                let _ = self.formula_tff(sid);
                let _ = self.formula_fof(sid);
            });
        }
        #[cfg(not(feature = "parallel"))]
        for sid in sids {
            let _ = self.formula_tff(sid);
            let _ = self.formula_fof(sid);
        }
        Ok(())
    }

    /// Snapshot the per-sentence formula caches.
    ///
    /// Returns `(tff_map, fof_map)` containing every populated entry
    /// in each cache (including `None` entries for suppressed sids).
    #[cfg(feature = "ask")]
    #[allow(dead_code)]
    pub(crate) fn snapshot_formula_caches(
        &self,
    ) -> (
        std::collections::HashMap<SentenceId, Option<CachedFormula>>,
        std::collections::HashMap<SentenceId, Option<CachedFormula>>,
    ) {
        (self.formulas_tff.snapshot(), self.formulas_fof.snapshot())
    }

    /// Bulk-install previously-snapshotted formula caches into the
    /// in-memory `EntryCache`s.
    ///
    /// Replaces any existing in-memory entries with the supplied maps
    /// wholesale; the caller is responsible for ensuring the maps are
    /// `kb_version`-consistent with the loaded sentence store.
    #[cfg(feature = "ask")]
    #[allow(dead_code)]
    pub(crate) fn restore_formula_caches(
        &self,
        tff: std::collections::HashMap<SentenceId, Option<CachedFormula>>,
        fof: std::collections::HashMap<SentenceId, Option<CachedFormula>>,
    ) {
        self.formulas_tff.restore(tff);
        self.formulas_fof.restore(fof);
    }

    // -------------------------------------------------------------------------
    // Problem assembly
    // -------------------------------------------------------------------------

    /// Assemble a complete [`ir::Problem`] from the cached formulas for
    /// `axiom_sids` in the requested `mode`.
    ///
    /// Returns `(problem, sid_map)` where `sid_map[i]` is the [`SentenceId`]
    /// that produced `problem.axioms()[i]`.  SIDs that are suppressed or that
    /// cannot be converted are silently omitted from both, so
    /// `sid_map.len() == problem.axioms().len()` always holds.
    ///
    /// - TFF: each axiom's `sort_decls`, `fn_decls`, and `pred_decls` are
    ///   merged into the preamble with deduplication.
    /// - FOF: no preamble declarations are emitted.
    ///
    /// On a cache miss for any SID (cold start, or cache disabled),
    /// `formula_tff` / `formula_fof` falls back to on-the-fly conversion
    /// automatically.
    ///
    /// Callers that need a conjecture (and the on-demand synthetic scan) should
    /// use [`Self::assemble_problem`] instead.
    pub(crate) fn build_problem(
        &self,
        axiom_sids: &[SentenceId],
        mode:       TptpLang,
    ) -> (ir::Problem, Vec<SentenceId>) {
        let (problem, sid_map, _) = self.build_problem_with_decls(axiom_sids, mode);
        (problem, sid_map)
    }

    /// [`Self::build_problem`] plus the declaration-dedup sets it accumulated,
    /// so a caller appending a conjecture can dedup against them instead of
    /// rebuilding the sets from the assembled problem.
    fn build_problem_with_decls(
        &self,
        axiom_sids: &[SentenceId],
        mode:       TptpLang,
    ) -> (ir::Problem, Vec<SentenceId>, DeclSeen) {
        let mut problem = if mode.is_typed() { ir::Problem::new_tff() } else { ir::Problem::new() };

        let mut seen = DeclSeen::default();

        // sid_map[i] is the SentenceId that produced problem.axioms()[i].
        let mut sid_map: Vec<SentenceId> = Vec::with_capacity(axiom_sids.len());

        #[cfg(feature = "parallel")]
        {
            use rayon::prelude::*;
            axiom_sids.par_iter().for_each(|&sid| if mode.is_typed() {
                let _ = self.formula_tff(sid);
            } else {
                let _ = self.formula_fof(sid);
            });
        }

        for &sid in axiom_sids {
            let Some(cf) = (if mode.is_typed() { self.formula_tff(sid) } else { self.formula_fof(sid) }) else {
                continue;
            };

            seen.merge_decls(&mut problem, &cf);
            problem.with_axiom(cf.formula);
            sid_map.push(sid);
        }

        (problem, sid_map, seen)
    }

    /// The main translation entry point: assemble a complete [`ir::Problem`]
    /// from a **pre-selected** axiom set plus an optional conjecture.
    ///
    /// The selected axioms are first scanned for *synthetic-formula
    /// eligibility*, and the eligible synthetics are generated, interned, and
    /// included in the translation:
    ///
    /// 1. **Replacements** — rewrite-pass synthetics that stand in for a
    ///    suppressed original in the selection ([`Self::synthetic_replacements`]).
    /// 2. **Predicate-variable instantiations** — property schemas
    ///    (transitivity / symmetry / subrelation-propagation) instantiated for
    ///    the concrete relations that actually occur in this problem
    ///    ([`Self::instantiate_predvars`]; `seed_sids` get cap priority).
    ///
    /// The axioms then translate through the (eagerly maintained) formula
    /// caches via [`Self::build_problem`]; the conjecture candidates are tried
    /// in order and the first convertible one installs with an existential
    /// free-variable wrap, merging its declarations into the preamble and
    /// returning its [`QueryVarMap`] for proof-binding extraction.
    ///
    /// Returns `(problem, sid_map, qvm)`; `qvm` is `None` when no conjecture
    /// was requested or none of the candidates converted.
    #[cfg(feature = "ask")]
    pub(crate) fn assemble_problem(
        &self,
        axiom_sids:  &[SentenceId],
        seed_sids:   &[SentenceId],
        conjecture:  &[SentenceId],
        mode:        TptpLang,
        query_scope: Option<crate::semantics::types::Scope>,
    ) -> (ir::Problem, Vec<SentenceId>, Option<QueryVarMap>) {
        // -- synthetic-eligibility scan over the selection --------------------
        let mut sids: Vec<SentenceId> = axiom_sids.to_vec();
        sids.sort_unstable();
        sids.dedup();

        // Replacements of suppressed originals in the selection.
        let extra = self.synthetic_replacements(&sids);
        sids.extend(extra);

        // Predicate-variable schema instantiation, scoped to this problem's
        // relations (conjecture + seeds prioritised under the cap).
        let pv = {
            let mut seed: Vec<SentenceId> = conjecture.to_vec();
            seed.extend(seed_sids.iter().copied());
            let mut scope: Vec<SentenceId> = conjecture.to_vec();
            scope.extend(sids.iter().copied());
            self.instantiate_predvars(
                &seed, &scope,
                query_scope.unwrap_or(crate::semantics::types::Scope::Base),
            )
        };
        sids.extend(pv);
        sids.sort_unstable();
        sids.dedup();

        // -- translate ---------------------------------------------------------
        let (mut problem, mut sid_map, mut decl_seen) = self.build_problem_with_decls(&sids, mode);

        // Polymorphic variant expansion (TFF only): rules using a poly
        // relation at a flexible position with an unclassified variable
        // re-lower once per plausible numeric sort, so they join facts
        // emitted at numeric variants.
        if mode.is_typed() {
            for (sid, overrides) in self.poly_expansions(&sids) {
                if let Some(cf) = self.lower_axiom_variant(sid, &overrides) {
                    decl_seen.merge_decls(&mut problem, &cf);
                    problem.with_axiom(cf.formula);
                    sid_map.push(sid);
                }
            }
        }

        // -- conjecture --------------------------------------------------------
        // The conjecture must hide numbers exactly as the cached axioms do
        // (FOF hides; TFF emits raw numerics). The candidates are the
        // CAF-normalized pieces of one query sharing variable ids across roots,
        // so they install as a single conjunction with the free-variable union
        // wrapped once.
        let mut qvm = None;
        if let Some((cf, map)) = self.lower_conjecture_set(
            conjecture,
            mode.is_typed(),
            !mode.is_typed(),
            query_scope,
        ) {
            decl_seen.merge_decls(&mut problem, &cf);
            problem.conjecture(cf.formula);
            qvm = Some(map);
        }

        (problem, sid_map, qvm)
    }

    /// Given a set of selected sentence ids, return the synthetic sentences
    /// produced by the rewrite pass that replaced them, so they can be added
    /// to the problem alongside the selection.
    ///
    /// The rewrite pass often suppresses an original implication and emits a
    /// guard-augmented / normalized synthetic in its place. `build_problem`
    /// skips the suppressed original, so unless the replacement is also in the
    /// sid list the axiom vanishes entirely.
    ///
    /// `synthetic_origin` maps each synthetic to its immediate parent, so a
    /// multi-stage chain is resolved by a transitive reverse closure starting
    /// from `selected`. Suppressed intermediates in the returned set are
    /// harmless — `build_problem` skips them; only the final, un-suppressed
    /// synthetic is emitted.
    pub(crate) fn synthetic_replacements(&self, selected: &[SentenceId]) -> Vec<SentenceId> {
        use std::collections::{HashMap, HashSet};
        let syn = &self.semantic.syntactic;
        if syn.synthetic_origin.is_empty() {
            return Vec::new();
        }
        // origin -> [synthetics produced directly from it]
        let mut by_origin: HashMap<SentenceId, Vec<SentenceId>> = HashMap::new();
        for (&synth, &origin) in syn.synthetic_origin.iter() {
            by_origin.entry(origin).or_default().push(synth);
        }
        let child_synthetics = self.synthetic_child_sids();
        let mut seen: HashSet<SentenceId> = selected.iter().copied().collect();
        let mut stack: Vec<SentenceId> = selected.to_vec();
        let mut out: Vec<SentenceId> = Vec::new();
        let predvar_instances = self.predvar_instances.read().unwrap();
        while let Some(sid) = stack.pop() {
            if let Some(children) = by_origin.get(&sid) {
                for &child in children {
                    if !seen.insert(child) { continue; }
                    // Predicate-variable instantiations are per-problem only;
                    // never sweep them in here and don't recurse through them,
                    // or one problem's relation-property rules leak into another.
                    if predvar_instances.contains(&child) { continue; }
                    // Emit unless `child` is an intermediate building block or a
                    // bare positive assertion; still recurse either way, since
                    // descendants may themselves be top-level.
                    if !child_synthetics.contains(&child)
                        && !crate::trans::rewrite::is_bare_positive_assertion(syn, child)
                    {
                        out.push(child);
                    }
                    stack.push(child);
                }
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantics::SemanticLayer;
    use crate::syntactic::SyntacticLayer;
    use crate::trans::ir::Formula as IrFormula;

    fn make_trans(kif: &str) -> TranslationLayer {
        let mut store = SyntacticLayer::default();
        store.load_kif(kif, "test");
        let sem = SemanticLayer::new(store);
        TranslationLayer::new(sem)
    }

    /// All root sentence ids for `trans`, sorted for deterministic ordering.
    fn roots_of(trans: &TranslationLayer) -> Vec<SentenceId> {
        let mut r: Vec<SentenceId> = trans.semantic.syntactic.root_sids();
        r.sort();
        r
    }

    /// The sole root sentence id (for single-sentence KBs).
    fn root_of(trans: &TranslationLayer) -> SentenceId {
        roots_of(trans)[0]
    }

    // -------------------------------------------------------------------------
    // formula_tff
    // -------------------------------------------------------------------------

    #[test]
    fn formula_tff_simple_predicate() {
        let trans = make_trans("(instance Fido Dog)");
        let sid = root_of(&trans);
        let f = trans.formula_tff(sid);
        assert!(f.is_some(), "should convert (instance Fido Dog) to a TFF formula");
    }

    #[test]
    fn formula_tff_implication() {
        let trans = make_trans("(=> (P ?X) (Q ?X))");
        let sid = root_of(&trans);
        let f = trans.formula_tff(sid);
        assert!(f.is_some(), "should convert an implication to a TFF formula");
        assert!(
            matches!(f.unwrap().formula, IrFormula::ForallTyped(..)),
            "free variable should be universally quantified with a sort in TFF mode"
        );
    }

    #[test]
    fn formula_tff_has_declarations() {
        // A predicate in TFF mode should produce pred_decls in the cached entry.
        let trans = make_trans("(instance Fido Dog)");
        let sid = root_of(&trans);
        let cf = trans.formula_tff(sid).expect("should convert");
        assert!(
            !cf.pred_decls.is_empty(),
            "TFF conversion should register at least one predicate declaration"
        );
    }

    #[test]
    fn formula_tff_suppressed_returns_none() {
        // Load a numeric characterisation that the rewrite pass will suppress.
        let kif = "(=> (instance ?X PositiveInteger) (greaterThan ?X 0))\n\
                   (=> (instance ?Y PositiveInteger) (SomePred ?Y))";
        let mut store = SyntacticLayer::default();
        store.load_kif(kif, "test");
        let sem = SemanticLayer::new(store);
        let mut trans = TranslationLayer::new(sem);

        // Prime the numeric sort for PositiveInteger so the rewrite pass fires.
        let pos_int_id = trans.semantic.syntactic.sym_id("PositiveInteger").unwrap();
        trans.numeric_sorts.update(pos_int_id, crate::trans::Sort::Integer);

        // Manually run the rewrite pass to populate suppressed.
        crate::trans::rewrite::run_rewrite_pass(
            &trans.numeric_sorts,
            &mut trans.suppressed.write().unwrap(),
            &mut trans.semantic.syntactic,
        );

        // The template sentence should now be suppressed.
        for &sid in trans.suppressed.read().unwrap().iter() {
            assert!(
                trans.formula_tff(sid).is_none(),
                "suppressed sentence {} should return None from formula_tff", sid
            );
        }
    }

    #[test]
    fn formula_tff_is_memoised() {
        let trans = make_trans("(instance Fido Dog)");
        let sid = root_of(&trans);
        // First call populates the cache.
        let f1 = trans.formula_tff(sid);
        // Second call should return the cached value.
        let f2 = trans.formula_tff(sid);
        assert!(f1.is_some() && f2.is_some());
        assert_eq!(f1.unwrap().formula, f2.unwrap().formula,
            "formula_tff should return identical results on repeated calls");
        assert!(trans.formulas_tff.peek(&sid).is_some(),
            "result should be in the formulas_tff cache");
    }

    #[test]
    fn formula_tff_cache_cleared_on_taxonomy_change() {
        use crate::cache::events::Event;
        use crate::layer::Layer;
        let trans = make_trans("(instance Fido Dog)");
        let sid = root_of(&trans);
        // Populate the cache.
        let _ = trans.formula_tff(sid);
        assert!(trans.formulas_tff.peek(&sid).is_some());
        // Drive a taxonomy change through the real reactor cascade (the formula
        // caches coarse-clear on any TaxonomyChanged).
        let dog = trans.semantic.syntactic.sym_id("Dog").unwrap();
        trans.cascade(vec![Event::TaxonomyChanged { syms: vec![dog] }]);
        assert!(
            trans.formulas_tff.peek(&sid).is_none(),
            "formulas_tff cache should be cleared after taxonomy change"
        );
    }

    #[test]
    fn formula_tff_cache_cleared_on_domain_range_change() {
        use crate::cache::events::Event;
        use crate::layer::Layer;
        let trans = make_trans("(instance Fido Dog)");
        let sid = root_of(&trans);
        let _ = trans.formula_tff(sid);
        assert!(trans.formulas_tff.peek(&sid).is_some());
        let dog = trans.semantic.syntactic.sym_id("Dog").unwrap();
        trans.cascade(vec![Event::DomainRangeChanged { syms: vec![dog] }]);
        assert!(
            trans.formulas_tff.peek(&sid).is_none(),
            "formulas_tff cache should be cleared after domain/range change"
        );
    }

    // -------------------------------------------------------------------------
    // formula_fof
    // -------------------------------------------------------------------------

    #[test]
    fn formula_fof_simple_predicate() {
        let trans = make_trans("(instance Fido Dog)");
        let sid = root_of(&trans);
        let f = trans.formula_fof(sid);
        assert!(f.is_some(), "should convert (instance Fido Dog) to a FOF formula");
    }

    #[test]
    fn formula_fof_implication_is_untyped() {
        let trans = make_trans("(=> (P ?X) (Q ?X))");
        let sid = root_of(&trans);
        let cf = trans.formula_fof(sid).expect("should convert");
        assert!(
            matches!(cf.formula, IrFormula::Forall(..)),
            "free variable should be universally quantified WITHOUT a sort in FOF mode"
        );
    }

    #[test]
    fn formula_fof_has_no_declarations() {
        // FOF emits no type declarations; all *_decls must be empty.
        let trans = make_trans("(instance Fido Dog)");
        let sid = root_of(&trans);
        let cf = trans.formula_fof(sid).expect("should convert");
        assert!(cf.sort_decls.is_empty(), "FOF sort_decls must be empty");
        assert!(cf.fn_decls.is_empty(),   "FOF fn_decls must be empty");
        assert!(cf.pred_decls.is_empty(), "FOF pred_decls must be empty");
    }

    #[test]
    fn formula_fof_and_tff_structural_difference() {
        // The same sentence must produce structurally different formula variants
        // in the two modes.
        let trans = make_trans("(=> (P ?X) (Q ?X))");
        let sid = root_of(&trans);
        let tff = trans.formula_tff(sid).expect("TFF convert ok").formula;
        let fof = trans.formula_fof(sid).expect("FOF convert ok").formula;
        assert!(
            matches!(tff, IrFormula::ForallTyped(..)),
            "TFF should produce ForallTyped"
        );
        assert!(
            matches!(fof, IrFormula::Forall(..)),
            "FOF should produce Forall (untyped)"
        );
    }

    #[test]
    fn formula_fof_suppressed_returns_none() {
        let kif = "(=> (instance ?X PositiveInteger) (greaterThan ?X 0))\n\
                   (=> (instance ?Y PositiveInteger) (SomePred ?Y))";
        let mut store = SyntacticLayer::default();
        store.load_kif(kif, "test");
        let sem = SemanticLayer::new(store);
        let mut trans = TranslationLayer::new(sem);

        let pos_int_id = trans.semantic.syntactic.sym_id("PositiveInteger").unwrap();
        trans.numeric_sorts.update(pos_int_id, crate::trans::Sort::Integer);
        crate::trans::rewrite::run_rewrite_pass(
            &trans.numeric_sorts,
            &mut trans.suppressed.write().unwrap(),
            &mut trans.semantic.syntactic,
        );

        for &sid in trans.suppressed.read().unwrap().iter() {
            assert!(
                trans.formula_fof(sid).is_none(),
                "suppressed sentence {} should return None from formula_fof", sid
            );
        }
    }

    #[test]
    fn formula_fof_is_memoised() {
        let trans = make_trans("(instance Fido Dog)");
        let sid = root_of(&trans);
        let f1 = trans.formula_fof(sid);
        let f2 = trans.formula_fof(sid);
        assert!(f1.is_some() && f2.is_some());
        assert_eq!(f1.unwrap().formula, f2.unwrap().formula,
            "formula_fof should return identical results on repeated calls");
        assert!(trans.formulas_fof.peek(&sid).is_some(),
            "result should be in the formulas_fof cache");
    }

    #[test]
    fn formula_fof_cache_cleared_on_taxonomy_change() {
        use crate::cache::events::Event;
        use crate::layer::Layer;
        let trans = make_trans("(instance Fido Dog)");
        let sid = root_of(&trans);
        let _ = trans.formula_fof(sid);
        assert!(trans.formulas_fof.peek(&sid).is_some());
        let dog = trans.semantic.syntactic.sym_id("Dog").unwrap();
        trans.cascade(vec![Event::TaxonomyChanged { syms: vec![dog] }]);
        assert!(
            trans.formulas_fof.peek(&sid).is_none(),
            "formulas_fof cache should be cleared after taxonomy change"
        );
    }

    #[test]
    fn formula_fof_cache_cleared_on_domain_range_change() {
        use crate::cache::events::Event;
        use crate::layer::Layer;
        let trans = make_trans("(instance Fido Dog)");
        let sid = root_of(&trans);
        let _ = trans.formula_fof(sid);
        assert!(trans.formulas_fof.peek(&sid).is_some());
        let dog = trans.semantic.syntactic.sym_id("Dog").unwrap();
        trans.cascade(vec![Event::DomainRangeChanged { syms: vec![dog] }]);
        assert!(
            trans.formulas_fof.peek(&sid).is_none(),
            "formulas_fof cache should be cleared after domain/range change"
        );
    }

    #[test]
    fn formula_fof_disabled_still_returns_value() {
        use crate::cache::CacheConfig;

        let cfg = CacheConfig::with_disabled(&["translation::formulas_fof"]);
        let mut store = SyntacticLayer::default();
        store.load_kif("(instance Fido Dog)", "test");
        let sem = SemanticLayer::new(store);
        let trans = TranslationLayer::with_config(sem, &cfg);

        let sid = root_of(&trans);
        // formula_fof should still return a value (on-the-fly).
        let cf = trans.formula_fof(sid);
        assert!(cf.is_some(),
            "formula_fof must return a value even when the cache is disabled");
        // But nothing should have been stored.
        assert!(trans.formulas_fof.peek(&sid).is_none(),
            "cache should remain empty when formulas_fof is disabled");
    }

    // -------------------------------------------------------------------------
    // prime_formula_cache
    // -------------------------------------------------------------------------

    #[test]
    fn prime_formula_cache_warms_both_caches() {
        let trans = make_trans("(instance Fido Dog)\n(subclass Dog Animal)");
        trans.prime_formula_cache().unwrap();
        for &sid in &roots_of(&trans) {
            assert!(trans.formulas_tff.peek(&sid).is_some(),
                "formulas_tff should be populated for sid {}", sid);
            assert!(trans.formulas_fof.peek(&sid).is_some(),
                "formulas_fof should be populated for sid {}", sid);
        }
    }

    // -------------------------------------------------------------------------
    // build_problem
    // -------------------------------------------------------------------------

    #[test]
    fn build_problem_tff_axiom_count() {
        let trans = make_trans("(instance Fido Dog)\n(subclass Dog Animal)");
        let sids: Vec<SentenceId> = roots_of(&trans);
        let (problem, sid_map) = trans.build_problem(&sids, TptpLang::Tff);
        assert_eq!(problem.axioms().len(), 2,
            "build_problem should produce one axiom per convertible SID");
        assert_eq!(problem.axioms().len(), sid_map.len(),
            "sid_map must be parallel to axioms");
    }

    #[test]
    fn build_problem_fof_axiom_count() {
        let trans = make_trans("(instance Fido Dog)\n(subclass Dog Animal)");
        let sids: Vec<SentenceId> = roots_of(&trans);
        let (problem, sid_map) = trans.build_problem(&sids, TptpLang::Fof);
        assert_eq!(problem.axioms().len(), 2,
            "build_problem FOF should produce one axiom per convertible SID");
        assert_eq!(problem.axioms().len(), sid_map.len(),
            "sid_map must be parallel to axioms");
    }

    #[test]
    fn build_problem_tff_deduplicates_declarations() {
        // Two axioms that both use `instance` should produce exactly one
        // `instance` declaration in the TFF preamble.
        let trans = make_trans("(instance Fido Dog)\n(instance Rex Cat)");
        let sids: Vec<SentenceId> = roots_of(&trans);
        let (problem, _) = trans.build_problem(&sids, TptpLang::Tff);

        let instance_decls = problem
            .pred_decls()
            .iter()
            .filter(|p| p.name().contains("instance"))
            .count();
        assert_eq!(instance_decls, 1,
            "shared predicate 'instance' should appear exactly once in TFF preamble");
    }

    #[test]
    fn build_problem_skips_suppressed() {
        let kif = "(=> (instance ?X PositiveInteger) (greaterThan ?X 0))\n\
                   (=> (instance ?Y PositiveInteger) (SomePred ?Y))";
        let mut store = SyntacticLayer::default();
        store.load_kif(kif, "test");
        let sem = SemanticLayer::new(store);
        let mut trans = TranslationLayer::new(sem);

        let pos_int_id = trans.semantic.syntactic.sym_id("PositiveInteger").unwrap();
        trans.numeric_sorts.update(pos_int_id, crate::trans::Sort::Integer);
        crate::trans::rewrite::run_rewrite_pass(
            &trans.numeric_sorts,
            &mut trans.suppressed.write().unwrap(),
            &mut trans.semantic.syntactic,
        );

        // Include ALL roots — suppressed ones should be silently skipped.
        let sids: Vec<SentenceId> = roots_of(&trans);
        let (problem, sid_map) = trans.build_problem(&sids, TptpLang::Tff);

        let suppressed_count = trans.suppressed.read().unwrap().len();
        let total_roots = sids.len();
        // The problem should contain fewer axioms than total roots.
        assert!(
            problem.axioms().len() <= total_roots - suppressed_count,
            "suppressed sentences must not appear in build_problem output: \
             roots={} suppressed={} axioms={}",
            total_roots, suppressed_count, problem.axioms().len()
        );
        assert_eq!(problem.axioms().len(), sid_map.len(),
            "sid_map must be parallel to axioms");
    }
}
