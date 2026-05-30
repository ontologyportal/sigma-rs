// crates/core/src/saturate/caches/clause_store.rs
//
// `saturate::clause_store` cache: root SentenceId -> its canonical
// clauses, computed on miss by the native clausifier.
//
// Lazy by design: only roots a problem actually selects (SInE subset +
// session hypotheses + conjecture) ever pay clausification; a SUMO-scale
// load clausifies nothing up front.  Skolem names are deterministic per
// root (`sk_<root_hex>_<n>`), so evict-and-regenerate is invisible —
// the regenerated clauses are byte-identical, which is what makes plain
// eviction a sufficient retraction story.

use std::sync::Arc;

use crate::cache::{CacheBehavior, EntryCache};
use crate::cache::events::{Event, EventKind};
use super::super::ProverLayer;
use super::super::clause::PClause;
use super::super::clausify::clausify_sentence;
use crate::types::SentenceId;

/// Behavior for the `saturate::clause_store` cache.
#[derive(Debug, Default)]
pub(crate) struct ClauseStore;

impl CacheBehavior for ClauseStore {
    type Parent = ProverLayer;
    type Key    = SentenceId;
    /// `Arc` so a problem assembling thousands of background clauses
    /// bumps refcounts instead of deep-copying clause vectors.
    type Value  = Arc<Vec<PClause>>;
    type Side   = ();
    type SideSnapshot = ();

    const NAME: &'static str = "saturate::clause_store";

    /// Clausify the root on miss.  Interning atoms into the layer's
    /// `AtomTable` is an idempotent (content-addressed) side effect, so
    /// the storage layer re-calling `generate` under contention is benign.
    fn generate(&self, parent: &ProverLayer, root: &SentenceId) -> Arc<Vec<PClause>> {
        let syn = &parent.semantic.syntactic;
        let Some(sent) = syn.sentence(*root) else { return Arc::new(Vec::new()) };
        Arc::new(clausify_sentence(syn, &parent.atoms, &sent, *root, false))
    }

    fn consumes(&self) -> &'static [EventKind] {
        &[EventKind::RootRemoved]
    }

    fn reads(&self) -> &'static [&'static str] {
        &["syntactic::sentences"]
    }

    fn react(
        &self,
        _parent: &ProverLayer,
        events:  &[&Event],
        store:   &EntryCache<SentenceId, Arc<Vec<PClause>>>,
        _side:   &Self::Side,
    ) -> Vec<Event> {
        // A retracted root's clauses are stale — evict; the next problem
        // that wants the root (it won't) would regenerate from the store.
        // Atoms interned by evicted clauses stay in the AtomTable: they
        // are content-addressed and side-effect-free, so a stale atom is
        // unreachable garbage, not a correctness hazard.  (A sweep can
        // land with the prover loop if memory ever warrants it.)
        let evict: Vec<SentenceId> = events
            .iter()
            .filter_map(|e| match e {
                Event::RootRemoved { sid, .. } => Some(*sid),
                _ => None,
            })
            .collect();
        if !evict.is_empty() {
            store.evict_keys(&evict);
        }
        Vec::new()
    }
}
