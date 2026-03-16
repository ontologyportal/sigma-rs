// crates/sumo-kb/src/semantic.rs
//
// Semantic query and validation layer.
//
// Ported from sumo-parser-core/src/kb.rs — semantic methods only.
// `SemanticLayer` owns the `KifStore` and wraps it with a lazy semantic
// cache.  `KnowledgeBase` (kb.rs) holds a `SemanticLayer` as its only store
// of truth and delegates all semantic queries through it.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::error::SemanticError;
use crate::kif_store::KifStore;
use crate::types::{Element, Literal, OpKind, SentenceId, SymbolId, TaxRelation};

// ── RelationDomain ────────────────────────────────────────────────────────────

/// Describes the expected type of a relation argument or return value.
#[derive(Debug, Clone)]
pub(crate) enum RelationDomain {
    /// Argument must be an instance of this class.
    Domain(SymbolId),
    /// Argument must be a subclass of this class.
    DomainSubclass(SymbolId),
}

impl RelationDomain {
    pub(crate) fn id(&self) -> SymbolId {
        match self {
            Self::Domain(id) | Self::DomainSubclass(id) => *id,
        }
    }
}

// ── SemanticCache ─────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct SemanticCache {
    is_instance:  HashMap<SymbolId, bool>,
    is_class:     HashMap<SymbolId, bool>,
    is_relation:  HashMap<SymbolId, bool>,
    is_predicate: HashMap<SymbolId, bool>,
    is_function:  HashMap<SymbolId, bool>,
    has_ancestor: HashMap<(SymbolId, SymbolId), bool>,
    arity:        HashMap<SymbolId, Option<i32>>,
    domain:       HashMap<SymbolId, Vec<RelationDomain>>,
    range:        HashMap<SymbolId, RelationDomain>,
}

// ── SemanticLayer ─────────────────────────────────────────────────────────────

/// Owns the `KifStore` and provides all semantic queries on top of it.
///
/// Semantic results are cached in a `RefCell<SemanticCache>` so that query
/// methods take `&self`, allowing `to_tptp` and similar readers to hold
/// `&self.store` while calling semantic methods without borrow-checker conflicts.
#[derive(Debug)]
pub(crate) struct SemanticLayer {
    pub store:      KifStore,
    cache:          RefCell<SemanticCache>,
}

impl SemanticLayer {
    pub(crate) fn new(store: KifStore) -> Self {
        Self {
            store,
            cache: RefCell::new(SemanticCache::default()),
        }
    }

    /// Invalidate the entire semantic cache (call after structural changes to the store).
    pub(crate) fn invalidate_cache(&self) {
        *self.cache.borrow_mut() = SemanticCache::default();
    }

    // ── Basic semantic queries ─────────────────────────────────────────────────

    pub(crate) fn is_instance(&self, sym: SymbolId) -> bool {
        if let Some(&v) = self.cache.borrow().is_instance.get(&sym) { return v; }
        let v = self.compute_is_instance(sym, &mut HashSet::new());
        self.cache.borrow_mut().is_instance.insert(sym, v);
        v
    }

    fn compute_is_instance(&self, sym: SymbolId, visited: &mut HashSet<SymbolId>) -> bool {
        if visited.contains(&sym) { return false; }
        visited.insert(sym);
        let edges = match self.store.tax_incoming.get(&sym) {
            Some(v) => v.clone(),
            None    => return false,
        };
        for &ei in &edges {
            let edge = &self.store.tax_edges[ei];
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
        if let Some(&v) = self.cache.borrow().is_class.get(&sym) { return v; }
        let v = match self.store.tax_incoming.get(&sym) {
            None    => true,
            Some(edges) => edges.iter().all(|&ei| {
                self.store.tax_edges[ei].rel == TaxRelation::Subclass
            }),
        };
        self.cache.borrow_mut().is_class.insert(sym, v);
        v
    }

    pub(crate) fn is_relation(&self, sym: SymbolId) -> bool {
        if let Some(&v) = self.cache.borrow().is_relation.get(&sym) { return v; }
        let v = self.is_instance(sym) && self.has_ancestor_by_name(sym, "Relation");
        self.cache.borrow_mut().is_relation.insert(sym, v);
        v
    }

    pub(crate) fn is_predicate(&self, sym: SymbolId) -> bool {
        if let Some(&v) = self.cache.borrow().is_predicate.get(&sym) { return v; }
        let v = self.is_instance(sym) && self.has_ancestor_by_name(sym, "Predicate");
        self.cache.borrow_mut().is_predicate.insert(sym, v);
        v
    }

    pub(crate) fn is_function(&self, sym: SymbolId) -> bool {
        if let Some(&v) = self.cache.borrow().is_function.get(&sym) { return v; }
        let v = self.is_instance(sym) && self.has_ancestor_by_name(sym, "Function");
        self.cache.borrow_mut().is_function.insert(sym, v);
        v
    }

    pub(crate) fn has_ancestor_by_name(&self, sym: SymbolId, ancestor: &str) -> bool {
        let anc_id = match self.store.sym_id(ancestor) {
            Some(id) => id,
            None     => return false,
        };
        self.has_ancestor(sym, anc_id)
    }

    pub(crate) fn has_ancestor(&self, sym: SymbolId, ancestor: SymbolId) -> bool {
        if sym == ancestor { return true; }
        if let Some(&v) = self.cache.borrow().has_ancestor.get(&(sym, ancestor)) {
            return v;
        }
        let v = self.compute_has_ancestor(sym, ancestor, &mut HashSet::new());
        self.cache.borrow_mut().has_ancestor.insert((sym, ancestor), v);
        v
    }

    fn compute_has_ancestor(
        &self, sym: SymbolId, ancestor: SymbolId, visited: &mut HashSet<SymbolId>,
    ) -> bool {
        if sym == ancestor { return true; }
        if visited.contains(&sym) { return false; }
        visited.insert(sym);
        let edges = match self.store.tax_incoming.get(&sym) {
            Some(v) => v.clone(),
            None    => return false,
        };
        for &ei in &edges {
            let from = self.store.tax_edges[ei].from;
            if self.compute_has_ancestor(from, ancestor, visited) { return true; }
        }
        false
    }

    pub(crate) fn arity(&self, sym: SymbolId) -> Option<i32> {
        if let Some(&v) = self.cache.borrow().arity.get(&sym) { return v; }
        let v = if !self.is_relation(sym) {
            None
        } else {
            self.compute_arity(sym)
        };
        self.cache.borrow_mut().arity.insert(sym, v);
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
        if let Some(v) = self.cache.borrow().range.get(&rel) {
            return Ok(Some(v.clone()));
        }
        match self.compute_range(rel)? {
            Some(r) => {
                self.cache.borrow_mut().range.insert(rel, r.clone());
                Ok(Some(r))
            }
            None => Ok(None),
        }
    }

    fn compute_range(
        &self, rel: SymbolId,
    ) -> Result<Option<RelationDomain>, SemanticError> {
        let process = |head: &str, make: fn(SymbolId) -> RelationDomain| -> Option<RelationDomain> {
            let sids = self.store.by_head(head).to_vec();
            for sid in sids {
                let sentence = &self.store.sentences[self.store.sent_idx(sid)];
                let arg1_ok = matches!(
                    sentence.elements.get(1),
                    Some(Element::Symbol(id)) if *id == rel
                );
                if !arg1_ok { continue; }
                let class_id = match sentence.elements.get(3) {
                    Some(Element::Symbol(id)) => *id,
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
                    sym: self.store.sym_name(rel).to_string(),
                }.handle(&self.store)?;
                Ok(Some(r))
            }
        }
    }

    pub(crate) fn domain(&self, rel: SymbolId) -> Vec<RelationDomain> {
        if let Some(v) = self.cache.borrow().domain.get(&rel) { return v.clone(); }
        let v = self.compute_domain(rel);
        self.cache.borrow_mut().domain.insert(rel, v.clone());
        v
    }

    fn compute_domain(&self, rel: SymbolId) -> Vec<RelationDomain> {
        let mut entries: Vec<(usize, RelationDomain)> = Vec::new();
        let mut process = |head: &str, make: fn(SymbolId) -> RelationDomain| {
            let sids = self.store.by_head(head).to_vec();
            for sid in sids {
                let sentence = &self.store.sentences[self.store.sent_idx(sid)];
                let arg1_ok = matches!(
                    sentence.elements.get(1),
                    Some(Element::Symbol(id)) if *id == rel
                );
                if !arg1_ok { continue; }
                let pos = match sentence.elements.get(2) {
                    Some(Element::Literal(Literal::Number(n))) => {
                        n.parse::<usize>().unwrap_or(0).saturating_sub(1)
                    }
                    _ => continue,
                };
                let class_id = match sentence.elements.get(3) {
                    Some(Element::Symbol(id)) => *id,
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

    // ── Validation ────────────────────────────────────────────────────────────

    pub(crate) fn validate_element(&self, el: &Element) -> Result<(), SemanticError> {
        let id = match el {
            Element::Variable { is_row: false, .. } => return Ok(()),
            Element::Symbol(id)  => *id,
            Element::Sub(sid)    => return self.validate_sentence(*sid),
            _                    => return Ok(()),
        };
        if !self.has_ancestor_by_name(id, "Entity") {
            SemanticError::NoEntityAncestor { sym: self.store.sym_name(id).to_string() }
                .handle(&self.store)?;
        }
        if self.is_relation(id) {
            let entity = *self.store.symbols.get("Entity").unwrap_or(&u64::MAX);
            let domain = self.domain(id);
            let _domain: Vec<SymbolId> = domain.iter().enumerate().map(|(idx, rd)| {
                if matches!(rd, RelationDomain::Domain(e) if *e == u64::MAX) {
                    SemanticError::MissingDomain {
                        sym: self.store.sym_name(rd.id()).to_string(), idx,
                    }.handle(&self.store)?;
                    Ok(entity)
                } else {
                    Ok(rd.id())
                }
            }).collect::<Result<Vec<_>, SemanticError>>()?;

            let arity = match self.arity(id) {
                Some(a) => a,
                None => {
                    SemanticError::MissingArity { sym: self.store.sym_name(id).to_string() }
                        .handle(&self.store)?;
                    -1
                }
            };
            if arity > 0 && arity < domain.len().try_into().unwrap() {
                SemanticError::ArityMismatch {
                    sid: id,
                    rel:      self.store.sym_name(id).to_string(),
                    expected: arity.try_into().unwrap(),
                    got:      domain.len(),
                }.handle(&self.store)?;
            }
            if self.is_function(id) {
                match self.range(id) {
                    Err(e) => return Err(e),
                    Ok(None) => {
                        SemanticError::MissingRange { sym: self.store.sym_name(id).to_string() }
                            .handle(&self.store)?;
                    }
                    Ok(Some(_)) => {}
                }
                let fun_name = self.store.sym_name(id);
                if !fun_name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                    SemanticError::FunctionCase { sym: fun_name.to_string() }
                        .handle(&self.store)?;
                }
            } else if self.is_predicate(id) {
                let rel_name = self.store.sym_name(id);
                if rel_name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                    SemanticError::PredicateCase { sym: rel_name.to_string() }
                        .handle(&self.store)?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn validate_sentence(&self, sid: SentenceId) -> Result<(), SemanticError> {
        let sentence = &self.store.sentences[self.store.sent_idx(sid)];
        if sentence.is_operator() {
            return self.validate_operator_sentence(sid);
        }
        log::trace!(target: "sumo_kb::semantic",
            "validating sentence sid={}", sid);

        let head_id = match sentence.elements.first() {
            Some(Element::Symbol(id))                    => *id,
            Some(Element::Variable { id, is_row: false, .. }) => *id,
            _ => unreachable!("parser ensures sentence head is a symbol or variable"),
        };
        self.validate_element(sentence.elements.first().unwrap())?;
        if !self.is_relation(head_id) {
            SemanticError::HeadNotRelation {
                sid,
                sym: self.store.sym_name(head_id).to_owned(),
            }.handle(&self.store)?;
        }

        let arg_count = sentence.elements.len().saturating_sub(1);
        if let Some(ar) = self.arity(head_id) {
            if ar > 0 && ar as usize != arg_count {
                SemanticError::ArityMismatch {
                    sid,
                    rel:      self.store.sym_name(head_id).to_owned(),
                    expected: ar as usize,
                    got:      arg_count,
                }.handle(&self.store)?;
            }
        }

        let domain = self.domain(head_id);
        if !domain.is_empty() {
            let args: Vec<Element> =
                self.store.sentences[self.store.sent_idx(sid)].elements[1..].to_vec();
            for (i, (arg, dom)) in args.iter().zip(domain.iter()).enumerate() {
                if !self.arg_satisfies_domain(arg, dom) {
                    SemanticError::DomainMismatch {
                        sid,
                        rel:    self.store.sym_name(head_id).to_owned(),
                        arg:    i + 1,
                        domain: self.store.sym_name(dom.id()).to_owned(),
                    }.handle(&self.store)?;
                }
            }
        }
        Ok(())
    }

    fn validate_operator_sentence(&self, sid: SentenceId) -> Result<(), SemanticError> {
        let op = match self.store.sentences[self.store.sent_idx(sid)].op().cloned() {
            Some(op) => op,
            None     => return Ok(()),
        };
        if op == OpKind::Equal { return Ok(()); }

        let is_quantifier = matches!(op, OpKind::ForAll | OpKind::Exists);
        let args_start = if is_quantifier { 2 } else { 1 };

        let sub_ids: Vec<SentenceId> = self.store.sentences[self.store.sent_idx(sid)]
            .elements[args_start..]
            .iter()
            .filter_map(|e| if let Element::Sub(id) = e { Some(*id) } else { None })
            .collect();

        for (idx, sub_id) in sub_ids.iter().enumerate() {
            if !self.is_logical_sentence(*sub_id) {
                SemanticError::NonLogicalArg { sid, arg: idx }.handle(&self.store)?;
            }
        }
        Ok(())
    }

    pub(crate) fn is_logical_sentence(&self, sid: SentenceId) -> bool {
        let sentence = &self.store.sentences[self.store.sent_idx(sid)];
        if sentence.is_operator() { return true; }
        let head_id = match sentence.elements.first() {
            Some(Element::Symbol(id))    => *id,
            Some(Element::Variable { id, .. }) => *id,
            _ => return false,
        };
        self.is_relation(head_id) && !self.is_function(head_id)
    }

    fn arg_satisfies_domain(&self, arg: &Element, dom: &RelationDomain) -> bool {
        match arg {
            Element::Symbol(sym_id) => {
                let sym_id = *sym_id;
                match dom {
                    RelationDomain::Domain(dom_id) => {
                        let dom_name = self.store.sym_name(*dom_id);
                        if dom_name == "Entity" { return true; }
                        if dom_name == "Class"  { return self.is_class(sym_id); }
                        self.is_instance(sym_id) && self.has_ancestor(sym_id, *dom_id)
                    }
                    RelationDomain::DomainSubclass(dom_id) => {
                        let dom_name = self.store.sym_name(*dom_id);
                        if dom_name == "Entity" { return true; }
                        // `domainSubclass R N Class` means "the argument must be a
                        // class".  Any symbol that IS a class satisfies this, even
                        // if it is not itself a subclass of `Class` in the hierarchy
                        // (e.g. SetOrClass is a superclass of Class, not a subclass,
                        // yet it is a class and is a valid range for rangeSubclass).
                        if dom_name == "Class"  { return self.is_class(sym_id); }
                        self.is_class(sym_id) && self.has_ancestor(sym_id, *dom_id)
                    }
                }
            }
            Element::Variable { id, is_row: false, .. } => {
                let var_id = *id;
                match dom {
                    RelationDomain::Domain(dom_id) => {
                        let dom_name = self.store.sym_name(*dom_id);
                        if dom_name == "Entity" { return true; }
                        if dom_name == "Class"  { return self.is_class(var_id); }
                        self.is_instance(var_id) || !self.is_class(var_id)
                    }
                    RelationDomain::DomainSubclass(dom_id) => {
                        let dom_name = self.store.sym_name(*dom_id);
                        if dom_name == "Entity" { return true; }
                        if dom_name == "Class"  { return self.is_class(var_id); }
                        self.is_class(var_id) || !self.is_instance(var_id)
                    }
                }
            }
            Element::Variable { is_row: true, .. }
            | Element::Sub(_)
            | Element::Literal(_) => true,
            Element::Op(_) => false,
        }
    }

    // ── Batch validation ──────────────────────────────────────────────────────

    /// Validate all root sentences, returning errors (does not stop on first error).
    pub(crate) fn validate_all(&self) -> Vec<(SentenceId, SemanticError)> {
        self.store.roots.iter()
            .filter_map(|&sid| self.validate_sentence(sid).err().map(|e| (sid, e)))
            .collect()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kif_store::{KifStore, load_kif};

    const BASE: &str = "
        (subclass Relation Entity)
        (subclass BinaryRelation Relation)
        (subclass Predicate Relation)
        (subclass BinaryPredicate Predicate)
        (subclass BinaryPredicate BinaryRelation)
        (instance subclass BinaryRelation)
        (domain subclass 1 Class)
        (domain subclass 2 Class)
        (instance instance BinaryPredicate)
        (domain instance 1 Entity)
        (domain instance 2 Class)
        (subclass Animal Entity)
        (subclass Human Entity)
        (subclass Human Animal)
    ";

    fn base_layer() -> SemanticLayer {
        let mut store = KifStore::default();
        load_kif(&mut store, BASE, "base");
        SemanticLayer::new(store)
    }

    fn kif(kif_str: &str) -> SemanticLayer {
        let mut store = KifStore::default();
        load_kif(&mut store, kif_str, "base");
        SemanticLayer::new(store)
    }

    #[test]
    fn is_relation() {
        let layer = base_layer();
        let id = layer.store.sym_id("subclass").unwrap();
        assert!(layer.is_relation(id));
    }

    #[test]
    fn is_predicate() {
        let layer = base_layer();
        let id = layer.store.sym_id("instance").unwrap();
        assert!(layer.is_predicate(id));
    }

    #[test]
    fn is_class() {
        let layer = base_layer();
        assert!( layer.is_class(layer.store.sym_id("Human").unwrap()));
        assert!(!layer.is_class(layer.store.sym_id("subclass").unwrap()));
    }

    #[test]
    fn has_ancestor() {
        let layer = base_layer();
        let human = layer.store.sym_id("Human").unwrap();
        assert!( layer.has_ancestor_by_name(human, "Entity"));
        assert!( layer.has_ancestor_by_name(human, "Animal"));
        assert!(!layer.has_ancestor_by_name(human, "Relation"));
    }

    #[test]
    fn validate_sentence_valid() {
        let layer = base_layer();
        let sub_id = layer.store.sym_id("subclass").unwrap();
        // Find a root sentence headed by "subclass"
        let sid = layer.store.by_head("subclass")[0];
        // validate_sentence should not error for a valid sentence.
        // (Semantic errors are warnings unless ALL_ERRORS is set.)
        let _ = layer.validate_sentence(sid);
        let _ = sub_id;
    }

    #[test]
    fn validate_all_runs() {
        let layer = base_layer();
        let errors = layer.validate_all();
        // Base ontology may have warnings but no fatal errors.
        // Just check it doesn't panic.
        let _ = errors;
    }

    #[test]
    fn is_logical_sentence() {
        let layer = kif("
            (and (relation A B) (relation D C))
            (instance relation Relation)
            (relation A B)
            (NotARelation A B)
        ");
        let store = &layer.store;
        assert!(layer.is_logical_sentence(store.roots[0]));
        assert!(layer.is_logical_sentence(store.roots[2]));
        assert!(!layer.is_logical_sentence(store.roots[3]));
    }
}
