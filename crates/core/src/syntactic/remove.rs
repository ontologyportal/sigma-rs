// crates/core/src/syntactic/remove.rs
//
// Sentence + file removal and orphaned-symbol pruning for SyntacticLayer.

use std::collections::HashSet;

use crate::types::SentenceId;

use super::SyntacticLayer;

impl SyntacticLayer {
    /// Remove the `file_roots` mapping for `file` without touching
    /// `roots` or `sentences`.  Used after promotion to detach
    /// sentences from their session tag while keeping them as
    /// in-memory axioms.
    #[cfg(feature = "persist")]
    pub(crate) fn clear_file_roots(&mut self, file: &str) {
        self.file_roots.remove(file);
    }

    pub(crate) fn remove_file(&mut self, file: &str) {
        let ids_to_remove: Vec<SentenceId> = self.file_roots.remove(file).unwrap_or_default();
        self.file_hashes.remove(file);
        if ids_to_remove.is_empty() { return; }
        // Drop occurrences for every sentence (and their sub-chain)
        // before the bodies are cleared.
        for &sid in &ids_to_remove {
            self.drop_sentence_occurrences(sid);
        }
        let id_set: HashSet<SentenceId> = ids_to_remove.iter().copied().collect();
        self.roots.retain(|id| !id_set.contains(id));
        for v in self.head_index.values_mut() { v.retain(|id| !id_set.contains(id)); }
        self.head_index.retain(|_, v| !v.is_empty());
        for &sid in &ids_to_remove {
            if self.sent_idx.contains_key(&sid) {
                let vec_idx = self.sent_idx(sid);
                self.sentences[vec_idx].elements.clear();
            }
        }
        self.prune_orphaned_symbols(&id_set);
        crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Debug, target: "sigmakee_rs_core::syntactic", message: format!("removed {} sentences from file '{}'", ids_to_remove.len(), file) });
    }

    /// Drop a single root sentence and every structural reference to it.
    pub(crate) fn remove_sentence(&mut self, sid: SentenceId) {
        if !self.sent_idx.contains_key(&sid) { return; }

        // Drop every occurrence entry tied to this sentence and its
        // sub-sentences *before* we clear the body.
        self.drop_sentence_occurrences(sid);

        // Snapshot the head symbol and owning file BEFORE we clear
        // the sentence body.
        let vec_idx     = self.sent_idx(sid);
        let head_symbol = self.sentences[vec_idx].head_symbol();
        let file_name   = self.sentences[vec_idx].file.clone();

        // Remove from roots / per-file indices.
        self.roots.retain(|&id| id != sid);
        if let Some(roots) = self.file_roots.get_mut(&file_name) {
            if let Some(pos) = roots.iter().position(|&id| id == sid) {
                roots.remove(pos);
                if let Some(hashes) = self.file_hashes.get_mut(&file_name) {
                    if pos < hashes.len() { hashes.remove(pos); }
                }
            }
        }

        // Head-index entries for this specific sid.
        for v in self.head_index.values_mut() { v.retain(|&id| id != sid); }
        self.head_index.retain(|_, v| !v.is_empty());
        if let Some(hid) = head_symbol {
            let head_vec_idx = self.sym_vec_idx(hid);
            self.symbol_data[head_vec_idx].head_sentences.retain(|&id| id != sid);
        }

        // Blank the sentence body.
        self.sentences[vec_idx].elements.clear();

        crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Trace, target: "sigmakee_rs_core::syntactic", message: format!("removed sentence sid={} (file='{}')", sid, file_name) });
    }

    /// Run the orphan-symbol prune as a public(-to-crate) operation.
    pub(crate) fn prune_orphaned_symbols_now(&mut self) {
        let empty = HashSet::new();
        self.prune_orphaned_symbols(&empty);
    }

    fn prune_orphaned_symbols(&mut self, _removed_ids: &HashSet<SentenceId>) {
        let mut referenced = HashSet::new();
        for &sid in &self.roots { self.collect_symbols(sid, &mut referenced); }
        let to_remove: Vec<String> = self.symbols.keys()
            .filter(|name| !referenced.contains(self.symbols.get(*name).unwrap()))
            .cloned().collect();
        for name in to_remove {
            if let Some(id) = self.symbols.remove(&name) {
                let sym_vec_idx = self.sym_vec_idx(id);
                self.symbol_data[sym_vec_idx].head_sentences.clear();
                self.symbol_data[sym_vec_idx].all_sentences.clear();
            }
        }
    }
}
