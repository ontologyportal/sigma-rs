// crates/core/src/syntactic/caches/sentence_vars.rs
//
// `syntactic::sentence_vars` — the set of (SymbolId, name) variable references
// in a sentence (transitively through sub-sentences).
//
// A *toggleable* cache, DISABLED BY DEFAULT (see `SyntacticLayer::with_config`):
// while disabled it is a transparent getter (recompute every call, store
// nothing).  See `caches::sentence_symbols` for the rationale and the
// enable-time `react_to_delta` caveat.

use std::collections::{HashMap};

use crate::cache::{CacheBehavior};
use crate::syntactic::SyntacticLayer;
use crate::types::{SentenceId, SymbolId};

/// Behavior for the `syntactic::sentence_vars` cache (default-disabled).
#[derive(Debug, Default)]
pub(crate) struct SentenceVars;

impl CacheBehavior for SentenceVars {
    type Parent = SyntacticLayer;
    type Key    = SentenceId;
    type Value  = HashMap<SymbolId, u32>;
    type Side = ();
    type SideSnapshot = ();

    const NAME: &'static str = "syntactic::sentence_vars";

    fn generate(&self, parent: &SyntacticLayer, &sid: &SentenceId) -> HashMap<SymbolId, u32> {
        let mut out = HashMap::new();
        if parent.has_sentence(sid) {
            parent.collect_vars(sid, &mut out);
        }
        out
    }

    // consumes/produces/reads/react/on_cycle etc.: trait defaults (no events,
    // no reads, panic on recursive entry) are exactly right for a pure
    // per-sentence recompute cache.
}

impl SyntacticLayer {
    /// The variables (id + name) referenced by `sid` (transitively).
    pub(crate) fn sentence_vars(&self, sid: SentenceId) -> HashMap<SymbolId, u32> {
        self.sentence_vars.get(self, sid)
    }
}
