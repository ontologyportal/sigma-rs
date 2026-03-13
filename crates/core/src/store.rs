/// Central index-based storage for all parsed KIF data.
///
/// Uses plain integer indices (SentenceId, SymbolId) throughout to avoid
/// Rc / RefCell — the store owns everything.
use std::{fmt};
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use inline_colorization::*;

use crate::error::{ParseError, Span};
use crate::parser::AstNode;
use crate::tokenizer::OpKind;
use log;

// ── Id types ──────────────────────────────────────────────────────────────────

pub type SymbolId  = usize;
pub type SentenceId = usize;

// ── Element ─────────────────────────────────────────────────────────

/// A literal value inside a sentence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Literal {
    Str(String),    // includes surrounding double-quotes
    Number(String),
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Literal::Number(num) => write!(f, "{}", num),
            Literal::Str(string) => write!(f, "{}", string)
        }
    }
}

/// One element of a sentence's term list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Element {
    Symbol(SymbolId),
    Variable { id: SymbolId, name: String, is_row: bool },
    Literal(Literal),
    /// A nested sub-sentence.
    Sub(SentenceId),
    /// A logical operator (always appears at index 0 in operator sentences).
    Op(OpKind),
}

/// Display wrapper for [`Element`] that resolves symbol ids via a store.
pub struct ElementDisplay<'a> {
    pub element: &'a Element,
    pub store:   &'a KifStore,
    pub indent:  usize,
    pub highlight: bool
}

impl<'a> fmt::Display for ElementDisplay<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.highlight {
            write!(f, "{style_bold}{style_underline}")?;
        }
        let res = match self.element {
            Element::Symbol(id)                   => {
                let sym_name = self.store.sym_name(*id);
                if sym_name.chars().next().map_or(false, |c| c.is_uppercase()) {
                    write!(f, "{color_yellow}{}{color_reset}", sym_name)
                } else {
                    write!(f, "{color_green}{}{color_reset}", sym_name)
                }
            },
            Element::Variable { name, is_row: false, .. } => write!(f, "{color_bright_blue}?{}{color_reset}", name),
            Element::Variable { name, is_row: true, ..  } => write!(f, "{color_bright_blue}@{}{color_reset}", name),
            Element::Literal(Literal::Str(s))     => write!(f, "{}", s),
            Element::Literal(Literal::Number(n))  => write!(f, "{}", n),
            Element::Op(op)                       => write!(f, "{}", op),
            Element::Sub(sid)                     => {
                // Sub-sentences never show their own gutter — the root owns it.
                SentenceDisplay::raw(*sid, self.store, self.indent, -1).fmt(f)
            }
        };
        if self.highlight {
            write!(f, "{style_reset}")
        } else { res }
    }
}

/// Display wrapper for a sentence id that resolves everything via a store.
pub struct SentenceDisplay<'a> {
    pub sid:         SentenceId,
    pub store:       &'a KifStore,
    /// Current indentation depth (each level = 2 spaces).
    pub indent:      usize,
    /// If true, wrap output with a rustc-style line-number gutter.
    pub show_gutter: bool,
    // What argument to highlight
    pub highlight_arg: i32,
}

impl<'a> SentenceDisplay<'a> {
    /// Root display: shows the line-number gutter.
    pub fn new(sid: SentenceId, store: &'a KifStore) -> Self {
        Self { sid, store, indent: 0, show_gutter: true, highlight_arg: -1  }
    }

    /// Inner display: no gutter, used for sub-sentences and raw formatting.
    pub fn raw(sid: SentenceId, store: &'a KifStore, indent: usize, arg: i32) -> Self {
        Self { sid, store, indent, show_gutter: false, highlight_arg: arg }
    }

    /// Format the sentence content (without gutter) into a String.
    fn to_raw_string(&self, arg: i32) -> String {
        SentenceDisplay::raw(self.sid, self.store, 0, arg).to_string()
    }
}

impl<'a> fmt::Display for SentenceDisplay<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.show_gutter {
            // Buffer the raw content, then prefix each line with the gutter.
            let content  = self.to_raw_string(self.highlight_arg);
            let line_no  = self.store.sentences[self.sid].span.line;
            let num_str  = line_no.to_string();
            let width    = num_str.len().min(6); // at least 6 digits wide
            let blank    = " ".repeat(width);

            for (i, line) in content.lines().enumerate() {
                if i > 0 { writeln!(f)?; }
                if i == 0 {
                    write!(f, "{:>width$} | {}", line_no, line, width = width)?;
                } else {
                    write!(f, "{} | {}", blank, line)?;
                }
            }
            return Ok(());
        }

        // ── Raw (no gutter) ──────────────────────────────────────────────────
        let sentence     = &self.store.sentences[self.sid];
        let child_indent = self.indent + 1;
        write!(f, "(")?;
        for (i, el) in sentence.elements.iter().enumerate() {
            if i == 0 {
                ElementDisplay { element: el, store: self.store, indent: child_indent, highlight: self.highlight_arg == (i as i32) }.fmt(f)?;
            } else if matches!(el, Element::Sub(_)) {
                write!(f, "\n{}", "  ".repeat(child_indent))?;
                ElementDisplay { element: el, store: self.store, indent: child_indent, highlight: self.highlight_arg == (i as i32)}.fmt(f)?;
            } else {
                write!(f, " ")?;
                ElementDisplay { element: el, store: self.store, indent: child_indent, highlight: self.highlight_arg == (i as i32)}.fmt(f)?;
            }
        }
        write!(f, ")")
    }
}

// ── Sentence ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sentence {
    /// The term list.  elements[0] is the head (Symbol or Op for operator sentences).
    pub elements: Vec<Element>,
    /// Source file tag (used by removeFile / tell rollback).
    pub file:     String,
    /// Source location of the opening paren.
    pub span:     Span,
}

impl Sentence {
    pub fn is_operator(&self) -> bool {
        matches!(self.elements.first(), Some(Element::Op(_)))
    }

    pub fn op(&self) -> Option<&OpKind> {
        match self.elements.first() {
            Some(Element::Op(op)) => Some(op),
            _ => None,
        }
    }

    /// Head symbol id (None for operator sentences or malformed sentences).
    pub fn head_symbol(&self) -> Option<SymbolId> {
        match self.elements.first() {
            Some(Element::Symbol(id)) => Some(*id),
            _ => None,
        }
    }
}

// ── Symbol ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    /// Root sentences where this symbol is the predicate / function head.
    pub head_sentences: Vec<SentenceId>,
    /// All root sentences (and sub-sentences) where this symbol appears anywhere.
    pub all_sentences:  Vec<SentenceId>,
}

// ── Taxonomy relations ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaxRelation {
    Subclass,
    Instance,
    Subrelation,
    SubAttribute,
}

impl TaxRelation {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "subclass"      => Some(TaxRelation::Subclass),
            "instance"      => Some(TaxRelation::Instance),
            "subrelation"   => Some(TaxRelation::Subrelation),
            "subAttribute"  => Some(TaxRelation::SubAttribute),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxEdge {
    /// The "parent" (second argument in the sentence; more general side).
    pub from: SymbolId,
    /// The "child" (first argument; more specific side).
    pub to:   SymbolId,
    pub rel:  TaxRelation,
}

/// Maps variable names to the SentenceId of the quantifier that binds them.
/// Variables not in `overrides` fall back to `default` (the root sentence scope).
struct ScopeCtx {
    default:   usize,
    overrides: HashMap<String, usize>,
}

impl ScopeCtx {
    fn scope_for(&self, var_name: &str) -> SentenceId {
        self.overrides.get(var_name).copied().unwrap_or(self.default)
    }
}


// ── KifStore ──────────────────────────────────────────────────────────────────

/// The raw parsed store — all sentences, symbols, and taxonomy edges.
///
/// Populated incrementally by [`crate::store::KifStore::load`].
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct KifStore {
    pub sentences:   Vec<Sentence>,
    pub symbols:     HashMap<String, SymbolId>,
    pub symbol_data: Vec<Symbol>,

    /// Root (top-level) sentence ids — in insertion order.
    pub roots: Vec<SentenceId>,

    /// All sub-sentence ids (nested inside root sentences).
    pub sub_sentences: Vec<SentenceId>,

    /// Root sentences grouped by file tag for targeted removal.
    pub file_roots: HashMap<String, Vec<SentenceId>>,

    /// Root sentences indexed by head predicate name → set of root ids.
    pub head_index: HashMap<String, Vec<SentenceId>>,

    /// Taxonomy edges extracted from the store.
    pub tax_edges:     Vec<TaxEdge>,
    /// incoming[sym_id] = list of edges where edge.to == sym_id
    pub tax_incoming:  HashMap<SymbolId, Vec<usize>>, // indices into tax_edges

    scope_counter: usize,
}

impl KifStore {
    // Add a symbol name to the Symbol table store, returning if it already exists, getting a new one if not
    pub fn intern(&mut self, name: &str) -> SymbolId {
        if let Some(&id) = self.symbols.get(name) {
            return id;
        }
        let id = self.symbol_data.len();
        self.symbol_data.push(Symbol {
            name:          name.to_owned(),
            head_sentences: Vec::new(),
            all_sentences:  Vec::new(),
        });
        self.symbols.insert(name.to_owned(), id);
        id
    }

    fn next_scope(&mut self) -> usize {
        let id = self.scope_counter;
        self.scope_counter += 1;
        id
    }


    pub fn sym_name(&self, id: SymbolId) -> &str {
        &self.symbol_data[id].name
    }

    pub fn sym_id(&self, name: &str) -> Option<SymbolId> {
        self.symbols.get(name).copied()
    }

    // ── Sentence allocation ───────────────────────────────────────────────────

    fn alloc_sentence(&mut self, sentence: Sentence) -> SentenceId {
        let id = self.sentences.len();
        self.sentences.push(sentence);
        id
    }

    // ── Load (syntax pass) ───────────────────────────────────────────────────

    /// Process a list of top-level AST nodes into this store, tagging them
    /// with `file`.  Returns any hard syntax errors found.
    pub fn load(
        &mut self,
        nodes: &[AstNode],
        file: &str,
    ) -> Vec<(Span, ParseError)> {
        let mut errors: Vec<(Span, ParseError)> = Vec::new();
        for node in nodes {
            match node {
                AstNode::List { .. } => {
                    let ctx = ScopeCtx { default: self.next_scope(), overrides: HashMap::new() };
                    match self.build_sentence(&ctx, node, file, &mut errors, true) {
                        Some(sent_id) => {
                            log::trace!("Registered root sentence {}: {}", sent_id, node);
                            self.roots.push(sent_id);
                            self.file_roots
                                .entry(file.to_owned())
                                .or_default()
                                .push(sent_id);
                            // Index by head predicate
                            if let Some(head_id) = self.sentences[sent_id].head_symbol() {
                                let head_name = self.sym_name(head_id).to_owned();
                                self.head_index
                                    .entry(head_name)
                                    .or_default()
                                    .push(sent_id);
                                self.symbol_data[head_id].head_sentences.push(sent_id);
                            }
                        }
                        None => {}
                    }
                }
                // Top-level atoms are ignored (comments already stripped)
                _ => {}
            }
        }
        errors
    }

    // Build a new sentence from a node
    fn build_sentence(
        &mut self,
        ctx: &ScopeCtx,
        node: &AstNode,
        file: &str,
        errors: &mut Vec<(Span, ParseError)>,
        top_level: bool,
    ) -> Option<SentenceId> {
        // Destructure the list node
        let (elements_ast, span) = match node {
            AstNode::List { elements, span } => (elements, span.clone()),
            _ => return None, // If the node is not a sentence node, skip this
        };
        
        // If the sentence is empty, its meaningless as a top-level axiom or head
        if elements_ast.is_empty() {
            if top_level {
                errors.push((span.clone(), ParseError::EmptySentence { span: span.clone() }));
                return None;
            } else {
                // Nested empty list is fine (e.g. empty quantifier var list)
                let sid = self.alloc_sentence(Sentence { elements: Vec::new(), file: file.to_owned(), span });
                return Some(sid);
            }
        }
        
        // Also check if the head is valid (Symbol, Variable, or Operator)
        if top_level {
            let first = &elements_ast[0];
            if !matches!(first, AstNode::Symbol { .. } | AstNode::Variable { .. } | AstNode::RowVariable { .. } | AstNode::Operator { .. }) {
                errors.push((first.span().clone(), ParseError::FirstTerm { span: first.span().clone() }));
                return None;
            }
        }

        log::trace!("Building sentence: {}", node);

        // Check for operator in non-head position
        for (i, el) in elements_ast.iter().enumerate() {
            if i > 0 {
                if let AstNode::Operator { op, span } = el {
                    errors.push((span.clone(), ParseError::OperatorOutOfPosition {
                        op:   op.name().to_owned(),
                        span: span.clone(),
                    }));
                    return None;
                }
            }
        }

        // Prebuild the element vector
        let mut elements = Vec::with_capacity(elements_ast.len());
        // Pre allocate the sentence because we want to get the SID for potential variable scoping
        
        // If this is a quantifier, build a child context for its body.
        let child_ctx;
        let body_ctx = if matches!(elements_ast.get(0), Some(AstNode::Operator { op: OpKind::Exists | OpKind::ForAll, .. })) {
            let bound = match elements_ast.get(1) {
                Some(AstNode::List { elements, .. }) => {
                    let result: Result<Vec<String>, (Span, ParseError)> = elements.iter().map(| e | match e {
                        AstNode::Variable { name, .. } => Ok(name.clone()),
                        AstNode::RowVariable { name, .. } => Ok(name.clone()),
                        _ => Err((span.clone(), ParseError::QuantiferArg { span: span.clone() }))
                    }).collect();
                    match result {
                        Ok(vars) => vars,
                        Err(e) => {
                            errors.push(e);
                            return None;
                        }
                    }
                },
                _ => {
                    errors.push((
                        span.clone(),
                        ParseError::QuantiferArg { span: span.clone() }
                    ));
                    return None
                }
            }; // peek at var list
            let q_scope = self.next_scope();
            child_ctx = ScopeCtx {
                default:   ctx.default,               // unbound vars keep outer scope
                overrides: ctx.overrides.clone()       // inherit outer bindings
                    .into_iter()
                    .chain(bound.into_iter().map(|v| (v, q_scope))) // bound vars → this sid
                    .collect(),
            };
            &child_ctx
        } else {
            ctx  // non-quantifier: pass context unchanged
        };

        for el in elements_ast {
            let elem = self.build_element(body_ctx, el, file, errors)?;
            elements.push(elem);
        }
        
        let sid = self.alloc_sentence(Sentence { elements, file: file.to_owned(), span });
        log::trace!("Allocated sentence: {}", sid);
        // Record taxonomy edges for symbol-headed sentences.
        self.extract_tax_edge(sid);
        Some(sid)
    }

    fn build_element(
        &mut self,
        ctx: &ScopeCtx,
        node: &AstNode,
        file: &str,
        errors: &mut Vec<(Span, ParseError)>,
    ) -> Option<Element> {
        log::trace!("Building element: {}", node);

        match node {
            AstNode::Symbol { name, .. } => {
                let id = self.intern(name);
                Some(Element::Symbol(id))
            }
            AstNode::Variable { name, .. } => {
                let scope = ctx.scope_for(name);
                Some(Element::Variable {
                    id: self.intern(format!("{}@{}", name, scope).as_str()),
                    name:   name.clone(),
                    is_row: false,
                })
            },
            AstNode::RowVariable { name, .. } => {
                let scope = ctx.scope_for(name);
                Some(Element::Variable {
                    id: self.intern(format!("{}@{}", name, scope).as_str()),
                    name:   name.clone(),
                    is_row: true,
                })
            },
            AstNode::Str { value, .. } => {
                Some(Element::Literal(Literal::Str(value.clone())))
            }
            AstNode::Number { value, .. } => {
                Some(Element::Literal(Literal::Number(value.clone())))
            }
            AstNode::Operator { op, .. } => Some(Element::Op(op.clone())),
            AstNode::List { .. } => {
                // Nested sub-sentence — build it and store as Sub
                match self.build_sentence(ctx, node, file, errors, false) {
                    Some(sub_id) => {
                        self.sub_sentences.push(sub_id);
                        Some(Element::Sub(sub_id))
                    }
                    None => None,
                }
            }
        }
    }
    // Check whether the sentence can create a valid taxonomy edge
    fn extract_tax_edge(&mut self, sent_id: SentenceId) {
        let sentence = &self.sentences[sent_id];
        let head_sym = match sentence.head_symbol() {
            Some(id) => id,
            None     => return,
        };
        let head_name = self.sym_name(head_sym).to_owned();

        // If the relation is not a valid taxonomy edge type, ignore
        let rel= match TaxRelation::from_str(&head_name) {
            Some(r) => r,
            None => return
        };

        // Sentence shape: (relation arg1 arg2), handle both variables and symbols
        //  as both are tracked using the same mechanism
        let arg1 = match sentence.elements.get(1) {
            Some(Element::Symbol(id)) => *id,
            Some(Element::Variable { id, is_row: false, .. }) => {
                *id
            },
            _ => return, // TODO: Handle this as an error
        };
        let arg2 = match sentence.elements.get(2) {
            Some(Element::Symbol(id)) => *id,
            Some(Element::Variable { id, is_row: false, .. }) => *id,
            _ => return // TODO: Handle this as an error 
        };

        // Edge: from=arg2 (parent), to=arg1 (child)
        let edge_idx = self.tax_edges.len();
        self.tax_edges.push(TaxEdge { from: arg2, to: arg1, rel });
        self.tax_incoming.entry(arg1).or_default().push(edge_idx);
        log::trace!("{} -{}-> {}", self.sym_name(arg2), head_name, self.sym_name(arg1));
    }

    /// Return the SymbolId for a variable `name` scoped to `scope`, if it has
    /// been interned (i.e. appeared in a type declaration in that scope).
    pub fn var_sym_id(&self, name: &str, scope: SentenceId) -> Option<SymbolId> {
        self.sym_id(&format!("{}@{}", name, scope))
    }

    // ── removeFile ────────────────────────────────────────────────────────────

    /// Remove all sentences tagged with `file`.  Symbols that lose all their
    /// references are removed from the symbol table.
    pub fn remove_file(&mut self, file: &str) {
        let ids_to_remove: Vec<SentenceId> = self
            .file_roots
            .remove(file)
            .unwrap_or_default();

        if ids_to_remove.is_empty() {
            return;
        }

        let id_set: std::collections::HashSet<SentenceId> =
            ids_to_remove.iter().copied().collect();

        // Remove from roots
        self.roots.retain(|id| !id_set.contains(id));

        // Remove from head_index
        for v in self.head_index.values_mut() {
            v.retain(|id| !id_set.contains(id));
        }
        self.head_index.retain(|_, v| !v.is_empty());

        // Remove taxonomy edges for removed sentences
        let removed_edges: Vec<usize> = self
            .tax_edges
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                // Edge belongs to a removed sentence if we can trace it back — we
                // use a simpler heuristic: rebuild from scratch after clearing.
                // For correctness we rebuild the entire taxonomy.
                let _ = e;
                false // placeholder — we rebuild below
            })
            .map(|(i, _)| i)
            .collect();
        let _ = removed_edges;

        // Mark removed sentences as tombstones by clearing their elements.
        for &sid in &ids_to_remove {
            if sid < self.sentences.len() {
                self.sentences[sid].elements.clear();
            }
        }

        // Rebuild taxonomy from surviving sentences.
        self.rebuild_taxonomy();

        // Remove orphaned symbols (those that appear in no remaining root sentences).
        self.prune_orphaned_symbols(&id_set);
    }

    fn rebuild_taxonomy(&mut self) {
        self.tax_edges.clear();
        self.tax_incoming.clear();
        for sent_id in 0..self.sentences.len() {
            self.extract_tax_edge(sent_id);
        }
    }

    fn prune_orphaned_symbols(&mut self, removed_ids: &std::collections::HashSet<SentenceId>) {
        // Collect symbols referenced by surviving sentences.
        let mut referenced = std::collections::HashSet::new();
        for &sid in &self.roots {
            self.collect_symbols(sid, &mut referenced);
        }
        // Remove symbols not in the referenced set.
        let to_remove: Vec<String> = self
            .symbols
            .keys()
            .filter(|name| {
                let id = *self.symbols.get(*name).unwrap();
                !referenced.contains(&id)
            })
            .cloned()
            .collect();
        for name in to_remove {
            if let Some(id) = self.symbols.remove(&name) {
                // Tombstone the symbol_data entry (keep index valid)
                self.symbol_data[id].head_sentences.clear();
                self.symbol_data[id].all_sentences.clear();
            }
        }
        let _ = removed_ids;
    }

    fn collect_symbols(&self, sent_id: SentenceId, out: &mut std::collections::HashSet<SymbolId>) {
        let sentence = &self.sentences[sent_id];
        for el in &sentence.elements {
            match el {
                Element::Symbol(id) => { out.insert(*id); }
                Element::Sub(sub_id) => self.collect_symbols(*sub_id, out),
                _ => {}
            }
        }
    }

    // ── Lookup helpers ────────────────────────────────────────────────────────

    /// Root sentences with the given head predicate name.
    pub fn by_head(&self, head: &str) -> &[SentenceId] {
        self.head_index.get(head).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Root sentences with `head` and first argument symbol matching `arg1`.
    pub fn by_head_arg1(&self, head: &str, arg1: &str) -> Vec<SentenceId> {
        let arg1_id = match self.sym_id(arg1) {
            Some(id) => id,
            None     => return Vec::new(),
        };
        self.by_head(head)
            .iter()
            .copied()
            .filter(|&sid| {
                matches!(
                    self.sentences[sid].elements.get(1),
                    Some(Element::Symbol(id)) if *id == arg1_id
                )
            })
            .collect()
    }

    /// Root sentences with `head`, `arg1` name, and a literal at position 2
    /// matching `arg2_literal` (for things like `(domain rel N Class)`).
    pub fn by_head_arg1_lit(&self, head: &str, arg1: &str, arg2_lit: &str) -> Vec<SentenceId> {
        self.by_head_arg1(head, arg1)
            .into_iter()
            .filter(|&sid| {
                matches!(
                    self.sentences[sid].elements.get(2),
                    Some(Element::Literal(Literal::Number(n))) if n == arg2_lit
                )
            })
            .collect()
    }

    /// String-based pattern lookup (the public query API).
    ///
    /// Pattern tokens are whitespace-separated:
    ///  - A literal word matches that symbol name exactly
    ///  - `_`  matches any single term
    ///  - `_N` matches at most N terms (variadic tail)
    ///
    /// Example: `"instance _ Entity"` matches `(instance <anything> Entity)`
    pub fn lookup(&self, pattern: &str) -> Vec<SentenceId> {
        let tokens: Vec<&str> = pattern.split_whitespace().collect();
        if tokens.is_empty() {
            return self.roots.clone();
        }

        // Fast-path: if first token is a literal, use the head index.
        let candidates: Box<dyn Iterator<Item = &SentenceId>> = if tokens[0] != "_" {
            match self.head_index.get(tokens[0]) {
                Some(ids) => Box::new(ids.iter()),
                None      => return Vec::new(),
            }
        } else {
            Box::new(self.roots.iter())
        };

        candidates
            .copied()
            .filter(|&sid| self.matches_pattern(sid, &tokens))
            .collect()
    }

    fn matches_pattern(&self, sid: SentenceId, tokens: &[&str]) -> bool {
        let sentence = &self.sentences[sid];
        // elements[0] is head (op or symbol), then args follow
        let elems = &sentence.elements;

        let mut e_idx = 0;
        let mut t_idx = 0;

        while t_idx < tokens.len() {
            let tok = tokens[t_idx];

            if tok == "_" {
                // Wildcard: consume one element (any type)
                if e_idx >= elems.len() { return false; }
                e_idx += 1;
                t_idx += 1;
            } else if tok.starts_with("_") && tok[1..].parse::<usize>().is_ok() {
                // _N: variadic — skip up to N remaining tokens
                // Just consume the rest
                // t_idx += 1;
                return true;
            } else {
                // Literal match
                if e_idx >= elems.len() { return false; }
                if !self.elem_matches_name(&elems[e_idx], tok) { return false; }
                e_idx += 1;
                t_idx += 1;
            }
        }
        // All pattern tokens consumed; require exact length match
        e_idx == elems.len()
    }

    fn elem_matches_name(&self, elem: &Element, name: &str) -> bool {
        match elem {
            Element::Symbol(id)   => self.sym_name(*id) == name,
            Element::Op(op)       => op.name() == name,
            Element::Variable { name: n, .. } => n == name,
            Element::Literal(Literal::Number(n)) => n == name,
            Element::Literal(Literal::Str(s))    => s == name,
            Element::Sub(_)                      => false,
        }
    }
}

// Top-level kif() / kifFile() equivalents

use crate::tokenizer::tokenize;
use crate::parser::parse;

/// Parse `text` (tagged as `file`) into `store`.  Returns hard parse errors.
pub fn load_kif(store: &mut KifStore, text: &str, file: &str) -> Vec<(Span, ParseError)> {
    let (tokens, tok_errors) = tokenize(text, file);
    let (nodes, parse_errors) = parse(tokens, file);
    let mut errors: Vec<(Span, ParseError)> = tok_errors;
    errors.extend(parse_errors);
    errors.extend(store.load(&nodes, file));
    errors
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    fn store_from(kif: &str) -> KifStore {
        let mut store = KifStore::default();
        let errors = load_kif(&mut store, kif, "test");
        assert!(errors.is_empty(), "load errors: {:?}", errors);
        store
    }

    #[test]
    fn basic_load() {
        let store = store_from("(subclass Human Animal)");
        assert_eq!(store.roots.len(), 1);
        assert!(store.symbols.contains_key("subclass"));
        assert!(store.symbols.contains_key("Human"));
        assert!(store.symbols.contains_key("Animal"));
    }

    #[test]
    fn taxonomy_edge() {
        let store = store_from("(subclass Human Animal)");
        assert_eq!(store.tax_edges.len(), 1);
        let edge = &store.tax_edges[0];
        assert_eq!(edge.rel, TaxRelation::Subclass);
        assert_eq!(store.sym_name(edge.from), "Animal"); // parent
        assert_eq!(store.sym_name(edge.to),   "Human");  // child
    }

    #[test]
    fn head_index() {
        let store = store_from("(subclass Human Animal)\n(subclass Dog Animal)");
        let hits = store.by_head("subclass");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn pattern_lookup() {
        let store = store_from(
            "(instance subclass BinaryRelation)\n\
             (instance instance BinaryPredicate)",
        );
        let hits = store.lookup("instance _ BinaryRelation");
        assert_eq!(hits.len(), 1);
        let hits2 = store.lookup("instance _ _");
        assert_eq!(hits2.len(), 2);
    }

    #[test]
    fn remove_file() {
        let mut store = KifStore::default();
        load_kif(&mut store, "(subclass Human Animal)", "base");
        load_kif(&mut store, "(subclass Cat Animal)", "delta");
        assert_eq!(store.roots.len(), 2);
        store.remove_file("delta");
        assert_eq!(store.roots.len(), 1);
        // Cat should be gone (orphaned)
        assert!(!store.symbols.contains_key("Cat"));
        // Human / Animal remain
        assert!(store.symbols.contains_key("Human"));
    }
}
