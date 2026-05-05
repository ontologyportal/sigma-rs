// crates/core/src/syntactic/persist.rs
//
// LMDB persistence helpers for SyntacticLayer.  Gated on `persist`.

use crate::types::{SentenceId, SymbolId};

use super::SyntacticLayer;

impl SyntacticLayer {
    /// Seed the ID counters from the LMDB max values.  Called by `open()`
    /// after loading all existing formulas from the DB so that any new IDs
    /// assigned in-memory are guaranteed not to collide with existing
    /// persisted IDs.
    pub(crate) fn seed_counters(&mut self, next_sym: u64, next_sent: u64) {
        self.next_symbol_id   = next_sym;
        self.next_sentence_id = next_sent;
        crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Debug, target: "sigmakee_rs_core::syntactic", message: format!("ID counters seeded: next_symbol_id={}, next_sentence_id={}", next_sym, next_sent) });
    }

    /// Insert a stable SentenceId -> Vec position mapping (used by persist::load).
    pub(crate) fn insert_sent_idx(&mut self, sid: SentenceId, vec_pos: usize) {
        self.sent_idx.insert(sid, vec_pos);
    }

    /// Insert a stable SymbolId -> Vec position mapping (used by persist::load).
    pub(crate) fn insert_sym_idx(&mut self, sym_id: SymbolId, vec_pos: usize) {
        self.sym_idx.insert(sym_id, vec_pos);
    }
}
