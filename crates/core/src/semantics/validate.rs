// crates/core/src/semantics/validate.rs
//
// Semantic validation layer (do the sentences adhere to SUMO semantics).

use crate::{Element, OpKind, SentenceId, SymbolId};

use super::SemanticLayer;
use super::relation::RelationDomain;
use super::errors::SemanticError;

// -- Validation ------------------------------------------------------------
impl SemanticLayer {
    fn validate_element(&self, el: &Element) -> Result<(), SemanticError> {
        let id = match el {
            Element::Variable { is_row: false, .. } => return Ok(()),
            Element::Symbol { id, .. }  => *id,
            Element::Sub { sid, .. }    => return self.validate_sentence(*sid),
            _                    => return Ok(()),
        };
        if !self.has_ancestor_by_name(id, "Entity") {
            SemanticError::NoEntityAncestor { sym: self.syntactic.sym_name(id).to_string() }
                .handle(&self.syntactic)?;
        }
        if self.is_relation(id) {
            let entity = *self.syntactic.symbols.get("Entity").unwrap_or(&u64::MAX);
            let domain = self.domain(id);
            let _domain: Vec<SymbolId> = domain.iter().enumerate().map(|(idx, rd)| {
                if matches!(rd, RelationDomain::Domain(e) if *e == u64::MAX) {
                    SemanticError::MissingDomain {
                        sym: self.syntactic.sym_name(rd.id()).to_string(), idx,
                    }.handle(&self.syntactic)?;
                    Ok(entity)
                } else {
                    Ok(rd.id())
                }
            }).collect::<Result<Vec<_>, SemanticError>>()?;

            let arity = match self.arity(id) {
                Some(a) => a,
                None => {
                    SemanticError::MissingArity { sym: self.syntactic.sym_name(id).to_string() }
                        .handle(&self.syntactic)?;
                    -1
                }
            };
            if arity > 0 && arity < domain.len().try_into().unwrap() {
                SemanticError::ArityMismatch {
                    sid: id,
                    rel:      self.syntactic.sym_name(id).to_string(),
                    expected: arity.try_into().unwrap(),
                    got:      domain.len(),
                }.handle(&self.syntactic)?;
            }
            if self.is_function(id) {
                match self.range(id) {
                    Err(e) => return Err(e),
                    Ok(None) => {
                        SemanticError::MissingRange { sym: self.syntactic.sym_name(id).to_string() }
                            .handle(&self.syntactic)?;
                    }
                    Ok(Some(_)) => {}
                }
                let fun_name = self.syntactic.sym_name(id);
                if !fun_name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                    SemanticError::FunctionCase { sym: fun_name.to_string() }
                        .handle(&self.syntactic)?;
                }
            } else if self.is_predicate(id) {
                let rel_name = self.syntactic.sym_name(id);
                if rel_name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                    SemanticError::PredicateCase { sym: rel_name.to_string() }
                        .handle(&self.syntactic)?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn validate_sentence(&self, sid: SentenceId) -> Result<(), SemanticError> {
        let sentence = &self.syntactic.sentences[self.syntactic.sent_idx(sid)];
        if sentence.is_operator() {
            return self.validate_operator_sentence(sid);
        }
        crate::emit_event!(crate::ProgressEvent::Log { level: crate::LogLevel::Trace, target: "sigmakee_rs_core::semantic", message: format!("validating sentence sid={}", sid) });

        let head_id = match sentence.elements.first() {
            Some(Element::Symbol { id, .. })                    => *id,
            Some(Element::Variable { id, is_row: false, .. }) => *id,
            _ => unreachable!("parser ensures sentence head is a symbol or variable"),
        };
        self.validate_element(sentence.elements.first().unwrap())?;
        if !self.is_relation(head_id) {
            SemanticError::HeadNotRelation {
                sid,
                sym: self.syntactic.sym_name(head_id).to_owned(),
            }.handle(&self.syntactic)?;
        }

        let arg_count = sentence.elements.len().saturating_sub(1);
        if let Some(ar) = self.arity(head_id) {
            if ar > 0 && ar as usize != arg_count {
                SemanticError::ArityMismatch {
                    sid,
                    rel:      self.syntactic.sym_name(head_id).to_owned(),
                    expected: ar as usize,
                    got:      arg_count,
                }.handle(&self.syntactic)?;
            }
        }

        let domain = self.domain(head_id);
        if !domain.is_empty() {
            let args: Vec<Element> =
                self.syntactic.sentences[self.syntactic.sent_idx(sid)].elements[1..].to_vec();
            for (i, (arg, dom)) in args.iter().zip(domain.iter()).enumerate() {
                if !self.arg_satisfies_domain(arg, dom) {
                    SemanticError::DomainMismatch {
                        sid,
                        rel:    self.syntactic.sym_name(head_id).to_owned(),
                        arg:    i + 1,
                        domain: self.syntactic.sym_name(dom.id()).to_owned(),
                    }.handle(&self.syntactic)?;
                }
            }
        }
        Ok(())
    }

    fn validate_operator_sentence(&self, sid: SentenceId) -> Result<(), SemanticError> {
        let sentence = match self.syntactic.sentences.get(self.syntactic.sent_idx(sid)) {
            Some(s) => s,
            None => return Ok(()),
        };
        let op: OpKind = match sentence.op().cloned() {
            Some(op) => op,
            None     => return Ok(()),
        };
        
        let arity = op.arity();
        if arity > 0 && arity != sentence.arity() {
            SemanticError::ArityMismatch { 
                sid, 
                rel: op.name().to_string(), 
                expected: arity, got: sentence.arity()
            }.handle(&self.syntactic)?
        }

        if matches!(op, OpKind::And | OpKind::Or) && sentence.arity() == 1 {
            SemanticError::SingleArity { sid }.handle(&self.syntactic)?;
        }

        if op == OpKind::Equal { return Ok(()); }

        let is_quantifier = matches!(op, OpKind::ForAll | OpKind::Exists);
        let args_start = if is_quantifier { 2 } else { 1 };

        let sub_ids: Vec<SentenceId> = self.syntactic.sentences[self.syntactic.sent_idx(sid)]
            .elements[args_start..]
            .iter()
            .filter_map(|e| if let Element::Sub { sid: id, .. } = e { Some(*id) } else { None })
            .collect();

        for (idx, sub_id) in sub_ids.iter().enumerate() {
            if !self.is_logical_sentence(*sub_id) {
                SemanticError::NonLogicalArg { sid, arg: idx + 1, op: op.to_string() }.handle(&self.syntactic)?;
            }
        }
        Ok(())
    }

    pub(crate) fn is_logical_sentence(&self, sid: SentenceId) -> bool {
        let sentence = &self.syntactic.sentences[self.syntactic.sent_idx(sid)];
        if sentence.is_operator() { return true; }
        let head_id = match sentence.elements.first() {
            Some(Element::Symbol { id, .. })    => *id,
            Some(Element::Variable { id, .. }) => *id,
            _ => return false,
        };
        // A sentence is logical if its head is a relation and not a function.
        // If the head is not declared in the taxonomy at all (unknown symbol, e.g. when
        // the full KB is not loaded), assume it is logical -- unknown != not-a-relation.
        // Only positively-declared functions are considered non-logical.
        self.is_relation(head_id) && !self.is_function(head_id)
    }

    fn arg_satisfies_domain(&self, arg: &Element, dom: &RelationDomain) -> bool {
        match arg {
            Element::Symbol { id: sym_id, .. } => {
                let sym_id = *sym_id;
                match dom {
                    RelationDomain::Domain(dom_id) => {
                        let dom_name = self.syntactic.sym_name(*dom_id);
                        if dom_name == "Entity" { return true; }
                        if dom_name == "Class"  { return self.is_class(sym_id); }
                        self.is_instance(sym_id) && self.has_ancestor(sym_id, *dom_id)
                    }
                    RelationDomain::DomainSubclass(dom_id) => {
                        let dom_name = self.syntactic.sym_name(*dom_id);
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
                        let dom_name = self.syntactic.sym_name(*dom_id);
                        if dom_name == "Entity" { return true; }
                        if dom_name == "Class"  { return self.is_class(var_id); }
                        self.is_instance(var_id) || !self.is_class(var_id)
                    }
                    RelationDomain::DomainSubclass(dom_id) => {
                        let dom_name = self.syntactic.sym_name(*dom_id);
                        if dom_name == "Entity" { return true; }
                        if dom_name == "Class"  { return self.is_class(var_id); }
                        self.is_class(var_id) || !self.is_instance(var_id)
                    }
                }
            }
            Element::Variable { is_row: true, .. }
            | Element::Sub { sid: _, .. }
            | Element::Literal { lit: _, .. } => true,
            Element::Op { op: _, .. } => false,
        }
    }
    
    // -- Batch validation ------------------------------------------------------

    /// Validate all root sentences, returning errors (does not stop on first error).
    pub(crate) fn validate_all(&self) -> Vec<(SentenceId, SemanticError)> {
        self.syntactic.roots.iter()
            .filter_map(|&sid| self.validate_sentence(sid).err().map(|e| (sid, e)))
            .collect()
    }

    /// Run `validate_sentence` and return *every* `SemanticError`
    /// it raises -- warnings and hard errors alike -- so the LSP
    /// can turn each into a diagnostic without caring about the
    /// CLI's severity config.
    ///
    /// Internally installs a thread-local collector via
    /// [`crate::error::with_collector`] so the existing
    /// `handle()`-based dispatch loop in `validate_sentence` /
    /// `validate_element` is reused verbatim.  The `handle()`
    /// calls push into the collector instead of swallowing the
    /// warning or returning the hard error, so every check in the
    /// chain runs to completion.
    pub(crate) fn validate_sentence_collect(&self, sid: SentenceId) -> Vec<SemanticError> {
        let (_, errs) = super::errors::with_collector(|| self.validate_sentence(sid));
        errs
    }
}