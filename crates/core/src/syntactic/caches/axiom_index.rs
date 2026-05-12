//! `syntactic::axiom_index` — the axiom-occurrence reverse index: for each
//! symbol, the set of axiom SentenceIds it appears in (transitively through
//! sub-sentences), de-duplicated per axiom.
//!
//! "Axiom" means a promoted root sentence; session assertions do not update
//! this index.  `axiom_sentences_of(sym).len()` is the symbol's SInE
//! generality.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::cache::{EagerMapBehavior};
use crate::cache::events::{EventKind, Event};
use crate::syntactic::SyntacticLayer;
use crate::syntactic::caches::sentences::SentenceCache;
use crate::syntactic::sentence::Sentence;
use crate::types::{Element, SentenceId, SymbolId};

/// Behavior for the `syntactic::axiom_index` eager keyed index.
#[derive(Debug, Default)]
pub(crate) struct AxiomIndex;

impl EagerMapBehavior for AxiomIndex {
    type Parent = SyntacticLayer;
    type Key    = SymbolId;
    type Value  = Arc<HashSet<SentenceId>>;
    type Side   = ();
    type SideSnapshot = ();

    const NAME: &'static str = "syntactic::axiom_index";

    fn consumes(&self) -> &'static [EventKind] {
        &[EventKind::AxiomsPromoted, EventKind::RootRemoved]
    }

    // Reads the sentence store, so the sentence reactor must run first.
    fn reads(&self) -> &'static [&'static str] {
        &[SentenceCache::NAME]
    }

    fn event_parallel(&self) -> bool { true }

    fn react(
        &self,
        parent: &Self::Parent,
        events: &[&crate::cache::events::Event],
        axiom_index:  &crate::cache::EntryCache<Self::Key, Self::Value>,
        _side:  &(),
    ) -> Vec<Event>
    {
        for event in events {
            match event {
                Event::AxiomsPromoted { sids } => {
                    for sid in sids {
                        let mut ids = parent.sentence_symbols(*sid);
                        ids.extend(parent.sentence_vars(*sid).into_iter().map(|(id, _)| id));
                        for s in ids {
                            axiom_index.modify_entry(s, |set| { Arc::make_mut(set).insert(*sid); });
                        }
                    }
                },
                Event::RootRemoved { sid, sentences } => {
                    let Some(root) = sentences.iter().find(|s| s.hash() == *sid) else { continue };
                    for s in parent.transitive_symbols_of(root, sentences) {
                        axiom_index.modify_entry(s, |set| { Arc::make_mut(set).remove(sid); });
                    }
                },
                _ => {}
            }
        }
        Vec::new()
    }
}

impl SyntacticLayer {
    /// A symbol's axiom-occurrence set (cloned; empty for unknown symbols).
    /// `axiom_sentences_of(sym).len()` is the symbol's SInE generality.
    pub(crate) fn axiom_sentences_of(&self, sym: SymbolId) -> Arc<HashSet<SentenceId>> {
        self.axiom_index.get(&sym).unwrap_or_default()
    }

    /// The transitive symbol set of a *removed* root, reconstructed from the
    /// bodies carried on `RootRemoved`.  `root` is the root body; for each
    /// `Element::Sub`, a removed sub is taken from `removed` and walked, while a
    /// sub that survived (shared by another root) is resolved from the live store
    /// via [`Self::sentence_symbols`].
    pub(crate) fn transitive_symbols_of(
        &self,
        root:    &Sentence,
        removed: &[Sentence],
    ) -> HashSet<SymbolId> {
        let by_id: HashMap<SentenceId, &Sentence> =
            removed.iter().map(|s| (s.hash(), s)).collect();
        let mut out:   HashSet<SymbolId>   = HashSet::new();
        let mut seen:  HashSet<SentenceId> = HashSet::new();
        let mut stack: Vec<&Sentence>      = vec![root];
        while let Some(s) = stack.pop() {
            for el in &s.elements {
                match el {
                    Element::Symbol(sym) => { out.insert(sym.id()); }
                    Element::Variable { id, is_row: false, .. } => { out.insert(*id); }
                    Element::Sub(c) => {
                        if !seen.insert(*c) { continue; }
                        match by_id.get(c) {
                            Some(sub) => stack.push(sub),
                            None      => {
                                out.extend(self.sentence_symbols(*c));
                                out.extend(self.sentence_vars(*c).into_iter().map(|(id, _)| id));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        out
    }
}