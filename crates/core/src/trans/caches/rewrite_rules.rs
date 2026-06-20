// crates/core/src/trans/caches/rewrite_rules.rs
//
// `translation::rewrite_rules` — the extracted rewrite *program* (Case-1/Case-2
// rules + predicate-variable schemas + derived suppression) for the current
// implication set.
//
// A lazy whole-value cache.  Its `generate` is a PURE read-only scan
// (delegating to [`TranslationLayer::build_rewrite_program`]), so the cache's
// compute-on-miss gives the "run once at first read" deferral the retired
// `rewrite_dirty` flag hand-rolled, and `react` invalidates wholesale when the
// implication population or numeric-class membership shifts.
//
// This cache holds only the PURE half of the rewrite pass.  Rule *application*
// (guard injection + augmentation) is side-effecting and per-query predicate-
// variable *instantiation* is problem-scoped; both stay outside.  No consumer
// is wired to this cache yet — it is the foundation for that work.

use std::sync::Arc;

use crate::cache::events::{Event, EventKind};
use crate::cache::{LayerCache, WholeCacheBehavior};
use crate::trans::TranslationLayer;
use crate::trans::rewrite::RewriteProgram;

/// Behavior for the `translation::rewrite_rules` cache.
#[derive(Debug, Default)]
pub(crate) struct RewriteRulesCache;

impl WholeCacheBehavior for RewriteRulesCache {
    type Parent = TranslationLayer;
    type Value  = Arc<RewriteProgram>;

    const NAME: &'static str = "translation::rewrite_rules";

    fn generate(&self, parent: &TranslationLayer) -> Arc<RewriteProgram> {
        Arc::new(parent.build_rewrite_program())
    }

    // Extraction is a function of the implication population (root add/remove)
    // and numeric-class membership (`numeric_sorts`, which shifts on taxonomy
    // changes).  It does NOT read domain/range — that feeds guard injection, not
    // rule extraction — so `DomainRangeChanged` is deliberately absent.
    fn consumes(&self) -> &'static [EventKind] {
        &[EventKind::TaxonomyChanged, EventKind::RootAdded, EventKind::RootRemoved]
    }

    fn reads(&self) -> &'static [&'static str] {
        &["syntactic::sentences", "translation::numeric_sorts"]
    }

    /// Whole-value cache built by a global extraction scan with no reverse
    /// index, so any relevant change invalidates wholesale; the lazy `generate`
    /// rebuilds on the next read.  Emits no follow-on events.
    fn react(
        &self,
        _parent: &TranslationLayer,
        events:  &[&Event],
        store:   &LayerCache<Arc<RewriteProgram>>,
    ) -> Vec<Event> {
        let relevant = events.iter().any(|e| matches!(
            e,
            Event::TaxonomyChanged { .. } | Event::RootAdded { .. } | Event::RootRemoved { .. }
        ));
        if relevant {
            store.invalidate();
        }
        Vec::new()
    }
}

impl TranslationLayer {
    /// The extracted [`RewriteProgram`] for the current KB, building it on first
    /// access and caching the `Arc`.  Consumed by `instantiate_predvars` as
    /// the schema source (its templates instantiate per problem).
    pub(crate) fn rewrite_program(&self) -> Arc<RewriteProgram> {
        self.rewrite_rules.get(self)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use crate::semantics::SemanticLayer;
    use crate::syntactic::SyntacticLayer;
    use crate::trans::TranslationLayer;
    use crate::trans::rewrite::{
        detect_predvar_schemas, extract_case1_rules, extract_case2_rules, RewriteRule,
    };
    use crate::types::{SentenceId, SymbolId};

    fn make_trans(kif: &str) -> TranslationLayer {
        let mut store = SyntacticLayer::default();
        let errors = store.load_kif(kif, "test");
        assert!(errors.is_empty(), "load errors: {:?}", errors);
        let sem = SemanticLayer::new(store);
        TranslationLayer::new(sem)
    }

    /// Project rules to a comparable, order-independent key (the pattern has no
    /// `PartialEq`, so compare the identifying fields).
    fn proj(rules: &[RewriteRule]) -> Vec<(SentenceId, SymbolId, SentenceId)> {
        let mut v: Vec<(SentenceId, SymbolId, SentenceId)> =
            rules.iter().map(|r| (r.source_sid, r.template_var, r.consequent_sid)).collect();
        v.sort();
        v
    }

    #[test]
    fn extracts_case1_numeric_rule() {
        let trans = make_trans(
            "(subclass PositiveInteger Integer)\n\
             (=> (instance ?X PositiveInteger) (greaterThan ?X 0))",
        );
        // Precondition: the subclass edge makes PositiveInteger numeric-sorted,
        // which is what gates Case-1 extraction.
        let pi = trans.semantic.syntactic.sym_id("PositiveInteger").unwrap();
        assert!(
            trans.numeric_sorts.get(&pi).is_some(),
            "PositiveInteger should be Integer-sorted via (subclass PositiveInteger Integer)",
        );

        let prog = trans.rewrite_program();
        assert_eq!(prog.rules.len(), 1, "expected exactly one Case-1 rule");
        assert!(
            prog.suppressed_sources.contains(&prog.rules[0].source_sid),
            "the rule's source implication must be suppressed",
        );
    }

    #[test]
    fn extracts_case2_predicate_rule() {
        let trans = make_trans("(=> (instance ?REL SymmetricRelation) (?REL ?X ?Y))");
        // SymmetricRelation is not numeric, so this is Case-2, not Case-1.
        let sr = trans.semantic.syntactic.sym_id("SymmetricRelation").unwrap();
        assert!(trans.numeric_sorts.get(&sr).is_none());

        let prog = trans.rewrite_program();
        assert_eq!(prog.rules.len(), 1, "expected exactly one Case-2 rule");
    }

    #[test]
    fn rules_match_standalone_extraction() {
        let trans = make_trans(
            "(subclass PositiveInteger Integer)\n\
             (=> (instance ?X PositiveInteger) (greaterThan ?X 0))\n\
             (=> (instance ?REL SymmetricRelation) (?REL ?X ?Y))",
        );
        let prog = trans.rewrite_program();

        // Re-run the extraction independently and compare — the cache must be a
        // faithful, deterministic mirror of Case-1 + offset-Case-2.
        let syntactic = &trans.semantic.syntactic;
        let impls = syntactic.normal_implications();
        let mut expect = extract_case1_rules(&trans.numeric_sorts, syntactic, &impls);
        let n = expect.len();
        expect.extend(
            extract_case2_rules(&trans.numeric_sorts, syntactic, &impls)
                .into_iter()
                .map(|mut r| {
                    r.id += n;
                    r
                }),
        );

        assert_eq!(
            proj(&prog.rules),
            proj(&expect),
            "cache rules must equal a fresh extraction over the same implications",
        );
        assert_eq!(prog.rules.len(), 2, "one Case-1 + one Case-2");
    }

    #[test]
    fn suppressed_sources_are_rule_and_schema_sources() {
        // The transitivity axiom is BOTH a Case-2 rule and a predvar schema, so
        // its sid contributes to suppressed_sources from both extractors (deduped
        // by the set); the numeric axiom contributes its own.
        let trans = make_trans(
            "(subclass PositiveInteger Integer)\n\
             (=> (instance ?X PositiveInteger) (greaterThan ?X 0))\n\
             (=> (instance ?REL TransitiveRelation) \
                 (=> (and (?REL ?X ?Y) (?REL ?Y ?Z)) (?REL ?X ?Z)))",
        );
        let prog = trans.rewrite_program();

        // Derived suppression == rule sources ∪ schema sources (the
        // synthetic_origin closure is empty today — that map is a dead stub).
        let mut expected: HashSet<SentenceId> = HashSet::new();
        for r in &prog.rules {
            expected.insert(r.source_sid);
        }
        for s in &prog.predvar_schemas {
            expected.insert(s.schema_sid);
        }
        assert_eq!(
            prog.suppressed_sources, expected,
            "suppressed_sources must be exactly the union of rule and schema sources",
        );
        assert!(!prog.suppressed_sources.is_empty());
    }

    #[test]
    fn predvar_schemas_match_standalone_detection() {
        let trans = make_trans(
            "(=> (instance ?REL TransitiveRelation) \
                 (=> (and (?REL ?X ?Y) (?REL ?Y ?Z)) (?REL ?X ?Z)))",
        );
        let prog = trans.rewrite_program();

        let syntactic = &trans.semantic.syntactic;
        let impls = syntactic.normal_implications();
        let expect = detect_predvar_schemas(syntactic, &impls);

        assert_eq!(prog.predvar_schemas.len(), expect.len());
        assert_eq!(prog.predvar_schemas.len(), 1, "transitivity yields one schema");

        let mut a: Vec<SentenceId> = prog.predvar_schemas.iter().map(|s| s.schema_sid).collect();
        let mut b: Vec<SentenceId> = expect.iter().map(|s| s.schema_sid).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b);
    }

    #[test]
    fn program_is_memoized_and_pure() {
        let trans = make_trans(
            "(subclass PositiveInteger Integer)\n\
             (=> (instance ?X PositiveInteger) (greaterThan ?X 0))",
        );
        let a = trans.rewrite_program();
        let b = trans.rewrite_program();
        assert!(Arc::ptr_eq(&a, &b), "second get must return the memoized Arc");

        // generate is deterministic (pure): a fresh, uncached build matches.
        let fresh = trans.build_rewrite_program();
        assert_eq!(proj(&fresh.rules), proj(&a.rules));
        assert_eq!(fresh.suppressed_sources, a.suppressed_sources);
    }

    #[test]
    fn invalidates_on_taxonomy_change() {
        use crate::cache::events::Event;
        use crate::layer::Layer;

        let trans = make_trans(
            "(subclass PositiveInteger Integer)\n\
             (=> (instance ?X PositiveInteger) (greaterThan ?X 0))",
        );
        let _ = trans.rewrite_program();
        assert!(trans.rewrite_rules.is_populated());

        let pi = trans.semantic.syntactic.sym_id("PositiveInteger").unwrap();
        trans.cascade(vec![Event::TaxonomyChanged { syms: vec![pi] }]);

        assert!(
            !trans.rewrite_rules.is_populated(),
            "rewrite_rules must invalidate on a taxonomy change",
        );
    }
}
