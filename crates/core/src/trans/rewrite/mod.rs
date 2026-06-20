// crates/core/src/trans/rewrite.rs
//
// Formula rewriting pass for the TranslationLayer.
//
// This module implements the two stages that live above `SyntacticLayer`:
//
//   Stage 2 — Rule Extraction
//     Pattern-match `normal_implications` to find class characterization
//     sentences and build `RewriteRule`s.  Two kinds of rule are extracted:
//
//     Case 1 (`extract_case1_rules`):
//       Numeric subclass characterizations — `(=> (instance ?X NumericClass) Arith…)`
//       where the consequent contains arithmetic predicates.
//
//     Case 2 (`extract_case2_rules`):
//       Predicate variable characterizations — `(=> (instance ?REL C) …?REL…)`
//       where the variable `?REL` appears in predicate/head position somewhere
//       in the consequent (e.g. `(?REL ?X ?Y)` or `(holds ?REL ?X ?Y)`).
//       Covers relation-class axioms such as symmetry, transitivity, etc.
//
//   Stage 3 — Augmentation Fixed-Point  (`augment_fixed_point`)
//     Apply each rule to every implication in the KB, adding derived
//     conjuncts to antecedents that contain a matching `(instance ?X C)` atom.
//
// The normalization stage (Stage 1) lives in `syntactic/normalize.rs`.


use crate::trans::TranslationLayer;

mod preprocess;
mod extract;
mod augment;
mod predvar;
#[cfg(test)]
mod tests;

// Re-exports used by the driver below and by other modules
// (`trans::rewrite::X`): the rest stays private to its stage file.
use preprocess::inject_domain_guards;
pub(crate) use extract::{RewriteRule, extract_case1_rules, extract_case2_rules};
use augment::augment_fixed_point;
pub(crate) use predvar::{PredVarSchema, detect_predvar_schemas, is_bare_positive_assertion};

// `HashSet` / `SentenceId` are used by `build_rewrite_program` below (and the
// `#[cfg(test)]` `run_rewrite_pass` driver).  `SyntacticLayer` is named only by
// the test driver.
use std::collections::HashSet;
use crate::types::SentenceId;
#[cfg(test)]
use crate::syntactic::SyntacticLayer;

/// The pure, cacheable output of rewrite-rule extraction over the current
/// (CAF-normalized) implication set: the Case-1/Case-2 [`RewriteRule`]s, the
/// predicate-variable [`PredVarSchema`]s, and the source implication ids they
/// make redundant (derived suppression).  Produced by
/// [`TranslationLayer::build_rewrite_program`] and held by the
/// `translation::rewrite_rules` cache.  Pure data — building it touches neither
/// the sentence store nor the imperative `suppressed` set.
#[derive(Debug, Clone, Default)]
pub(crate) struct RewriteProgram {
    /// Case-1 (numeric subclass) rules followed by Case-2 (predicate-variable)
    /// rules, with Case-2 ids offset past the Case-1 block (as in
    /// [`TranslationLayer::run_rewrite_pass`]).
    pub rules: Vec<RewriteRule>,
    /// Predicate-variable schema templates (transitivity / symmetry /
    /// subrelation-propagation); instantiated lazily per query elsewhere.
    pub predvar_schemas: Vec<PredVarSchema>,
    /// Source implication ids (rule sources ∪ schema sources, plus any
    /// `synthetic_origin` they derive from) whose rewritten / instantiated forms
    /// stand in for them, so the originals must be suppressed from plain TPTP.
    pub suppressed_sources: HashSet<SentenceId>,
}

impl TranslationLayer {
    /// Case-1 (numeric subclass) rules followed by Case-2 (predicate-variable)
    /// rules, with Case-2 ids offset past the Case-1 block so the
    /// `(SentenceId, rule_id)` applied-pair set in `augment_fixed_point` never
    /// confuses the two.  The ONE extraction used by both the imperative
    /// [`Self::run_rewrite_pass`] and the pure [`Self::build_rewrite_program`].
    fn extract_rules(&self, implications: &[SentenceId]) -> Vec<RewriteRule> {
        let syntactic = &self.semantic.syntactic;
        let mut rules = extract_case1_rules(&self.numeric_sorts, syntactic, implications);
        let case1_count = rules.len();
        rules.extend(
            extract_case2_rules(&self.numeric_sorts, syntactic, implications)
                .into_iter()
                .map(|mut r| {
                    r.id += case1_count;
                    r
                }),
        );
        rules
    }

    /// Build the [`RewriteProgram`] from the current implication set — a PURE,
    /// read-only scan (no store mutation, no `suppressed` writes), suitable as
    /// the `generate` body of the `translation::rewrite_rules` cache.
    ///
    /// Mirrors the *extraction* half of [`Self::run_rewrite_pass`] (Case-1 then
    /// Case-2 with the same id offset) plus predicate-variable schema detection,
    /// and derives the suppression set declaratively instead of mutating
    /// `self.suppressed`.  Guard injection (Stage 2a) and augmentation (Stage 3)
    /// are intentionally NOT done here — they are side-effecting and remain an
    /// explicit pass; this captures only what is a pure function of the stored
    /// implications.
    pub(crate) fn build_rewrite_program(&self) -> RewriteProgram {
        let syntactic = &self.semantic.syntactic;
        let implications = syntactic.normal_implications();

        let rules = self.extract_rules(&implications);

        let predvar_schemas = detect_predvar_schemas(syntactic, &implications);

        // Derived suppression: each characterization implication that becomes a
        // rule (and each predvar schema) is replaced by its rewritten /
        // instantiated form, so the original must not also emit as a plain axiom.
        // Mirror the imperative pass's source + synthetic-origin closure
        // (`mod.rs` rule-source loop, `predvar.rs` schema loop).  `synthetic_origin`
        // is an empty stub today, so the closure is a no-op, but we compute it for
        // forward-compatibility with revived provenance tracking.
        let mut suppressed_sources: HashSet<SentenceId> = HashSet::new();
        for r in &rules {
            suppressed_sources.insert(r.source_sid);
            if let Some(origin) = syntactic.synthetic_origin.get(&r.source_sid).copied() {
                suppressed_sources.insert(origin);
            }
        }
        for s in &predvar_schemas {
            suppressed_sources.insert(s.schema_sid);
            if let Some(origin) = syntactic.synthetic_origin.get(&s.schema_sid).copied() {
                suppressed_sources.insert(origin);
            }
        }

        RewriteProgram { rules, predvar_schemas, suppressed_sources }
    }

    /// Run the complete rewrite pass (Stage 2 + Stage 3).
    ///
    /// Uses split borrows so that `numeric_sorts` and `suppressed` (fields of
    /// `TranslationLayer`) can be accessed while `syntactic` (a field inside
    /// `TranslationLayer::semantic`) is borrowed mutably.
    ///
    /// Typical call from `TranslationLayer::on_change`:
    /// ```ignore
    /// run_rewrite_pass(
    ///     &self.numeric_sorts,
    ///     &mut self.suppressed,
    ///     &mut self.semantic.syntactic,
    /// );
    /// ```
    pub(crate) fn run_rewrite_pass(
        &self
    ) {
        // Stage 1 — normalization (lazy build).
        let implications = self.semantic.syntactic.normal_implications();

        // Stage 2a — preProcess / type-hypothesis injection (§D).
        // Walks each implication, looks up declared domain classes on every
        // predicate/function use of free variables, and injects synthetic
        // `(instance ?V C)` guards into the antecedent.  Suppresses the
        // original.  New synthetic implications are returned and prepended
        // to the implication seed so Case 1/2 rule extraction (Stage 2b)
        // sees the augmented antecedents — in particular the freshly-added
        // `(instance ?V Integer)` guards become matchable by Case 1 rules.
        let injected = inject_domain_guards(
            &self.semantic,
            &implications,
            &mut self.suppressed.write().unwrap(),
        );
        let mut implications = implications;
        implications.extend(injected.iter().copied());

        // Stage 2b — Case 1 + Case 2 rule extraction (shared with
        // `build_rewrite_program`).
        let rules = self.extract_rules(&implications);

        let syntactic = &self.semantic.syntactic;
        {
        let mut suppressed = self.suppressed.write().unwrap();
        for rule in &rules {
            suppressed.insert(rule.source_sid);
            // If the rule's source is a synthetic sentence (produced by
            // CAF normalization in `syntactic/normalize.rs`), also
            // suppress the original root it was derived from.  Without
            // this, biconditionals like
            //   (<=> (instance ?X NonnegativeRealNumber)
            //        (and (greaterOrEqual ?X 0) (instance ?X RealNumber)))
            // survive to the TPTP emitter: CAF splits them into two
            // synthetic implications, Case 1 matches the forward
            // direction and suppresses the *synthetic* sid, but the
            // unsplit original `<=>` axiom passes through.  Vampire
            // then sees `$greatereq(X0, 0)` with X0 quantified at `$i`
            // and rejects the file as ill-typed.
            if let Some(origin) = syntactic.synthetic_origin.get(&rule.source_sid).copied() {
                suppressed.insert(origin);
            }
        }
        }
        augment_fixed_point(syntactic, &rules, &implications, &mut self.suppressed.write().unwrap());

        // The pass changed the implication set (injected guards, suppressed
        // originals) without cascading events, so invalidate the reactive
        // rewrite program — `instantiate_predvars` re-detects schemas from it
        // on the next read — and drop the memoised instantiations.
        self.rewrite_rules.invalidate();
        self.predvar_cache.write().unwrap().clear();
    }

    /// Run the rewrite pass if it has been marked dirty by `on_change`.
    /// Idempotent — clears the dirty flag on entry so a self-recursive
    /// call (theoretically impossible, but cheap to guard) is a no-op.
    ///
    /// **Why deferred?**  `on_change` is fired once per file load and once
    /// per session-promotion, but each invocation walks every root in the
    /// growing KB.  For SUMO's 49-file incremental ingest that's
    /// O(files × roots) ≈ O(N²) without this guard — measured at 10+
    /// minutes on a release build.  Deferring to the first reader
    /// collapses the cost to a single O(N) sweep at first query time.
    pub(crate) fn ensure_rewrite_pass(&self) {
        if !self.rewrite_dirty.swap(false, std::sync::atomic::Ordering::Relaxed) { return; }
        self.run_rewrite_pass();
    }
}


/// Test-only driver: run the full rewrite pass over a bare `SyntacticLayer`.
#[cfg(test)]
pub(crate) fn run_rewrite_pass(
    numeric_sorts: &crate::cache::EagerMap<crate::trans::caches::numeric_sorts::NumericSorts>,
    suppressed:    &mut HashSet<SentenceId>,
    syntactic:     &mut SyntacticLayer,
) {
    let implications = syntactic.normal_implications();
    let mut rules = extract_case1_rules(numeric_sorts, syntactic, &implications);
    let case1_count = rules.len();
    rules.extend(
        extract_case2_rules(numeric_sorts, syntactic, &implications)
            .into_iter()
            .map(|mut r| { r.id += case1_count; r }),
    );
    for rule in &rules {
        suppressed.insert(rule.source_sid);
    }
    augment_fixed_point(syntactic, &rules, &implications, suppressed);
}
