// crates/core/src/kb/store.rs
//
// This handles symbol store interactions for the KB

use super::KnowledgeBase;
use crate::{SentenceId, SymbolId, Sentence};

impl KnowledgeBase {
    /// Pattern-based sentence lookup (delegates to SyntacticLayer::lookup).
    pub fn lookup(&self, pattern: &str) -> Vec<SentenceId> {
        self.layer.semantic.syntactic.lookup(pattern)
    }

    /// Pass through for fetching the persistent [`SymbolId`]
    /// from the name of the symbol
    pub fn symbol_id(&self, name: &str) -> Option<SymbolId> {
        self.layer.semantic.syntactic.sym_id(name)
    }

    /// Inverse of [`Self::symbol_id`]: resolve a [`SymbolId`] to its interned
    /// name. Returns an owned `String` to keep the lifetime simple.
    /// Ids that aren't in the store return `None`.
    pub fn sym_name(&self, id: crate::types::SymbolId) -> Option<String> {
        if self.layer.semantic.syntactic.has_symbol(id) {
            Some(self.layer.semantic.syntactic.sym_name(id).to_owned())
        } else {
            None
        }
    }

    /// Fetch a root or sub-sentence by id.  Returns `None` when
    /// `sid` isn't a known sentence (e.g. after [`remove_sentence`]
    /// the id is valid but the body is empty).
    pub fn sentence(&self, sid: SentenceId) -> Option<&Sentence> {
        if !self.layer.semantic.syntactic.has_sentence(sid) { return None; }
        Some(&self.layer.semantic.syntactic.sentences[self.layer.semantic.syntactic.sent_idx(sid)])
    }

    /// Find the innermost element at byte `offset` in `file`.
    ///
    /// Walks the file's root sentences and descends through sub-
    /// sentences; returns the deepest non-synthetic element whose
    /// span covers the offset.  Useful for hover, goto-definition,
    /// rename, and any other cursor-driven query.
    ///
    /// Returns `None` when `file` isn't loaded or when `offset`
    /// falls outside every root sentence's `(...)` range.
    pub fn element_at_offset(&self, file: &str, offset: usize) -> Option<crate::lookup::ElementHit> {
        crate::lookup::element_at_offset(&self.layer.semantic.syntactic, file, offset)
    }

    /// Name of the symbol at `offset`, if the element there is a
    /// [`Element::Symbol`](crate::types::Element::Symbol).  Thin
    /// wrapper over [`element_at_offset`](Self::element_at_offset)
    /// + a type check.
    pub fn symbol_at_offset(&self, file: &str, offset: usize) -> Option<String> {
        crate::lookup::symbol_at_offset(&self.layer.semantic.syntactic, file, offset)
    }

    /// Interned id for whatever symbol-like element is at `offset`,
    /// **including** [`Element::Variable`](crate::types::Element::Variable).
    /// For ordinary symbols the id is the intern-table entry; for variables
    /// it's the scope-qualified id (distinct `?X` instances in different
    /// quantifier bodies get distinct ids).
    ///
    /// Powers references / rename for variables: looking up the
    /// occurrence index by this id automatically gives back every
    /// co-bound occurrence inside the same scope and excludes
    /// same-named variables in other scopes.
    ///
    /// Returns `(id, display_name)` -- the display name is `"?X"` or
    /// `"@Row"` for variables, the plain interned name for symbols.
    pub fn id_at_offset(
        &self, file: &str, offset: usize,
    ) -> Option<(SymbolId, String)> {
        let hit = self.element_at_offset(file, offset)?;
        let sent = self.sentence(hit.sid)?;
        match sent.elements.get(hit.idx)? {
            crate::types::Element::Symbol { id, .. } => {
                let name = self.layer.semantic.syntactic.sym_name(*id).to_owned();
                Some((*id, name))
            }
            crate::types::Element::Variable { id, name, is_row, .. } => {
                let display = if *is_row { format!("@{}", name) } else { format!("?{}", name) };
                Some((*id, display))
            }
            _ => None,
        }
    }

    /// Every occurrence of `symbol` across every loaded file.
    ///
    /// Returned in insertion order (root sentences first by their
    /// load order, then sub-sentences within each).  Non-LSP
    /// consumers: a CLI "find references" command, coverage
    /// reporting, programmatic walks.  Returns an empty slice when
    /// the symbol is unknown or has no non-synthetic occurrences.
    pub fn occurrences(&self, symbol: &str) -> &[crate::types::Occurrence] {
        self.symbol_id(symbol)
            .map(|id| self.occurrences_of(id))
            .unwrap_or(&[])
    }

    /// Occurrences by raw `SymbolId`.  Useful when the caller has
    /// already done name lookup (variables' scope-qualified ids,
    /// cursor-driven queries that already produced a
    /// `KnowledgeBase::element_at_offset` hit).
    pub fn occurrences_of(&self, id: crate::types::SymbolId) -> &[crate::types::Occurrence] {
        self.layer.semantic.syntactic.occurrences.get(&id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Iterate every interned symbol as `(SymbolId, name)` pairs.
    /// Powers workspace-symbol search (fuzzy "jump to any symbol
    /// in the KB"), dump utilities, and any consumer that needs
    /// the full symbol set.  Skolem symbols are included --
    /// callers that want to hide them can filter via
    /// [`symbol_is_skolem`](Self::symbol_is_skolem).
    ///
    /// Iteration order matches the intern table's hash-map order
    /// (i.e. arbitrary but stable within one KB instance).
    pub fn iter_symbols(&self) -> impl Iterator<Item = (crate::types::SymbolId, &str)> + '_ {
        self.layer.semantic.syntactic.symbols.iter().map(|(name, &id)| (id, name.as_str()))
    }

    /// Iterate every distinct head-predicate name currently indexed
    /// in the store.  These are the relations / predicates /
    /// functions that *actually appear as sentence heads*, which is
    /// almost always what a completion menu at sentence-head
    /// position wants to suggest (anything declared but never used
    /// as a head isn't a useful completion target).
    ///
    /// Non-LSP uses: any tool presenting a menu of the KB's
    /// relation vocabulary -- CLI REPL completions, doc generators,
    /// summary reports.
    pub fn head_names(&self) -> impl Iterator<Item = &str> + '_ {
        self.layer.semantic.syntactic.head_index.keys().map(|s| s.as_str())
    }

    /// Read-only view of the per-file fingerprint vector.  The
    /// returned slice is positionally aligned with
    /// [`file_roots`](Self::file_roots) for the same `file`.
    pub fn file_hashes(&self, file: &str) -> &[u64] {
        self.layer.semantic.syntactic.file_hashes.get(file).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Read-only view of the per-file root-sentence ids, in source order.
    pub fn file_roots(&self, file: &str) -> &[SentenceId] {
        self.layer.semantic.syntactic.file_roots.get(file).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Iterate every file tag currently loaded in the KB, in
    /// HashMap-iteration (arbitrary but stable-within-run) order.
    pub fn iter_files(&self) -> impl Iterator<Item = &str> + '_ {
        self.layer.semantic.syntactic.file_roots.keys().map(|s| s.as_str())
    }
}