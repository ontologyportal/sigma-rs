//! `syntactic::sentence_symbols` — the set of SymbolIds referenced by a
//! sentence (transitively through sub-sentences).
//!
//! Toggleable cache, disabled by default (see `SyntacticLayer::with_config`).
//! While disabled it behaves as a transparent getter: every call recomputes
//! via `generate` and stores nothing. When enabled it memoises per SentenceId,
//! and `react_to_delta` clears the whole store on any change, since callers may
//! key on sub-sentence ids that a per-id eviction would miss.

use std::collections::HashSet;

use crate::cache::{CacheBehavior};
use crate::syntactic::SyntacticLayer;
use crate::types::{SentenceId, SymbolId};

/// Behavior for the `syntactic::sentence_symbols` cache (default-disabled).
#[derive(Debug, Default)]
pub(crate) struct SentenceSymbols;

impl CacheBehavior for SentenceSymbols {
    type Parent = SyntacticLayer;
    type Key    = SentenceId;
    type Value  = HashSet<SymbolId>;
    type Side = ();
    type SideSnapshot = ();

    const NAME: &'static str = "syntactic::sentence_symbols";

    fn generate(&self, parent: &SyntacticLayer, &sid: &SentenceId) -> HashSet<SymbolId> {
        let mut out = HashSet::new();
        if parent.has_sentence(sid) {
            parent.collect_symbols(sid, &mut out);
        }
        out
    }
}

impl SyntacticLayer {
    /// The SymbolIds referenced by `sid` (transitively through sub-sentences).
    pub(crate) fn sentence_symbols(&self, sid: SentenceId) -> HashSet<SymbolId> {
        self.sentence_symbols.get(self, sid)
    }
}
