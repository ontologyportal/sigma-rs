// crates/core/src/saturate/caches/model_registry.rs
//
// `saturate::model_registry` cache (Phase 5, slice 1): the whole-KB
// `ModelProgram` — extracted Datalog program + role schemas + cluster
// partition + monotone fragment — computed once and held for the KB's life.
//
// A `WholeCache` (singleton, not per-key): the registry is derived from the
// entire root set, so it is one value, invalidated wholesale whenever a root
// is added or removed (the reactor drops it; the next `get` rebuilds).  Build
// is cheap — extraction + partition only, no model evaluation — so a lazy
// rebuild-on-edit is sufficient; differential maintenance is a later phase.
//
// This slice wires the cache only; the prover does not yet consult it (so the
// saturation path is byte-identical).  The query path (decide / retrieve) is
// the next slice.

use std::marker::PhantomData;
use std::sync::Arc;

use super::super::model::ModelProgram;
use super::super::ProverLayer;
use crate::cache::events::{Event, EventKind};
use crate::cache::{LayerCache, WholeCacheBehavior};
use crate::layer::TopLayer;

/// Behavior for the `saturate::model_registry` whole-KB cache.  Carries `S`
/// only as a marker — see [`ClauseStore`](super::clause_store::ClauseStore)'s
/// doc for why.
#[derive(Debug)]
pub(crate) struct ModelRegistry<S = crate::semantics::SemanticLayer>(PhantomData<fn() -> S>);

impl<S> Default for ModelRegistry<S> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<S: TopLayer + 'static> WholeCacheBehavior for ModelRegistry<S> {
    type Parent = ProverLayer<S>;
    /// `Arc` so consulting the registry per query bumps a refcount instead of
    /// cloning the whole program.
    type Value = Arc<ModelProgram>;

    const NAME: &'static str = "saturate::model_registry";

    fn generate(&self, parent: &ProverLayer<S>) -> Arc<ModelProgram> {
        Arc::new(ModelProgram::build(&parent.semantic.semantic().syntactic))
    }

    fn consumes(&self) -> &'static [EventKind] {
        &[EventKind::RootAdded, EventKind::RootRemoved]
    }

    fn reads(&self) -> &'static [&'static str] {
        &["syntactic::sentences"]
    }

    /// Any root change invalidates the whole program (it is derived from the
    /// full root set); the next `get` rebuilds.
    fn react(
        &self,
        _parent: &ProverLayer<S>,
        events: &[&Event],
        store: &LayerCache<Arc<ModelProgram>>,
    ) -> Vec<Event> {
        let changed = events
            .iter()
            .any(|e| matches!(e, Event::RootAdded { .. } | Event::RootRemoved { .. }));
        if changed {
            store.invalidate();
        }
        Vec::new()
    }
}

impl<S: TopLayer + 'static> ProverLayer<S> {
    /// The whole-KB inductive-definition model program, built on
    /// first request and held until a root changes.
    pub(crate) fn model_program(&self) -> Arc<ModelProgram> {
        self.model_program.get(self)
    }
}
