// crates/core/src/syntactic/index.rs
//
// Occurrence + head + axiom-symbol indices for SyntacticLayer.

use std::collections::HashSet;

use crate::types::{Element, Occurrence, OccurrenceKind, SentenceId, SymbolId};

use super::SyntacticLayer;

impl SyntacticLayer {
    /// `true` iff any sentence in `sids` has a taxonomy-relation
    /// head (`subclass` / `instance` / `subrelation` / `subAttribute`).
    pub(crate) fn any_touches_taxonomy(&self, sids: &[SentenceId]) -> bool {
        use crate::semantics::taxonomy::TaxRelation;
        sids.iter().any(|&sid| {
            if !self.has_sentence(sid) { return false; }
            let sentence = &self.sentences[self.sent_idx(sid)];
            match sentence.head_symbol() {
                Some(head_id) => {
                    let name = self.sym_name(head_id);
                    TaxRelation::from_str(name).is_some()
                }
                None => false,
            }
        })
    }

    /// Walk `sid` and every sub-sentence it transitively reaches,
    /// recording each `Element::Symbol` in the reverse index.
    pub(crate) fn index_sentence_occurrences(&mut self, sid: SentenceId) {
        if !self.sent_idx.contains_key(&sid) { return; }
        let mut stack: Vec<SentenceId> = vec![sid];
        while let Some(cur) = stack.pop() {
            let vec_idx = self.sent_idx(cur);
            // Collect first to avoid holding `&self.sentences` across the
            // mutation of `self.occurrences`.
            let entries: Vec<(SymbolId, Occurrence)> = {
                let sentence = &self.sentences[vec_idx];
                sentence.elements.iter().enumerate().filter_map(|(i, el)| {
                    match el {
                        // Ordinary symbols: indexed by their stable id.
                        Element::Symbol { id, span } if !span.is_synthetic() => {
                            let kind = if i == 0 { OccurrenceKind::Head } else { OccurrenceKind::Arg };
                            Some((*id, Occurrence { sid: cur, idx: i, span: span.clone(), kind }))
                        }
                        // Variables: indexed by scope-qualified id so
                        // rename can enumerate every co-bound reference
                        // and respect quantifier scoping automatically.
                        Element::Variable { id, span, .. } if !span.is_synthetic() => {
                            Some((*id, Occurrence { sid: cur, idx: i, span: span.clone(),
                                                    kind: OccurrenceKind::Arg }))
                        }
                        Element::Sub { sid: sub, .. } => {
                            stack.push(*sub);
                            None
                        }
                        _ => None,
                    }
                }).collect()
            };
            for (id, occ) in entries {
                self.occurrences.entry(id).or_default().push(occ);
            }
        }
    }

    /// Drop all occurrence-index entries attached to `sid` (and
    /// every sub-sentence it transitively reaches via `Element::Sub`).
    pub(crate) fn drop_sentence_occurrences(&mut self, sid: SentenceId) {
        if !self.sent_idx.contains_key(&sid) { return; }
        // Collect the set of sids we need to purge (root + every
        // reachable sub-sentence) before mutating the index.
        let mut to_purge: Vec<SentenceId> = vec![sid];
        let mut stack: Vec<SentenceId>    = vec![sid];
        while let Some(cur) = stack.pop() {
            if !self.sent_idx.contains_key(&cur) { continue; }
            let vec_idx  = self.sent_idx(cur);
            let sentence = &self.sentences[vec_idx];
            for el in &sentence.elements {
                if let Element::Sub { sid: sub, .. } = el {
                    to_purge.push(*sub);
                    stack.push(*sub);
                }
            }
        }
        let purge: HashSet<SentenceId> = to_purge.into_iter().collect();
        for entries in self.occurrences.values_mut() {
            entries.retain(|o| !purge.contains(&o.sid));
        }
        self.occurrences.retain(|_, v| !v.is_empty());
    }

    // -- Axiom-occurrence index (Symbol.all_sentences) ------------------------
    //
    // Semantics of `Symbol.all_sentences`: axiom SentenceIds in which the
    // symbol appears.  "Axiom" means a root sentence that has been promoted
    // (fingerprint session = None); session assertions do NOT update this
    // index.  Population sites: `KnowledgeBase::{make_session_axiomatic,
    // promote_assertions_unchecked, open}`.
    //
    // De-duplicated per axiom: if a symbol appears multiple times in the
    // same axiom (e.g. `(subclass Dog Dog)`), the axiom is recorded once.

    /// Register the symbols of `sid` (recursively through sub-sentences) as
    /// axiom occurrences.  Idempotent: calling twice for the same sid
    /// does not create duplicate entries.
    pub(crate) fn register_axiom_symbols(&mut self, sid: SentenceId) {
        let syms = self.sentence_symbols(sid);
        for s in syms {
            let vec_idx = self.sym_vec_idx(s);
            let entries = &mut self.symbol_data[vec_idx].all_sentences;
            if !entries.contains(&sid) {
                entries.push(sid);
            }
        }
    }

    /// Symmetric to [`Self::register_axiom_symbols`]: remove `sid` from every
    /// symbol's `all_sentences` list.
    #[allow(dead_code)]
    pub(crate) fn unregister_axiom_symbols(&mut self, sid: SentenceId) {
        let syms = self.sentence_symbols(sid);
        for s in syms {
            let vec_idx = self.sym_vec_idx(s);
            self.symbol_data[vec_idx].all_sentences.retain(|&x| x != sid);
        }
    }

    /// Read-only access to a symbol's axiom-occurrence list.  Empty slice
    /// for unknown ids.
    #[allow(dead_code)]
    pub(crate) fn axiom_sentences_of(&self, sym: SymbolId) -> &[SentenceId] {
        self.sym_idx.get(&sym)
            .map(|&idx| self.symbol_data[idx].all_sentences.as_slice())
            .unwrap_or(&[])
    }

    /// Collect the set of SymbolIds referenced by the given sentence
    /// (transitively into its sub-sentences).
    pub(crate) fn sentence_symbols(&self, sid: SentenceId) -> HashSet<SymbolId> {
        let mut out = HashSet::new();
        if self.sent_idx.contains_key(&sid) {
            self.collect_symbols(sid, &mut out);
        }
        out
    }

    pub(in crate::syntactic) fn collect_symbols(&self, sent_id: SentenceId, out: &mut HashSet<SymbolId>) {
        let sentence = &self.sentences[self.sent_idx(sent_id)];
        for el in &sentence.elements {
            match el {
                Element::Symbol { id, .. } => { out.insert(*id); }
                Element::Sub { sid: sub_id, .. } => self.collect_symbols(*sub_id, out),
                _ => {}
            }
        }
    }
}
