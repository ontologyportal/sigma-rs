// crates/core/src/semantics/mod.rs
//
// Semantic query and validation layer.

pub mod cache;
pub mod errors;
pub mod validate;
pub mod query;
pub mod relation;
pub mod taxonomy;

use std::{collections::HashMap, sync::RwLock};

use crate::{SymbolId, syntactic::SyntacticLayer};
use crate::layer::Layer;
use crate::trans::TranslationLayer;

use taxonomy::TaxEdge;
use cache::Inner;

/// Middle layer of the KB stack.  Owns the [`SyntacticLayer`] and provides
/// every semantic query on top of it.
///
/// Semantic results are cached in `RwLock<Inner>` so that query
/// methods take `&self`, allowing `to_tptp` and similar readers to hold
/// `&self.syntactic` while calling semantic methods without borrow-checker
/// conflicts.
///
/// The taxonomy graph (`tax_edges`, `tax_incoming`) lives here rather than
/// in `SyntacticLayer` because it is derived semantic structure, not raw
/// storage.
#[derive(Debug)]
pub(crate) struct SemanticLayer {
    /// Inner layer: raw parse store.
    pub syntactic:       SyntacticLayer,
    /// Taxonomy edges (subclass, instance, subrelation, subAttribute).
    pub tax_edges:       Vec<TaxEdge>,
    /// `tax_incoming[sym_id]` = indices into `tax_edges` where `edge.to == sym_id`.
    pub tax_incoming:    HashMap<SymbolId, Vec<usize>>,
    /// Semantic cache entry
    pub(crate) cache:    RwLock<Inner>,
}

impl SemanticLayer {
    pub(crate) fn new(syntactic: SyntacticLayer) -> Self {
        let mut layer = Self {
            syntactic,
            tax_edges:    Vec::new(),
            tax_incoming: HashMap::new(),
            cache:        RwLock::new(Inner::default()),
        };
        layer.rebuild_taxonomy();
        layer
    }
}

impl Layer for SemanticLayer {
    type Inner = SyntacticLayer;
    type Outer = TranslationLayer;

    fn inner(&self) -> Option<&SyntacticLayer> { Some(&self.syntactic) }
    fn outer(&self) -> Option<&TranslationLayer> { None }
}
