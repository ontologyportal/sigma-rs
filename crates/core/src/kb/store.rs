//! Symbol store interactions for the KB.

use super::KnowledgeBase;
use crate::{SentenceId, SymbolId, Sentence};

impl<L: crate::layer::TopLayer> KnowledgeBase<L> {
    /// Pattern-based sentence lookup.
    ///
    /// `pattern` is a KIF expression string such as `"(instance ?X Dog)"` or
    /// `"(=> (instance ?X PositiveInteger) ?Q)"`.  KIF variables (`?X`, `@Row`,
    /// etc.) act as wildcards; the same variable name within one pattern
    /// generates a consistency check (both positions must match the same value).
    /// Nested sub-formulas are matched structurally via
    /// [`PatternElement::SubPattern`].
    ///
    /// Returns an empty `Vec` when `pattern` is empty, fails to parse, or
    /// references a symbol that is not present in this KB.
    ///
    /// # Examples
    /// ```ignore
    /// kb.lookup("(instance ?X BinaryRelation)")  // all (instance X BinaryRelation)
    /// kb.lookup("(instance ?X ?C)")              // every instance sentence
    /// kb.lookup("(=> (instance ?X PositiveInteger) ?Q)")  // implications about PositiveInteger
    /// ```
    pub fn lookup(&self, pattern: &str) -> Vec<SentenceId> {
        use crate::syntactic::pattern::{MatchKey, PatternElement, PatternFromKifError};
        let syntactic = &self.layer.semantic().syntactic;
        let pat = match syntactic.patterns().pattern_from_kif(pattern) {
            Ok(p)  => p,
            Err(PatternFromKifError::NoRootSentence) => return Vec::new(),
            Err(PatternFromKifError::UnknownSymbol(sym)) =>
                panic!("KnowledgeBase::lookup: unknown symbol '{sym}' in pattern \"{pattern}\""),
        };

        let head: Option<String> = if let Some(PatternElement::Exact(MatchKey::Symbol(sym))) = pat.0.first() {
            Some(sym.name().to_string())
        } else {
            None
        };

        syntactic
            .patterns().find_by_pattern(&pat, head.as_deref(), None)
            .into_iter()
            .map(|(sid, _)| sid)
            .collect()
    }

    /// The persistent [`SymbolId`] for a symbol name, if interned.
    pub fn symbol_id(&self, name: &str) -> Option<SymbolId> {
        self.layer.semantic().syntactic.sym_id(name)
    }

    /// True when `symbol` is a Skolem function (introduced by the CNF
    /// clausifier). Useful for filtering Skolem names out of
    /// workspace-symbol search and completion lists.
    pub fn symbol_is_skolem(&self, symbol: &str) -> bool {
        self.symbol_id(symbol)
            .map(|id| self.layer.semantic().syntactic.is_skolem(id))
            .unwrap_or(false)
    }

    /// True when `symbol` is a scope-qualified variable, not an ontology term.
    ///
    /// Variables are interned into the symbol table under the name
    /// `<var>__<scope>` (see `Sentence::from_node`) so their id resolves to a
    /// name, but they are stored as `Element::Variable`, never as an ontology
    /// symbol. Callers counting or listing *terms* (search, completion, KB
    /// stats) should exclude them. No SUMO term ends in `__<digits>`, so the
    /// name shape is an exact discriminator.
    pub fn symbol_is_variable(&self, symbol: &str) -> bool {
        symbol.rsplit_once("__")
            .is_some_and(|(head, scope)| !head.is_empty()
                && !scope.is_empty() && scope.bytes().all(|b| b.is_ascii_digit()))
    }

    /// Resolve a [`SymbolId`] to its interned name.
    ///
    /// Returns `None` for ids that aren't in the store.
    pub fn sym_name(&self, id: crate::types::SymbolId) -> Option<String> {
        self.layer.semantic().syntactic.sym_name(id).map(|s| s.name().to_string())
    }

    /// Fetch a root or sub-sentence by id.
    ///
    /// Returns `None` when `sid` isn't a known sentence.
    pub fn sentence(&self, sid: SentenceId) -> Option<std::sync::Arc<Sentence>> {
        self.layer.semantic().syntactic.sentence(sid)
    }

    /// Find the innermost element at byte `offset` in `file`.
    ///
    /// Returns the deepest non-synthetic element whose span covers the
    /// offset, or `None` when `file` isn't loaded or `offset` falls
    /// outside every root sentence's `(...)` range.
    pub fn element_at_offset(&self, file: &str, offset: usize) -> Option<crate::syntactic::position::ElementHit> {
        crate::syntactic::position::element_at_offset(&self.layer.semantic().syntactic, file, offset)
    }

    /// Name of the symbol at `offset`, if the element there is a
    /// [`Element::Symbol`](crate::types::Element::Symbol).
    pub fn symbol_at_offset(&self, file: &str, offset: usize) -> Option<String> {
        crate::syntactic::position::symbol_at_offset(&self.layer.semantic().syntactic, file, offset)
    }

    /// Interned id for whatever symbol-like element is at `offset`,
    /// **including** [`Element::Variable`](crate::types::Element::Variable).
    /// For ordinary symbols the id is the intern-table entry; for variables
    /// it's the scope-qualified id (distinct `?X` instances in different
    /// quantifier bodies get distinct ids).
    ///
    /// Returns `(id, display_name)` -- the display name is `"?X"` or
    /// `"@Row"` for variables, the plain interned name for symbols.
    pub fn id_at_offset(
        &self, file: &str, offset: usize,
    ) -> Option<(SymbolId, String)> {
        let hit = self.element_at_offset(file, offset)?;
        let name = hit.name?;
        if hit.is_variable { return None; } // scoped variable id is not derivable from a source position
        let id = self.layer.semantic().syntactic.sym_id(&name)?;
        Some((id, name))
    }

    /// Every occurrence of `symbol` across every loaded file.
    ///
    /// Returns an empty `Vec` when the symbol is unknown or has no
    /// non-synthetic occurrences.
    pub fn occurrences(&self, symbol: &str) -> Vec<crate::types::Occurrence> {
        self.symbol_id(symbol)
            .map(|id| self.occurrences_of(id))
            .unwrap_or_default()
    }

    /// Occurrences by raw `SymbolId`.
    ///
    /// Returns a deterministic `Vec` ordered by file then source position.
    pub fn occurrences_of(&self, id: crate::types::SymbolId) -> Vec<crate::types::Occurrence> {
        let syntactic = &self.layer.semantic().syntactic;
        let Some(name) = syntactic.sym_name(id) else { return Vec::new() };
        let mut occs: Vec<crate::types::Occurrence> = syntactic
            .occurrences
            .get(&name.name().to_string()).unwrap_or_default().iter().cloned().collect();
        occs.sort_by(|a, b| {
            a.span.file.cmp(&b.span.file).then(a.span.offset.cmp(&b.span.offset))
        });
        occs
    }

    /// Iterate every interned symbol as `(SymbolId, name)` pairs.
    ///
    /// Skolem symbols are included; filter them via
    /// [`symbol_is_skolem`](Self::symbol_is_skolem). Iteration order is
    /// arbitrary but stable within one KB instance.
    pub fn iter_symbols(&self) -> impl Iterator<Item = (crate::types::SymbolId, String)> + '_ {
        self.layer.semantic().syntactic.symbols.snapshot()
            .into_iter()
            .map(|(id, sym)| (id, sym.name().to_string()))
    }

    /// The file/session tags that contributed `sid`.
    pub fn files_of(&self, sid: SentenceId) -> Vec<String> {
        self.layer.semantic().syntactic.sessions.provenance_of(sid)
    }

    /// Every distinct head-predicate name currently indexed in the store
    /// (the relations / predicates / functions that appear as sentence heads).
    pub fn head_names(&self) -> Vec<String> {
        let store = &self.layer.semantic().syntactic;
        store.residue_head_symbols()
            .into_iter()
            .filter_map(|id| store.sym_name(id).map(|s| s.name().to_string()))
            .collect()
    }

    /// The content fingerprints a file contributed. Order is unspecified.
    pub fn file_hashes(&self, file: &str) -> Vec<u64> {
        self.layer.semantic().syntactic.file_fingerprints(file)
    }

    /// The root sentence ids a file produced. Order is unspecified.
    pub fn file_roots(&self, file: &str) -> Vec<SentenceId> {
        self.layer.semantic().syntactic.file_root_sids(file)
    }

    /// Every file tag currently loaded in the KB.
    pub fn iter_files(&self) -> Vec<String> {
        self.layer.semantic().syntactic.source_files()
    }

    /// The provenance recorded when `file` was last ingested (its
    /// mtime/content-hash for a local file, or branch/commit for a git
    /// source), if any. This is a baseline snapshot from ingest time, not a
    /// live check — compare it against a freshly computed provenance to tell
    /// whether the source has changed since.
    pub fn file_origin(&self, file: &str) -> Option<crate::FileOrigin> {
        self.layer.semantic().syntactic.file_origin(file)
    }
}