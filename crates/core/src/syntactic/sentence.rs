// crates/core/src/syntactic/sentence.rs
//
// Sentence allocation, AST -> Sentence build, and ScopeCtx for variable
// scope disambiguation.

use std::collections::HashMap;

use smallvec::SmallVec;

use crate::KbError;
use crate::parse::ast::{AstNode, OpKind, Span};
use crate::parse::fingerprint::sentence_fingerprint;
use crate::types::{Element, Literal, Sentence, SentenceId};

use super::SyntacticLayer;

impl SyntacticLayer {
    // -- Sentence helpers ------------------------------------------------------

    /// Resolve a SentenceId to its Vec index (panics if not found).
    #[inline]
    pub(crate) fn sent_idx(&self, sid: SentenceId) -> usize {
        self.sent_idx[&sid]
    }

    /// Return true if `sid` is a known sentence.
    #[inline]
    pub(crate) fn has_sentence(&self, sid: SentenceId) -> bool {
        self.sent_idx.contains_key(&sid)
    }

    /// Update the `Sentence.span` for `sid` to `new_span`, without
    /// changing any other fields.  Used by the incremental-reload
    /// path when a root sentence is textually unchanged but has
    /// shifted in its file.  Returns `true` when the sentence
    /// existed and was updated.
    pub(crate) fn update_sentence_span(&mut self, sid: SentenceId, new_span: Span) -> bool {
        if !self.sent_idx.contains_key(&sid) { return false; }
        let vec_idx = self.sent_idx(sid);
        self.sentences[vec_idx].span = new_span;
        true
    }

    // -- Sentence allocation ---------------------------------------------------

    pub(in crate::syntactic) fn alloc_sentence(&mut self, sentence: Sentence) -> SentenceId {
        let id  = self.next_sentence_id;
        let idx = self.sentences.len();
        self.next_sentence_id += 1;
        self.sentences.push(sentence);
        self.sent_idx.insert(id, idx);
        #[cfg(debug_assertions)]
        crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Trace, target: "sigmakee_rs_core::syntactic", message: format!("allocated sentence id={} at vec[{}]", id, idx) });
        id
    }

    pub(in crate::syntactic) fn next_scope(&mut self) -> u64 {
        let id = self.scope_counter;
        self.scope_counter += 1;
        id
    }

    // -- Load (syntax pass) ----------------------------------------------------

    /// Process a list of top-level AST nodes into this store, tagging them
    /// with `file`.  Returns any stoppable errors found.
    pub(crate) fn load(&mut self, nodes: &[AstNode], file: &str) -> Vec<(Span, KbError)> {
        let mut errors: Vec<(Span, KbError)> = Vec::new();
        for node in nodes {
            if let AstNode::List { .. } = node {
                let ctx = ScopeCtx { default: self.next_scope(), overrides: HashMap::new() };
                if let Some(sent_id) = self.build_sentence(&ctx, node, file, &mut errors, true) {
                    #[cfg(debug_assertions)]
                    crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Trace, target: "sigmakee_rs_core::syntactic", message: format!("registered root sentence id={}", sent_id) });
                    self.finalize_root(sent_id, node, file);
                }
            }
        }
        errors
    }

    /// Append a single already-parsed root AST node to `file`.
    ///
    /// Identical to what [`Self::load`] does for one list node, with the
    /// return value exposed for callers that need the allocated
    /// `SentenceId` to build their response.  Returns `None` when the
    /// node is not a list (malformed root).
    pub(crate) fn append_root_sentence(
        &mut self,
        node: &AstNode,
        file: &str,
        errors: &mut Vec<(Span, KbError)>,
    ) -> Option<SentenceId> {
        if !matches!(node, AstNode::List { .. }) { return None; }
        let ctx     = ScopeCtx { default: self.next_scope(), overrides: HashMap::new() };
        let sent_id = self.build_sentence(&ctx, node, file, errors, true)?;
        self.finalize_root(sent_id, node, file);
        Some(sent_id)
    }

    /// Shared post-build bookkeeping for a freshly-created root sentence.
    fn finalize_root(&mut self, sent_id: SentenceId, node: &AstNode, file: &str) {
        self.roots.push(sent_id);
        self.file_roots.entry(file.to_owned()).or_default().push(sent_id);
        let fp = sentence_fingerprint(node);
        self.file_hashes.entry(file.to_owned()).or_default().push(fp);
        if let Some(head_id) = self.sentences[self.sent_idx(sent_id)].head_symbol() {
            let head_name    = self.sym_name(head_id).to_owned();
            self.head_index.entry(head_name).or_default().push(sent_id);
            let head_vec_idx = self.sym_vec_idx(head_id);
            self.symbol_data[head_vec_idx].head_sentences.push(sent_id);
        }
        // Record every symbol reference in this sentence (and all
        // transitively-reached sub-sentences) in the reverse index.
        self.index_sentence_occurrences(sent_id);
    }

    fn build_sentence(
        &mut self,
        ctx: &ScopeCtx,
        node: &AstNode,
        file: &str,
        errors: &mut Vec<(Span, KbError)>,
        top_level: bool,
    ) -> Option<SentenceId> {
        let (elements_ast, span) = match node {
            AstNode::List { elements, span } => (elements, span.clone()),
            _ => return None,
        };

        if elements_ast.is_empty() {
            if top_level {
                unreachable!("The parser should have found and rejected empty sentences");
            } else {
                let sid = self.alloc_sentence(Sentence { elements: SmallVec::new(), file: file.to_owned(), span });
                return Some(sid);
            }
        }

        if top_level {
            let first = &elements_ast[0];
            if !matches!(first, AstNode::Symbol { .. } | AstNode::Variable { .. } | AstNode::RowVariable { .. } | AstNode::Operator { .. }) {
                unreachable!("The parser should have caught sentences which did not start with a symbol");
            }
        }

        for (i, el) in elements_ast.iter().enumerate() {
            if i > 0 {
                if let AstNode::Operator { .. } = el {
                    unreachable!("The parser should have caught sentences where the symbol did not appear in the first term of a sentence");
                }
            }
        }

        // If this is a quantifier, build a child context for its body.
        let child_ctx;
        let body_ctx = if matches!(elements_ast.get(0), Some(AstNode::Operator { op: OpKind::Exists | OpKind::ForAll, .. })) {
            let bound: Vec<String> = match elements_ast.get(1) {
                Some(AstNode::List { elements, .. }) => {
                    elements.iter().map(|e| match e {
                        AstNode::Variable { name, .. }
                        | AstNode::RowVariable { name, .. } => name.clone(),
                        _ => unreachable!("The parser should have caught a quantifier variable sentence"),
                    }).collect()
                }
                _ => unreachable!("The parser should have caught a quantifier variable sentence"),

            };
            let q_scope = self.next_scope();
            child_ctx = ScopeCtx {
                default:   ctx.default,
                overrides: ctx.overrides.clone().into_iter()
                    .chain(bound.into_iter().map(|v| (v, q_scope)))
                    .collect(),
            };
            &child_ctx
        } else {
            ctx
        };

        let mut elements: SmallVec<[Element; 4]> = SmallVec::with_capacity(elements_ast.len());
        for el in elements_ast {
            let elem = self.build_element(body_ctx, el, file, errors)?;
            elements.push(elem);
        }
        let sid = self.alloc_sentence(Sentence { elements, file: file.to_owned(), span });
        Some(sid)
    }

    fn build_element(
        &mut self,
        ctx: &ScopeCtx,
        node: &AstNode,
        file: &str,
        errors: &mut Vec<(Span, KbError)>,
    ) -> Option<Element> {
        #[cfg(debug_assertions)]
        crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Trace, target: "sigmakee_rs_core::syntactic", message: format!("building element: {}", node) });
        match node {
            AstNode::Symbol { name, span } => Some(Element::Symbol {
                id:   self.intern(name),
                span: span.clone(),
            }),
            AstNode::Variable { name, span } => {
                let scope = ctx.scope_for(name);
                Some(Element::Variable {
                    id:     self.intern(&format!("{}__{}", name, scope)),
                    name:   name.clone(),
                    is_row: false,
                    span:   span.clone(),
                })
            }
            AstNode::RowVariable { name, span } => {
                let scope = ctx.scope_for(name);
                Some(Element::Variable {
                    id:     self.intern(&format!("{}__{}", name, scope)),
                    name:   name.clone(),
                    is_row: true,
                    span:   span.clone(),
                })
            }
            AstNode::Str    { value, span } => Some(Element::Literal {
                lit:  Literal::Str(value.clone()),
                span: span.clone(),
            }),
            AstNode::Number { value, span } => Some(Element::Literal {
                lit:  Literal::Number(value.clone()),
                span: span.clone(),
            }),
            AstNode::Operator { op, span } => Some(Element::Op {
                op:   op.clone(),
                span: span.clone(),
            }),
            AstNode::List { span, .. } => {
                let list_span = span.clone();
                match self.build_sentence(ctx, node, file, errors, false) {
                    Some(sub_id) => {
                        self.sub_sentences.push(sub_id);
                        Some(Element::Sub { sid: sub_id, span: list_span })
                    }
                    None => None,
                }
            }
        }
    }
}

// -- Scope context -------------------------------------------------------------

pub(in crate::syntactic) struct ScopeCtx {
    pub default:   u64,
    pub overrides: HashMap<String, u64>,
}

impl ScopeCtx {
    pub(in crate::syntactic) fn scope_for(&self, var_name: &str) -> u64 {
        self.overrides.get(var_name).copied().unwrap_or(self.default)
    }
}
