//! `semantic::validate` cache: memoises the semantic-validation result of each
//! sentence — the `Vec<SemanticError>` (warnings and hard errors alike) the
//! validator raises for it, empty meaning "valid".
//!
//! Lazy by key: `generate(sid)` runs the validator over that one sentence.  It
//! is also driven explicitly by `ValidateSentence { sid }` (one sentence) and
//! `ValidateKB` (every root sentence).  Validation reads the whole KB
//! (taxonomy, domains, arities), so any change to the sentence set invalidates
//! everything.  Emits no events.

use std::sync::Arc;

use crate::SentenceId;
use crate::cache::{CacheBehavior, EntryCache};
use crate::cache::events::{Event, EventKind};
use crate::semantics::SemanticLayer;
use crate::semantics::errors::SemanticError;
use crate::semantics::types::{Scope, Scoped};
use crate::syntactic::caches::session::session_id;

/// Behavior for the `semantic::validate` cache.
#[derive(Debug, Default)]
pub(crate) struct Validate;

impl CacheBehavior for Validate {
    type Parent = SemanticLayer;
    type Key    = Scoped<SentenceId>;
    type Value  = Arc<Vec<SemanticError>>;
    type Side = ();
    type SideSnapshot = ();

    const NAME: &'static str = "semantic::validate";

    /// Compute-on-miss: validate `sid` in `scope`.
    fn generate(&self, parent: &SemanticLayer, &Scoped { scope, key: sid }: &Scoped<SentenceId>) -> Arc<Vec<SemanticError>> {
        Arc::new(parent.validator_scoped(scope).validate_sentence_collect(sid))
    }

    fn consumes(&self) -> &'static [EventKind] {
        &[
            EventKind::ValidateKB,
            EventKind::ValidateSentence,
            EventKind::ValidateSession,
            EventKind::RootAdded,
            EventKind::RootRemoved,
            EventKind::TaxonomyChanged,
            EventKind::DomainRangeChanged,
            EventKind::SessionReferenced,
            EventKind::SessionRetracted,
        ]
    }

    fn reads(&self) -> &'static [&'static str] {
        &[
            "semantic::is_class", "semantic::is_instance", "semantic::is_relation",
            "semantic::is_predicate", "semantic::is_function",
            "semantic::has_ancestor", "semantic::arity",
            "semantic::domain", "semantic::range",
            "syntactic::sentences", "syntactic::sessions",
        ]
    }

    fn react(
        &self,
        parent: &SemanticLayer,
        events: &[&Event],
        store:  &EntryCache<Scoped<SentenceId>, Arc<Vec<SemanticError>>>,
        _side:   &Self::Side,
    ) -> Vec<Event> {
        // Whole-KB churn invalidates every scope's verdicts.  Clear first so a
        // `Validate*` request in the same batch recomputes against the new state.
        if events.iter().any(|e| matches!(e,
            Event::RootAdded { .. } | Event::RootRemoved { .. }
            | Event::TaxonomyChanged { .. } | Event::DomainRangeChanged { .. }))
        {
            store.clear();
        }
        for event in events {
            match event {
                Event::SessionReferenced { session, .. } | Event::SessionRetracted { session } => {
                    let s = Scope::Session(session_id(session));
                    store.retain(|k, _| k.scope != s);
                }
                Event::ValidateSentence { sid } => {
                    let errs = parent.validator_scoped(Scope::Base).validate_sentence_collect(*sid);
                    store.update(Scoped { scope: Scope::Base, key: *sid }, Arc::new(errs));
                }
                Event::ValidateKB => {
                    let roots: Vec<SentenceId> = parent.syntactic.root_sids();
                    let v = parent.validator_scoped(Scope::Base);
                    for sid in roots {
                        store.update(Scoped { scope: Scope::Base, key: sid }, Arc::new(v.validate_sentence_collect(sid)));
                    }
                }
                // Keyed by session scope so it never clobbers the Base entry.
                Event::ValidateSession { session } => {
                    let scope = Scope::Session(session_id(session));
                    let v = parent.validator_scoped(scope);
                    for sid in parent.syntactic.sessions.session_sentences(session) {
                        store.update(Scoped { scope, key: sid }, Arc::new(v.validate_sentence_collect(sid)));
                    }
                }
                _ => {}
            }
        }
        Vec::new()
    }
}

impl SemanticLayer {
    /// The semantic-validation result for sentence `sid` in an explicit
    /// [`Scope`] — every `SemanticError` the validator raises, empty meaning
    /// valid — read through the `semantic::validate` cache (compute-on-miss via
    /// the validator engine).
    ///
    /// Memoised only when the `semantic::validate` cache is enabled; it is off
    /// by default, in which case each call recomputes.
    pub(crate) fn validation_scoped(&self, sid: SentenceId, scope: Scope) -> Arc<Vec<SemanticError>> {
        self.validate.get(self, Scoped { scope, key: sid })
    }
}

#[cfg(test)]
mod tests {
    use crate::cache::{CacheConfig, events::Event};
    use crate::layer::Layer;
    use crate::semantics::SemanticLayer;
    use crate::semantics::types::{Scope, Scoped};
    use crate::syntactic::SyntacticLayer;

    /// `Base`-scoped key helper for the now-`Scoped<SentenceId>`-keyed cache.
    fn base(sid: crate::SentenceId) -> Scoped<crate::SentenceId> {
        Scoped { scope: Scope::Base, key: sid }
    }

    /// Build a layer with the `validate` cache **enabled** (`SemanticLayer::new`
    /// disables it by default).  `CacheConfig::default()` leaves every cache on.
    fn layer(kif: &str) -> SemanticLayer {
        let mut store = SyntacticLayer::default();
        store.load_kif(kif, "t");
        SemanticLayer::with_config(store, &CacheConfig::default())
    }

    #[test]
    fn validation_memoises_per_sentence() {
        // `(Foo Bar Baz)` is headed by an undeclared relation → a warning-level
        // `HeadNotRelation`, which `validate_sentence_collect` surfaces.
        let layer = layer(r#"
            (subclass Foo Entity)
            (Foo Bar Baz)
        "#);
        let sid = *layer.syntactic.by_head("Foo").iter().next().unwrap();
        let errs = layer.validation_scoped(sid, Scope::Base);
        assert!(!errs.is_empty(), "undeclared-relation head should raise a diagnostic");
        // Cached after first access (under the Base scope).
        assert!(layer.validate.peek(&base(sid)).is_some(),
            "validation cache should be populated after first access");
    }

    #[test]
    fn validate_sentence_event_stores_result() {
        let layer = layer(r#"
            (subclass Foo Entity)
            (Foo Bar Baz)
        "#);
        let sid = *layer.syntactic.by_head("Foo").iter().next().unwrap();
        assert!(layer.validate.peek(&base(sid)).is_none(), "not validated yet");

        layer.cascade(vec![Event::ValidateSentence { sid }]);
        assert!(layer.validate.peek(&base(sid)).is_some(),
            "ValidateSentence should have memoised the result");
    }

    #[test]
    fn validate_kb_event_validates_all_roots() {
        let layer = layer(r#"
            (subclass Animal Entity)
            (Foo Bar Baz)
        "#);
        layer.cascade(vec![Event::ValidateKB]);
        for sid in layer.syntactic.root_sids().into_iter() {
            assert!(layer.validate.peek(&base(sid)).is_some(),
                "ValidateKB should validate every root sentence");
        }
    }

    #[test]
    fn root_change_clears_validation() {
        let layer = layer("(subclass Animal Entity)");
        let sid = layer.syntactic.root_sids().into_iter().next().unwrap();
        let _ = layer.validation_scoped(sid, Scope::Base);
        assert!(layer.validate.peek(&base(sid)).is_some());

        // Any sentence add/remove drops the whole cache.
        layer.cascade(vec![Event::RootAdded { sid: 12345 }]);
        assert!(layer.validate.peek(&base(sid)).is_none(),
            "RootAdded should clear the validation cache");
    }
}
