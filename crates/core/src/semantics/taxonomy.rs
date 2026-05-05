// crates/core/src/semantics/taxonomy.rs
//
// Manage and construct taxonomies for KB from SemanticLayer

use serde::{Deserialize, Serialize};

use crate::{Element, SentenceId, SymbolId, semantics::cache::CacheImpact};

use super::SemanticLayer;

// -- Taxonomy ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TaxRelation {
    Subclass,
    Instance,
    Subrelation,
    SubAttribute,
}

impl TaxRelation {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "subclass"     => Some(TaxRelation::Subclass),
            "instance"     => Some(TaxRelation::Instance),
            "subrelation"  => Some(TaxRelation::Subrelation),
            "subAttribute" => Some(TaxRelation::SubAttribute),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxEdge {
    /// The "parent" (second argument in the sentence; more general side).
    pub from: SymbolId,
    /// The "child" (first argument; more specific side).
    pub to: SymbolId,
    pub rel: TaxRelation,
}

impl SemanticLayer {
    // -- Taxonomy management ---------------------------------------------------

    /// Extract a taxonomy edge from a single sentence, if applicable.
    ///
    /// Called for every sentence (roots and sub-sentences) when rebuilding.
    /// Non-taxonomy sentences (those not headed by subclass/instance/etc.) are
    /// silently ignored.
    fn extract_tax_edge_for(&mut self, sid: SentenceId) {
        let sentence  = &self.syntactic.sentences[self.syntactic.sent_idx(sid)];
        let head_sym  = match sentence.head_symbol() { Some(id) => id, None => return };
        let head_name = self.syntactic.sym_name(head_sym).to_owned();
        let rel       = match TaxRelation::from_str(&head_name) { Some(r) => r, None => return };
        let arg1 = match sentence.elements.get(1) {
            Some(Element::Symbol { id, .. })                        => *id,
            Some(Element::Variable { id, is_row: false, .. }) => *id,
            _ => return,
        };
        let arg2 = match sentence.elements.get(2) {
            Some(Element::Symbol { id, .. })                        => *id,
            Some(Element::Variable { id, is_row: false, .. }) => *id,
            _ => return,
        };
        let edge_idx = self.tax_edges.len();
        self.tax_edges.push(TaxEdge { from: arg2, to: arg1, rel });
        self.tax_incoming.entry(arg1).or_default().push(edge_idx);
        crate::emit_event!(crate::ProgressEvent::Log { level: crate::LogLevel::Trace, target: "sigmakee_rs_core::semantic", message: format!("tax edge: {} -{}-> {}", self.syntactic.sym_name(arg2), head_name, self.syntactic.sym_name(arg1)) });
    }

    /// Rebuild the taxonomy from scratch by scanning all known sentences.
    ///
    /// Call after `store.remove_file` (which removes sentences) or after
    /// loading from LMDB (where sentences are inserted without going through
    /// `build_sentence`).  Also called internally by `SemanticLayer::new`.
    pub(crate) fn rebuild_taxonomy(&mut self) {
        self.tax_edges.clear();
        self.tax_incoming.clear();
        // Scan roots and all sub-sentences.  Taxonomy predicates are always
        // top-level in SUMO, but sub-sentences are included for completeness.
        let mut all_sids = self.syntactic.roots.clone();
        all_sids.extend(self.syntactic.sub_sentences.iter().copied());
        for sid in all_sids {
            self.extract_tax_edge_for(sid);
        }
        // crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Debug, target: "sigmakee_rs_core::semantic", message: format!("taxonomy rebuilt: {} edges", self.tax_edges.len()) });
        // self.numeric_sort_cache   = self.build_numeric_sort_cache();
        // self.numeric_ancestor_set = self.build_numeric_ancestor_set();
        // self.poly_variant_symbols = self.build_poly_variant_symbols();
        // self.numeric_char_cache   = self.build_numeric_char_cache();
        // crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Debug, target: "sigmakee_rs_core::semantic", message: format!("numeric sort cache: {} classes, {} numeric-ancestor classes, {} poly-variant symbols, \
            //  {} numeric characterizations", self.numeric_sort_cache.len(), self.numeric_ancestor_set.len(), self.poly_variant_symbols.len(), self.numeric_char_cache.len()) });
    }

    /// Extend the taxonomy and selectively invalidate derived caches
    /// based on what the new sentences actually contain.
    ///
    /// Phase B + C of the semantic-cache optimisation series.  The
    /// expensive part of a full `rebuild_taxonomy` is the
    /// `extract_tax_edge_for` scan across every root + sub-sentence
    /// in the store -- tens of thousands of sentences at SUMO scale.
    /// This function walks **only the new sids** passed in:
    ///
    /// 1. For each new root, classify whether the sentence (or any of
    ///    its sub-sentences) could affect a derived cache.  The four
    ///    categories are taxonomy, sort_annotations, numeric_char,
    ///    semantic_cache (see [`CacheImpact`]).
    /// 2. For each new sid with a taxonomy impact, extract tax edges
    ///    via `extract_tax_edge_for` (walking root + sub-sentences of
    ///    that sid only, not the whole KB).
    /// 3. If any tax edges were added, rebuild the four derived
    ///    taxonomy caches (`numeric_sort_cache`, `numeric_ancestor_set`,
    ///    `poly_variant_symbols`, `numeric_char_cache`).  These
    ///    rebuilds already scan the taxonomy tables, not sentences,
    ///    so they're O(edges) and fast.
    /// 4. Selectively invalidate the other caches based on the
    ///    per-sentence classification.
    ///
    /// When none of the new sentences have a cache impact (the common
    /// case for SUMO tells like `(attribute X Y)`), this function is
    /// effectively free -- no scans, no invalidations, no rebuilds.
    pub(crate) fn extend_taxonomy_with(&mut self, new_sids: &[SentenceId]) {
        if new_sids.is_empty() {
            return;
        }

        // -- Classify: union of impact across all new sentences -------
        let mut impact = CacheImpact::none();
        for &sid in new_sids {
            impact = impact.union(&self.classify_sentence_tree(sid));
            if impact.all_set() {
                break;  // already at worst case, no point in more classification
            }
        }

        crate::emit_event!(crate::ProgressEvent::Log { level: crate::LogLevel::Debug, target: "sigmakee_rs_core::semantic", message: format!("extend_taxonomy_with: {} sids -> impact {:?}", new_sids.len(), impact) });

        if !impact.any() {
            // Most common case: no derived state is affected.
            return;
        }

        // -- Extract tax edges from new sentences ---------------------
        //
        // Only scan the new sids (and their sub-sentences), not the
        // entire KB.  Edge duplicates are handled by `extract_tax_edge_for`
        // itself -- it appends unconditionally, and the downstream
        // cache rebuilders tolerate duplicates.  For this to be safe
        // the new sids must not have been extracted before, which
        // holds by construction in the ingest path.
        if impact.taxonomy {
            let before = self.tax_edges.len();
            for &sid in new_sids {
                // Extract from the root...
                self.extract_tax_edge_for(sid);
                // ...and all its sub-sentences.  Sub-sentence ids are
                // tracked globally in store.sub_sentences; instead of
                // iterating that whole list, we recursively walk this
                // sentence tree (small -- typically <20 nested sids).
                self.extract_tax_edges_from_subtree(sid);
            }
            let added = self.tax_edges.len() - before;
            crate::emit_event!(crate::ProgressEvent::Log { level: crate::LogLevel::Debug, target: "sigmakee_rs_core::semantic", message: format!("extend_taxonomy_with: {} new tax edges added (total now {})", added, self.tax_edges.len()) });

            if added > 0 {
                // Rebuild the four derived taxonomy caches.  These
                // walk tax_edges (O(edges)) + a targeted sentence
                // scan for numeric_char_cache.  Cheap relative to a
                // full extract_tax_edge_for-everything rebuild.
                // self.numeric_sort_cache   = self.build_numeric_sort_cache();
                // self.numeric_ancestor_set = self.build_numeric_ancestor_set();
                // self.poly_variant_symbols = self.build_poly_variant_symbols();
                // numeric_char_cache build is sentence-scanning today;
                // only rebuild if we flagged a numeric_char impact.
                // if impact.numeric_char {
                //     self.numeric_char_cache = self.build_numeric_char_cache();
                // }
            }
        } else if impact.numeric_char {
            // Numeric-char biconditional added with no taxonomy edge
            // (unusual -- most numeric biconditionals come with
            // their subclass declaration elsewhere).  Rebuild that
            // one cache, leave the rest.
            // self.numeric_char_cache = self.build_numeric_char_cache();
        }

        // -- Selective invalidation based on impact -------------------
        if impact.semantic_cache {
            self.invalidate_semantic_cache();
        }
        if impact.sort_annotations {
            // self.invalidate_sort_annotations();
        }
    }

    /// Walk every `Element::Sub { sid: ssid, .. }` under the tree rooted at `sid`
    /// and call `extract_tax_edge_for` for each.  This covers the
    /// "sub-sentence taxonomy edges" the original full-rebuild picked
    /// up by iterating `store.sub_sentences`.
    fn extract_tax_edges_from_subtree(&mut self, sid: SentenceId) {
        // Collect sub-sids first to avoid borrow conflicts between
        // iterating the sentence and mutating self.tax_edges.
        let mut sub_sids: Vec<SentenceId> = Vec::new();
        self.collect_sub_sids(sid, &mut sub_sids);
        for ssid in sub_sids {
            self.extract_tax_edge_for(ssid);
        }
    }
}