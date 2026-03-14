/// KnowledgeBase — semantic layer on top of KifStore.
///
/// Semantic caches use `RefCell` so that query methods take `&self`, allowing
/// the TPTP generator to hold `&self.store` and call semantic methods at the
/// same time without borrow-checker conflicts.
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::{SentenceDisplay, Span};
use crate::error::{ParseError, SemanticError};
use crate::store::{Element, KifStore, Literal, SentenceId, SymbolId, TaxRelation, load_kif};
use crate::tokenizer::OpKind;

use log;

// ── Semantic cache ────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct SemanticCache {
    is_instance:     HashMap<SymbolId, bool>,
    is_class:        HashMap<SymbolId, bool>,
    is_relation:     HashMap<SymbolId, bool>,
    is_predicate:    HashMap<SymbolId, bool>,
    is_function:     HashMap<SymbolId, bool>,
    has_ancestor:    HashMap<(SymbolId, SymbolId), bool>,
    arity:           HashMap<SymbolId, Option<i32>>,
    domain:          HashMap<SymbolId, Vec<RelationDomain>>,
    range:           HashMap<SymbolId, RelationDomain>,
}

// Domain tracker
#[derive(Debug, Clone)]
pub enum RelationDomain {
    Domain(SymbolId),
    DomainSubclass(SymbolId),
}

impl RelationDomain {
    fn id(&self) -> SymbolId {
        match self {
            Self::Domain(id) | Self::DomainSubclass(id) => *id,
        }
    }
}

// ── KnowledgeBase ─────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct KnowledgeBase {
    pub store:          KifStore,
    cache:              RefCell<SemanticCache>,
    kb_validated:       RefCell<bool>,
    /// session_key → ordered list of assertion file tags added via `tell`.
    sessions:           HashMap<String, Vec<String>>,
    assertion_counter:  u32,
}

impl KnowledgeBase {
    pub fn new(store: KifStore) -> Self {
        Self {
            store,
            cache: RefCell::new(SemanticCache::default()),
            kb_validated: RefCell::new(false),
            sessions: HashMap::new(),
            assertion_counter: 0,
        }
    }

    // ── KIF loading ───────────────────────────────────────────────────────────

    pub fn load_kif(&mut self, text: &str, file: &str) -> Vec<(Span, ParseError)> {
        *self.kb_validated.borrow_mut() = false;
        *self.cache.borrow_mut() = SemanticCache::default();
        load_kif(&mut self.store, text, file)
    }

    // ── Semantic queries (all &self via RefCell) ──────────────────────────────

    pub fn is_instance(&self, sym: SymbolId) -> bool {
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

    pub fn is_class(&self, sym: SymbolId) -> bool {
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

    pub fn is_relation(&self, sym: SymbolId) -> bool {
        if let Some(&v) = self.cache.borrow().is_relation.get(&sym) { return v; }
        let v = self.is_instance(sym) && self.has_ancestor_by_name(sym, "Relation");
        self.cache.borrow_mut().is_relation.insert(sym, v);
        v
    }

    pub fn is_predicate(&self, sym: SymbolId) -> bool {
        if let Some(&v) = self.cache.borrow().is_predicate.get(&sym) { return v; }
        let v = self.is_instance(sym) && self.has_ancestor_by_name(sym, "Predicate");
        self.cache.borrow_mut().is_predicate.insert(sym, v);
        v
    }

    pub fn is_function(&self, sym: SymbolId) -> bool {
        if let Some(&v) = self.cache.borrow().is_function.get(&sym) { return v; }
        let v = self.is_instance(sym) && self.has_ancestor_by_name(sym, "Function");
        self.cache.borrow_mut().is_function.insert(sym, v);
        v
    }

    pub fn has_ancestor_by_name(&self, sym: SymbolId, ancestor: &str) -> bool {
        let anc_id = match self.store.sym_id(ancestor) {
            Some(id) => id,
            None     => return false,
        };
        self.has_ancestor(sym, anc_id)
    }

    pub fn has_ancestor(&self, sym: SymbolId, ancestor: SymbolId) -> bool {
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

    pub fn arity(&self, sym: SymbolId) -> Option<i32> {
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

    pub fn range(&self, rel: SymbolId) -> Result<Option<RelationDomain>, SemanticError> {
        if let Some(v) = self.cache.borrow().range.get(&rel) { return Ok(Some(v.clone())); }
        let v = self.compute_range(rel);
        match v {
            Ok(Some(r)) => {
                self.cache.borrow_mut().range.insert(rel, r.clone());
                Ok(Some(r))
            },
            Ok(None) => Ok(None),
            Err(e) => Err(e)
        }
    }

    fn compute_range(&self, rel: SymbolId) -> Result<Option<RelationDomain>, SemanticError> {
        // Get domain relations
        let process= |head: &str, make_variant: fn(SymbolId) -> RelationDomain| -> Option<RelationDomain> {
            let sids = self.store.by_head(head).to_vec();
            for sid in sids {
                let sentence = &self.store.sentences[sid as usize];
                let arg1_ok = matches!(
                    sentence.elements.get(1),
                    Some(Element::Symbol(id)) if *id == rel
                );
                if !arg1_ok { continue; }
                let class_id = match sentence.elements.get(3) {
                    Some(Element::Symbol(id)) => *id,
                    _ => continue,
                };
                return Some(make_variant(class_id));
            };
            return None
        };

        let range = process("range",         RelationDomain::Domain);
        let range_subclass = process("rangeSubclass", RelationDomain::DomainSubclass);
        if range.is_none() {
            if range_subclass.is_none() {
                Ok(None)
            } else {
                Ok(Some(range_subclass.unwrap()))
            }
        } else if range_subclass.is_none() {
            Ok(Some(range.unwrap()))
        } else {
            SemanticError::DoubleRange { sym: self.store.sym_name(rel).to_string() }.handle(&self.store)?;
            Ok(Some(range.unwrap()))
        }
    }

    pub fn domain(&self, rel: SymbolId) -> Vec<RelationDomain> {
        if let Some(v) = self.cache.borrow().domain.get(&rel) { return v.clone(); }
        let v = self.compute_domain(rel);
        self.cache.borrow_mut().domain.insert(rel, v.clone());
        v
    }

    fn compute_domain(&self, rel: SymbolId) -> Vec<RelationDomain> {
        // Get domain relations
        let mut entries: Vec<(usize, RelationDomain)> = Vec::new();
        let mut process = |head: &str, make_variant: fn(SymbolId) -> RelationDomain| {
            let sids = self.store.by_head(head).to_vec();
            for sid in sids {
                let sentence = &self.store.sentences[sid as usize];
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
                entries.push((pos, make_variant(class_id)));
            }
        };

        process("domain",         RelationDomain::Domain);
        process("domainSubclass", RelationDomain::DomainSubclass);
        entries.sort_by_key(|&(p, _)| p);
        let max = entries.iter().map(|&(p, _)| p).max().map(|p| p + 1).unwrap_or(0);
        let mut result = vec![RelationDomain::Domain(u64::MAX); max];
        for (pos, id) in entries {
            if pos < max { result[pos] = id; }
        }
        result
    }

    // Element Validation
    pub fn validate_element(&self, el: &Element) -> Result<(), SemanticError> {
        let id = match el {
            Element::Variable { is_row: false, ..} => return Ok(()),
            Element::Symbol(id) => *id,
            Element::Sub(sid) => return self.validate_sentence(*sid),
            _ => return Ok(())
        };
        // Check if the symbol has Entity as its ancestor
        if !self.has_ancestor_by_name(id, "Entity") {
            SemanticError::NoEntityAncestor { sym: self.store.sym_name(id).to_string() }.handle(&self.store)?
        }
        if self.is_relation(id) {
            // Check arity
            let entity = self.store.symbols.get("Entity").unwrap();
            let domain = self.domain(id);
            let domain: Vec<u64> = domain.iter().enumerate().map(|(idx, id)| {
                if matches!(id, RelationDomain::Domain(u64::MAX)) {
                    SemanticError::MissingDomain { sym: self.store.sym_name(id.id()).to_string(), idx }.handle(&self.store)?;
                    Ok(*entity)
                } else {
                    Ok(id.id())
                }
            }).collect::<Result<Vec<u64>, SemanticError>>()?;

            let arity = match self.arity(id) {
                Some(a) => a,
                None => {
                    SemanticError::MissingArity { sym: self.store.sym_name(id).to_string() }.handle(&self.store)?;
                    -1
                },
            };
            if arity > 0 && arity < domain.len().try_into().unwrap() {
                SemanticError::ArityMismatch { sid: id, rel: self.store.sym_name(id).to_string(), expected: arity.try_into().unwrap(), got: domain.len() }.handle(&self.store)?;
            }

            // Functions must declare a range
            if self.is_function(id) {
                let range = self.range(id);
                match range {
                    Err(e) => return Err(e),
                    Ok(None) => { SemanticError::MissingRange { sym: self.store.sym_name(id).to_string() }.handle(&self.store)? },
                    Ok(Some(..)) => {}
                };
                let fun_name = self.store.sym_name(id);
                // Functions should start with an uppercase
                if !fun_name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                    SemanticError::FunctionCase { sym: fun_name.to_string() }.handle(&self.store)?;
                }
            } else if self.is_predicate(id) {
                let rel_name = self.store.sym_name(id);
                // Functions should start with an uppercase
                if rel_name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                    SemanticError::PredicateCase { sym: rel_name.to_string() }.handle(&self.store)?;

                }
            }
        }
        Ok(())
    }

    // Formula validation
    pub fn validate_sentence(&self, sid: SentenceId) -> Result<(), SemanticError> {
        let sentence = &self.store.sentences[sid as usize];

        if sentence.is_operator() {
            return self.validate_operator_sentence(sid);
        }

        log::trace!("Validating sentence {}:\n{}", sid, SentenceDisplay::new(sid, &self.store));
        
        // Rule 2: head must be a declared relation
        let head_id = match sentence.elements.first() {
            Some(Element::Symbol(id)) => *id,
            Some(Element::Variable { id, is_row: false, ..}) => *id,
            _ => unreachable!("The parser should have already validated that sentences start with a valid term"), // This should never get hit
        };
        // Validate the head
        self.validate_element(sentence.elements.first().unwrap())?;
        // Validate the relation element
        if !self.is_relation(head_id) {
            SemanticError::HeadNotRelation {
                sid,
                sym: self.store.sym_name(head_id).to_owned(),
            }.handle(&self.store)?;
        }

        // Rule 3: arity check
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

        // Rule 3b: domain check (only when domain declarations exist)
        let domain = self.domain(head_id);
        if !domain.is_empty() {
            let args: Vec<Element> = sentence.elements[1..].to_vec();
            for (i, (arg, dom)) in args.iter().zip(domain.iter()).enumerate() {
                if !self.arg_satisfies_domain(arg, dom) {
                    let dom_id = dom.id();
                    SemanticError::DomainMismatch {
                        sid,
                        rel:    self.store.sym_name(head_id).to_owned(),
                        arg:    i + 1,
                        domain: self.store.sym_name(dom_id).to_owned(),
                    }.handle(&self.store)?;
                }
            }
        }

        Ok(())
    }

    fn validate_operator_sentence(&self, sid: SentenceId) -> Result<(), SemanticError> {
        let op = match self.store.sentences[sid as usize].op().cloned() {
            Some(op) => op,
            None     => return Ok(()),
        };
        if op == OpKind::Equal { return Ok(()); }

        let is_quantifier = matches!(op, OpKind::ForAll | OpKind::Exists);
        // For quantifiers: skip op-element (0) and var-list (1); body starts at 2.
        // For others: skip only op-element (0).
        let args_start = if is_quantifier { 2 } else { 1 };

        // Check for type conflicts among all typed subjects in this scope.
        // `sid` is the root operator, so it doubles as the variable scope key.
        // self.check_type_conflicts(sid, sid)?;

        let sub_ids: Vec<SentenceId> = self.store.sentences[sid as usize]
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

    /// Returns true if `sid` is a truth-valued sentence (operator, or headed by
    /// a relation that is not a function).  `scope` is the root operator sentence
    /// ID used to resolve scoped variable names (`?VAR@{scope}`).
    pub fn is_logical_sentence(&self, sid: SentenceId) -> bool {
        let sentence = &self.store.sentences[sid as usize];
        if sentence.is_operator() { return true; }
        let head_id = match sentence.elements.first() {
            Some(Element::Symbol(id)) => *id,
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
                    },
                    RelationDomain::DomainSubclass(dom_id) => {
                        let dom_name = self.store.sym_name(*dom_id);
                        if dom_name == "Entity" { return true; }
                        self.is_class(sym_id) && self.has_ancestor(sym_id, *dom_id)
                    }
                }
            }
            // Variables and sub-sentences are always accepted (polymorphic)
            Element::Variable { id, is_row: false, .. } => {
                let var_id = *id;
                match dom {
                    RelationDomain::Domain(dom_id) => {
                        let dom_name = self.store.sym_name(*dom_id);
                        if dom_name == "Entity" { return true; }
                        if dom_name == "Class"  { return self.is_class(var_id); }
                        self.is_instance(var_id) || !self.is_class(var_id)
                    },
                    RelationDomain::DomainSubclass(dom_id) => {
                        let dom_name = self.store.sym_name(*dom_id);
                        if dom_name == "Entity" { return true; }
                        self.is_class(var_id) || !self.is_instance(var_id)
                    }
                }
            },
            Element::Variable { is_row: true, .. } | Element::Sub(_) | Element::Literal(_) => true,
            Element::Op(_) => false,
        }
    }

    // ── One-time KB validation (warms caches) ─────────────────────────────────

    pub fn validate_kb_once(&self) {
        if *self.kb_validated.borrow() { return; }
        let root_ids: Vec<SentenceId> = self.store.roots.clone();
        for sid in root_ids {
            let _ = self.validate_sentence(sid); // pre-existing errors are non-fatal
        }
        *self.kb_validated.borrow_mut() = true;
    }

    // ── tell() ────────────────────────────────────────────────────────────────

    pub fn tell(&mut self, session: &str, kif_text: &str) -> TellResult {
        self.assertion_counter += 1;
        let tag = format!("__assertion_{}_{}__", session, self.assertion_counter);

        let prev_sym_names: HashSet<String> =
            self.store.symbols.keys().cloned().collect();

        // 1. Parse into the store
        let errors = load_kif(&mut self.store, kif_text, &tag);
        let hard: Vec<String> = errors.iter().map(|e| e.1.to_string()).collect();
        if !hard.is_empty() {
            self.rollback(&tag, &prev_sym_names);
            return TellResult { ok: false, errors: hard, sentence_id: None };
        }

        // 2. Find the newly added root sentence(s)
        let new_roots: Vec<SentenceId> = self
            .store
            .file_roots
            .get(&tag)
            .cloned()
            .unwrap_or_default();

        if new_roots.is_empty() {
            self.rollback(&tag, &prev_sym_names);
            return TellResult {
                ok:     false,
                errors: vec!["No sentences parsed from input".into()],
                sentence_id: None,
            };
        }
        if new_roots.len() > 1 {
            self.rollback(&tag, &prev_sym_names);
            return TellResult {
                ok: false,
                errors: vec![format!(
                    "tell() accepts exactly one formula at a time; got {}",
                    new_roots.len()
                )],
                sentence_id: None,
            };
        }

        let sid = new_roots[0];

        // 3. Warm KB caches
        self.validate_kb_once();

        // 4. Strictly validate the new sentence
        if let Err(e) = self.validate_sentence(sid) {
            self.rollback(&tag, &prev_sym_names);
            return TellResult {
                ok:     false,
                errors: vec![e.to_string()],
                sentence_id: None,
            };
        }

        // 5. Commit
        self.sessions.entry(session.to_owned()).or_default().push(tag);
        TellResult { ok: true, errors: Vec::new(), sentence_id: Some(sid) }
    }

    fn rollback(&mut self, tag: &str, prev_sym_names: &HashSet<String>) {
        // Invalidate caches for new symbols about to be removed
        let new_ids: Vec<SymbolId> = self
            .store
            .symbols
            .iter()
            .filter_map(|(name, &id)| {
                if !prev_sym_names.contains(name.as_str()) { Some(id) } else { None }
            })
            .collect();
        {
            let mut cache = self.cache.borrow_mut();
            for id in new_ids {
                cache.is_instance.remove(&id);
                cache.is_class.remove(&id);
                cache.is_relation.remove(&id);
                cache.is_predicate.remove(&id);
                cache.is_function.remove(&id);
            }
        }
        self.store.remove_file(tag);
    }

    // ── flush() ───────────────────────────────────────────────────────────────

    /// Remove all assertions from every session.
    pub fn flush(&mut self) {
        let all_sessions = std::mem::take(&mut self.sessions);
        for (_, tags) in all_sessions {
            for tag in tags {
                self.store.remove_file(&tag);
            }
        }
        *self.cache.borrow_mut() = SemanticCache::default();
    }

    /// Remove all assertions belonging to `session`.
    pub fn flush_session(&mut self, session: &str) {
        let tags = self.sessions.remove(session).unwrap_or_default();
        for tag in tags {
            self.store.remove_file(&tag);
        }
        *self.cache.borrow_mut() = SemanticCache::default();
    }

    // ── Batch validation ──────────────────────────────────────────────────────

    /// Validate all root sentences, returning `(SentenceId, SemanticError)` for
    /// every formula that fails.  Unlike [`validate_kb_once`], errors are not
    /// discarded — they are returned to the caller for reporting.
    pub fn validate_all(&self) -> Vec<(crate::store::SentenceId, SemanticError)> {
        self.store
            .roots
            .iter()
            .filter_map(|&sid| self.validate_sentence(sid).err().map(|e| (sid, e)))
            .collect()
    }

    // ── Helpers for TPTP layer ────────────────────────────────────────────────

    /// Assertion file tags for a specific session (empty slice if unknown).
    pub fn session_tags(&self, session: &str) -> &[String] {
        self.sessions.get(session).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// All known session keys.
    pub fn session_keys(&self) -> impl Iterator<Item = &str> {
        self.sessions.keys().map(|s| s.as_str())
    }

    /// Sentence IDs for assertions in every session (used when no session filter).
    pub fn assertion_sentence_ids(&self) -> Vec<SentenceId> {
        self.sessions
            .values()
            .flat_map(|tags| {
                tags.iter()
                    .flat_map(|tag| self.store.file_roots.get(tag).cloned().unwrap_or_default())
            })
            .collect()
    }

    /// Sentence IDs for assertions belonging to a specific session.
    pub fn assertion_sentence_ids_for_session(&self, session: &str) -> Vec<SentenceId> {
        self.sessions
            .get(session)
            .into_iter()
            .flat_map(|tags| {
                tags.iter()
                    .flat_map(|tag| self.store.file_roots.get(tag).cloned().unwrap_or_default())
            })
            .collect()
    }
}

// ── TellResult ────────────────────────────────────────────────────────────────

pub struct TellResult {
    pub ok:          bool,
    pub errors:      Vec<String>,
    pub sentence_id: Option<SentenceId>,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::KifStore;

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

    fn base_kb() -> KnowledgeBase {
        let mut store = KifStore::default();
        load_kif(&mut store, BASE, "base");
        KnowledgeBase::new(store)
    }

    #[test]
    fn is_relation() {
        let kb = base_kb();
        let id = kb.store.sym_id("subclass").unwrap();
        assert!(kb.is_relation(id));
    }

    #[test]
    fn is_predicate() {
        let kb = base_kb();
        let id = kb.store.sym_id("instance").unwrap();
        assert!(kb.is_predicate(id));
    }

    #[test]
    fn is_class() {
        let kb = base_kb();
        assert!(kb.is_class(kb.store.sym_id("Human").unwrap()));
        assert!(!kb.is_class(kb.store.sym_id("subclass").unwrap()));
    }

    #[test]
    fn has_ancestor() {
        let kb = base_kb();
        let human = kb.store.sym_id("Human").unwrap();
        assert!(kb.has_ancestor_by_name(human, "Entity"));
        assert!(kb.has_ancestor_by_name(human, "Animal"));
        assert!(!kb.has_ancestor_by_name(human, "Relation"));
    }

    #[test]
    fn tell_valid() {
        let mut kb = base_kb();
        let r = kb.tell("s1", "(subclass Cat Animal)");
        assert!(r.ok, "errors: {:?}", r.errors);
        assert_eq!(kb.session_tags("s1").len(), 1);
    }

    #[test]
    fn tell_invalid_head() {
        let mut kb = base_kb();
        let r = kb.tell("s1", "(UnknownPred Cat Animal)");
        assert!(!r.ok);
        assert_eq!(kb.session_tags("s1").len(), 0);
    }

    #[test]
    fn tell_parse_error() {
        let mut kb = base_kb();
        let r = kb.tell("s1", "(subclass Cat");
        assert!(!r.ok);
    }

    #[test]
    fn tell_multiple_sentences() {
        let mut kb = base_kb();
        let r = kb.tell("s1", "(subclass Cat Animal) (subclass Dog Animal)");
        assert!(!r.ok);
        assert!(r.errors[0].contains("exactly one"));
    }

    #[test]
    fn flush_restores_state() {
        let mut kb = base_kb();
        let before = kb.store.roots.len();
        kb.tell("s1", "(subclass Cat Animal)");
        assert_eq!(kb.store.roots.len(), before + 1);
        kb.flush();
        assert_eq!(kb.store.roots.len(), before);
        assert!(kb.session_tags("s1").is_empty());
    }

    #[test]
    fn flush_session_isolates() {
        let mut kb = base_kb();
        let before = kb.store.roots.len();
        kb.tell("s1", "(subclass Cat Animal)");
        kb.tell("s2", "(subclass Dog Animal)");
        assert_eq!(kb.store.roots.len(), before + 2);
        kb.flush_session("s1");
        assert_eq!(kb.store.roots.len(), before + 1);
        assert!(kb.session_tags("s1").is_empty());
        assert_eq!(kb.session_tags("s2").len(), 1);
    }

    #[test]
    fn conditional_with_logical_args() {
        let mut kb = base_kb();
        let r = kb.tell("s1", "(=> (instance ?X Human) (instance ?X Animal))");
        assert!(r.ok, "errors: {:?}", r.errors);
    }
}
