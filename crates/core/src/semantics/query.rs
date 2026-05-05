// crates/core/src/semantics/query.rs
//
// Semantic layer querying functionality

use std::collections::HashSet;

use crate::{Element, Literal, SentenceId};
use crate::syntactic::SyntacticLayer;
use crate::{SymbolId, semantics::taxonomy::TaxRelation};

use super::SemanticLayer;
use super::relation::RelationDomain;
use super::errors::SemanticError;

/// A single documentation blurb as authored in the ontology.
///
/// `text` has the surrounding quotes of the KIF string literal stripped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocEntry {
    pub language: String,
    pub text:     String,
}

impl SemanticLayer {
    // -- Basic semantic queries -------------------------------------------------

    pub(crate) fn is_instance(&self, sym: SymbolId) -> bool {
        if let Some(&v) = self.cache.read().unwrap().is_instance.get(&sym) { return v; }
        let v = self.compute_is_instance(sym, &mut HashSet::new());
        self.cache.write().unwrap().is_instance.insert(sym, v);
        v
    }

    fn compute_is_instance(&self, sym: SymbolId, visited: &mut HashSet<SymbolId>) -> bool {
        if visited.contains(&sym) { return false; }
        visited.insert(sym);
        let edges = match self.tax_incoming.get(&sym) {
            Some(v) => v.clone(),
            None    => return false,
        };
        for &ei in &edges {
            let edge = &self.tax_edges[ei];
            match edge.rel {
                TaxRelation::Instance => return true,
                TaxRelation::Subrelation | TaxRelation::SubAttribute => {
                    if self.compute_is_instance(edge.from, visited) { return true; }
                }
                TaxRelation::Subclass => {}
            }
        }
        false
    }

    pub(crate) fn is_class(&self, sym: SymbolId) -> bool {
        if let Some(&v) = self.cache.read().unwrap().is_class.get(&sym) { return v; }
        let v = match self.tax_incoming.get(&sym) {
            None    => true,
            Some(edges) => edges.iter().all(|&ei| {
                self.tax_edges[ei].rel == TaxRelation::Subclass
            }),
        };
        self.cache.write().unwrap().is_class.insert(sym, v);
        v
    }

    pub(crate) fn is_relation(&self, sym: SymbolId) -> bool {
        if let Some(&v) = self.cache.read().unwrap().is_relation.get(&sym) { return v; }
        let v = self.is_instance(sym) && self.has_ancestor_by_name(sym, "Relation");
        self.cache.write().unwrap().is_relation.insert(sym, v);
        v
    }

    pub(crate) fn is_predicate(&self, sym: SymbolId) -> bool {
        if let Some(&v) = self.cache.read().unwrap().is_predicate.get(&sym) { return v; }
        let v = self.is_instance(sym) && self.has_ancestor_by_name(sym, "Predicate");
        self.cache.write().unwrap().is_predicate.insert(sym, v);
        v
    }

    pub(crate) fn is_function(&self, sym: SymbolId) -> bool {
        if let Some(&v) = self.cache.read().unwrap().is_function.get(&sym) { return v; }
        let v = self.is_instance(sym) && self.has_ancestor_by_name(sym, "Function");
        self.cache.write().unwrap().is_function.insert(sym, v);
        v
    }

    pub(crate) fn has_ancestor_by_name(&self, sym: SymbolId, ancestor: &str) -> bool {
        let anc_id = match self.syntactic.sym_id(ancestor) {
            Some(id) => id,
            None     => return false,
        };
        self.has_ancestor(sym, anc_id)
    }

    pub(crate) fn has_ancestor(&self, sym: SymbolId, ancestor: SymbolId) -> bool {
        if sym == ancestor { return true; }
        if let Some(&v) = self.cache.read().unwrap().has_ancestor.get(&(sym, ancestor)) {
            return v;
        }
        let v = self.compute_has_ancestor(sym, ancestor, &mut HashSet::new());
        self.cache.write().unwrap().has_ancestor.insert((sym, ancestor), v);
        v
    }

    fn compute_has_ancestor(
        &self, sym: SymbolId, ancestor: SymbolId, visited: &mut HashSet<SymbolId>,
    ) -> bool {
        if sym == ancestor { return true; }
        if visited.contains(&sym) { return false; }
        visited.insert(sym);
        let edges = match self.tax_incoming.get(&sym) {
            Some(v) => v.clone(),
            None    => return false,
        };
        for &ei in &edges {
            let from = self.tax_edges[ei].from;
            if self.compute_has_ancestor(from, ancestor, visited) { return true; }
        }
        false
    }

    /// Collect every `Element::Sub { sid: ssid, .. }` descendant of `sid` (excluding
    /// `sid` itself) into `out`.  Ordering is a pre-order traversal.
    pub(super) fn collect_sub_sids(&self, sid: SentenceId, out: &mut Vec<SentenceId>) {
        if !self.syntactic.has_sentence(sid) { return; }
        for el in &self.syntactic.sentences[self.syntactic.sent_idx(sid)].elements {
            if let Element::Sub { sid: ssid, .. } = el {
                out.push(*ssid);
                self.collect_sub_sids(*ssid, out);
            }
        }
    }

    /// Pick the most-specific class from `classes` according to the
    /// loaded subclass taxonomy.  A class is "most specific" when every
    /// other class in the list is one of its ancestors (i.e. it sits
    /// deepest in the subclass hierarchy among the candidates).
    ///
    /// Returns `None` when:
    ///   * `classes` is empty,
    ///   * none of the names resolve to a known SymbolId, or
    ///   * the candidates form an antichain (no single class dominates;
    ///     e.g. `["Animal", "Plant"]` where neither is an ancestor of
    ///     the other).
    ///
    /// Single-element input returns that name (after symbol resolution).
    /// Names that don't resolve to a SymbolId are silently dropped before
    /// the comparison — pass only candidates you've already confirmed
    /// exist if that matters.
    pub(crate) fn most_specific_class(&self, classes: &[&str]) -> Option<String> {
        let resolved: Vec<(SymbolId, String)> = classes
            .iter()
            .filter_map(|n| self.syntactic.sym_id(n).map(|id| (id, (*n).to_string())))
            .collect();
        if resolved.is_empty() { return None; }
        if resolved.len() == 1 { return Some(resolved[0].1.clone()); }

        // A candidate is "most specific" iff every *other* candidate is
        // its ancestor — i.e. the candidate descends from all of them.
        for (cand_id, cand_name) in &resolved {
            let dominates_all = resolved.iter().all(|(other_id, _)| {
                other_id == cand_id || self.has_ancestor(*cand_id, *other_id)
            });
            if dominates_all {
                return Some(cand_name.clone());
            }
        }
        None
    }

    pub(crate) fn arity(&self, sym: SymbolId) -> Option<i32> {
        if let Some(&v) = self.cache.read().unwrap().arity.get(&sym) { return v; }
        let v = if !self.is_relation(sym) {
            None
        } else {
            self.compute_arity(sym)
        };
        self.cache.write().unwrap().arity.insert(sym, v);
        v
    }

    fn compute_arity(&self, sym: SymbolId) -> Option<i32> {
        const MAPPINGS: &[(&str, i32)] = &[
            ("BinaryRelation",        2),
            ("TernaryRelation",       3),
            ("QuaternaryRelation",    4),
            ("QuintaryRelation",      5),
            ("VariableArityRelation", -1),
        ];
        for &(class, n) in MAPPINGS {
            if self.has_ancestor_by_name(sym, class) {
                let arity = if n > 0 && self.is_function(sym) { n - 1 } else { n };
                return Some(arity);
            }
        }
        None
    }

    pub(crate) fn range(
        &self, rel: SymbolId,
    ) -> Result<Option<RelationDomain>, SemanticError> {
        if let Some(v) = self.cache.read().unwrap().range.get(&rel) {
            return Ok(Some(v.clone()));
        }
        match self.compute_range(rel)? {
            Some(r) => {
                self.cache.write().unwrap().range.insert(rel, r.clone());
                Ok(Some(r))
            }
            None => Ok(None),
        }
    }

    fn compute_range(
        &self, rel: SymbolId,
    ) -> Result<Option<RelationDomain>, SemanticError> {
        let process = |head: &str, make: fn(SymbolId) -> RelationDomain| -> Option<RelationDomain> {
            for &sid in self.syntactic.by_head(head) {
                let sentence = &self.syntactic.sentences[self.syntactic.sent_idx(sid)];
                let arg1_ok = matches!(
                    sentence.elements.get(1),
                    Some(Element::Symbol { id, .. }) if *id == rel
                );
                if !arg1_ok { continue; }
                // `range` has 2 args: (range rel class) -> class is at index 2.
                // `domain` has 3 args: (domain rel argNum class) -> class at index 3.
                let class_id = match sentence.elements.get(2) {
                    Some(Element::Symbol { id, .. }) => *id,
                    _ => continue,
                };
                return Some(make(class_id));
            }
            None
        };

        let range           = process("range",        RelationDomain::Domain);
        let range_subclass  = process("rangeSubclass", RelationDomain::DomainSubclass);
        match (range, range_subclass) {
            (None, None)               => Ok(None),
            (None, Some(rs))           => Ok(Some(rs)),
            (Some(r), None)            => Ok(Some(r)),
            (Some(r), Some(_))         => {
                SemanticError::DoubleRange {
                    sym: self.syntactic.sym_name(rel).to_string(),
                }.handle(&self.syntactic)?;
                Ok(Some(r))
            }
        }
    }

    pub(crate) fn domain(&self, rel: SymbolId) -> Vec<RelationDomain> {
        if let Some(v) = self.cache.read().unwrap().domain.get(&rel) { return v.clone(); }
        let v = self.compute_domain(rel);
        self.cache.write().unwrap().domain.insert(rel, v.clone());
        v
    }

    fn compute_domain(&self, rel: SymbolId) -> Vec<RelationDomain> {
        let mut entries: Vec<(usize, RelationDomain)> = Vec::new();
        let mut process = |head: &str, make: fn(SymbolId) -> RelationDomain| {
            for &sid in self.syntactic.by_head(head) {
                let sentence = &self.syntactic.sentences[self.syntactic.sent_idx(sid)];
                let arg1_ok = matches!(
                    sentence.elements.get(1),
                    Some(Element::Symbol { id, .. }) if *id == rel
                );
                if !arg1_ok { continue; }
                let pos = match sentence.elements.get(2) {
                    Some(Element::Literal { lit: Literal::Number(n), .. }) => {
                        n.parse::<usize>().unwrap_or(0).saturating_sub(1)
                    }
                    _ => continue,
                };
                let class_id = match sentence.elements.get(3) {
                    Some(Element::Symbol { id, .. }) => *id,
                    _ => continue,
                };
                entries.push((pos, make(class_id)));
            }
        };
        process("domain",         RelationDomain::Domain);
        process("domainSubclass", RelationDomain::DomainSubclass);
        entries.sort_by_key(|&(p, _)| p);
        let max = entries.iter().map(|&(p, _)| p).max().map(|p| p + 1).unwrap_or(0);
        let mut result = vec![RelationDomain::Domain(u64::MAX); max];
        for (pos, rd) in entries {
            if pos < max { result[pos] = rd; }
        }
        result
    }

    /// Conservative "looks like `(instance ?X Class)`" check used only to
    /// flag numeric_char_cache rebuilds.  Returns true for an
    /// `Element::Sub` whose sentence has `instance` as its head; false
    /// otherwise.  No sort analysis -- the subsequent rebuild pass is
    /// what decides whether the class is actually numeric.
    pub(crate) fn contains_instance_pattern(&self, el: &Element) -> bool {
        let Element::Sub { sid, .. } = el else { return false };
        if !self.syntactic.has_sentence(*sid) { return false; }
        let s = &self.syntactic.sentences[self.syntactic.sent_idx(*sid)];
        match s.head_symbol() {
            Some(id) => self.syntactic.sym_name(id) == "instance",
            None     => false,
        }
    }

    // -- Doc-relation lookups --------------------------------------------------
    //
    // These three scan the store's head-indexed view for
    // `(documentation SYM LANG TEXT)`, `(termFormat LANG SYM TEXT)`, and
    // `(format LANG REL TEXT)` respectively.  Results are cached per
    // SymbolId on first query; subsequent lookups are HashMap hits.
    //
    // `language` filters the cached result at retrieval time; the cache
    // itself always holds the full cross-language list for a symbol.

    /// `(documentation sym lang text)` entries for this symbol.
    pub(crate) fn documentation(&self, sym: SymbolId, language: Option<&str>) -> Vec<DocEntry> {
        if let Some(v) = self.cache.read().unwrap().documentation.get(&sym) {
            return filter_lang(v, language);
        }
        let all = Self::collect_doc_relation(&self.syntactic, "documentation", sym, 1, 2, 3);
        let filtered = filter_lang(&all, language);
        self.cache.write().unwrap().documentation.insert(sym, all);
        filtered
    }

    /// `(termFormat lang sym text)` entries for this symbol.
    pub(crate) fn term_format(&self, sym: SymbolId, language: Option<&str>) -> Vec<DocEntry> {
        if let Some(v) = self.cache.read().unwrap().term_format.get(&sym) {
            return filter_lang(v, language);
        }
        let all = Self::collect_doc_relation(&self.syntactic, "termFormat", sym, 2, 1, 3);
        let filtered = filter_lang(&all, language);
        self.cache.write().unwrap().term_format.insert(sym, all);
        filtered
    }

    /// `(format lang relation text)` entries for this symbol.
    pub(crate) fn format(&self, sym: SymbolId, language: Option<&str>) -> Vec<DocEntry> {
        if let Some(v) = self.cache.read().unwrap().format.get(&sym) {
            return filter_lang(v, language);
        }
        let all = Self::collect_doc_relation(&self.syntactic, "format", sym, 2, 1, 3);
        let filtered = filter_lang(&all, language);
        self.cache.write().unwrap().format.insert(sym, all);
        filtered
    }

    /// Internal scan over head-indexed root sentences with shape
    /// `(head A B C)` where `target_idx` / `lang_idx` / `text_idx` pick
    /// the target-symbol, language-tag, and text-literal argument
    /// positions (1-based over `elements`).  Runs once per cache miss.
    fn collect_doc_relation(
        store:      &SyntacticLayer,
        head:       &str,
        target:     SymbolId,
        target_idx: usize,
        lang_idx:   usize,
        text_idx:   usize,
    ) -> Vec<DocEntry> {
        let mut out = Vec::new();
        for &sid in store.by_head(head) {
            let sent = &store.sentences[store.sent_idx(sid)];
            let tgt  = match sent.elements.get(target_idx) {
                Some(Element::Symbol { id, .. }) => *id,
                _ => continue,
            };
            if tgt != target { continue; }
            let lang = match sent.elements.get(lang_idx) {
                Some(Element::Symbol { id, .. }) => store.sym_name(*id).to_string(),
                _ => continue,
            };
            let text = match sent.elements.get(text_idx) {
                Some(Element::Literal { lit: Literal::Str(s), .. }) => strip_quotes(s),
                _ => continue,
            };
            out.push(DocEntry { language: lang, text });
        }
        out
    }
}


/// Filter a cached `DocEntry` list by language, returning owned clones.
fn filter_lang(entries: &[DocEntry], want: Option<&str>) -> Vec<DocEntry> {
    match want {
        None    => entries.to_vec(),
        Some(l) => entries.iter().filter(|e| e.language == l).cloned().collect(),
    }
}

/// Strip the surrounding `"..."` that the KIF tokenizer preserves on
/// string literals.  Safe on unquoted input -- no-op when the bounds
/// don't match.
fn strip_quotes(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}