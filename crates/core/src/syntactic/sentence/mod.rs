//! The [`Sentence`] type (a flat list of [`Element`]s) and its accessors.

mod element;
mod literal;
mod hash;

pub use element::Element;
pub use literal::Literal;
pub use element::{InternedSym, Symbol, SymbolId};
pub(crate) use element::{clear_thaw_pool, seed_thaw_pool};

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::AstNode;
use crate::parse::OpKind;

/// Inline capacity for [`ElementVec`].
///
/// Sentences with up to this many elements are stored entirely on the stack;
/// longer sentences spill to the heap exactly like a `Vec`.
pub(crate) const ELEM_INLINE: usize = 5;

/// A `SmallVec` of [`Element`]s using the crate-wide inline capacity.
pub type ElementVec = smallvec::SmallVec<[Element; ELEM_INLINE]>;

// Sentence

/// Stable sentence / formula identifier.
pub type SentenceId = u64;

/// A KIF sentence: a flat list of [`Element`]s where `elements[0]` is the head.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Sentence {
    /// The parent sentence for full tree traversal
    pub parent: Vec<SentenceId>,
    /// The term list.  `elements[0]` is the head (Symbol or Op).
    /// Up to [`ELEM_INLINE`] elements fit inline without heap allocation.
    /// Longer sentences spill to the heap transparently.
    pub elements: ElementVec,
}

impl PartialEq for Sentence {
    fn eq(&self, other: &Self) -> bool {
        hash::elements_eq(&self.elements, &other.elements)
    }
}

impl Sentence {
    /// True if this is an operator sentence (and, or, not, =>, <=>, forall, exists).
    pub fn is_operator(&self) -> bool {
        matches!(self.elements.first(), Some(Element::Op(..)))
    }

    /// The operator kind, if this is an operator sentence.
    pub fn op(&self) -> Option<&OpKind> {
        match self.elements.first() {
            Some(Element::Op(op)) => Some(op),
            _ => None,
        }
    }

    /// The head symbol id, if this is a symbol-headed sentence.
    pub fn head_symbol(&self) -> Option<SymbolId> {
        match self.elements.first() {
            Some(Element::Symbol(sym)) => Some(sym.id()),
            _ => None,
        }
    }

    /// The head symbol, if this is a symbol-headed sentence.
    pub fn head_symbol_name(&self) -> Option<Symbol> {
        match self.elements.first() {
            Some(Element::Symbol(sym)) => Some(sym.0.clone()),
            _ => None,
        }
    }

    /// get the arity of the sentence (number of arguments)
    pub fn arity(&self) -> usize {
        self.elements.len().saturating_sub(1)
    }

    /// The content-addressed id of this sentence (a hash of its elements).
    pub fn hash(&self) -> SentenceId {
        hash::content_hash(&self.elements)
    }

    /// Build a content-addressed sentence from an AST node under scope `ctx`.
    ///
    /// Returns the root sentence, its sub-sentences, and the symbols
    /// collected during the walk. Returns `None` when `node` is not a list.
    pub(crate) fn from_node(node: &AstNode, ctx: &ScopeCtx) -> Option<(Self, Vec<Self>, Vec<Symbol>)> {
        let AstNode::List { elements: elements_ast, .. } = node else { return None };

        let Some(first) = elements_ast.get(0) else {
            unreachable!("The parser should have found and rejected empty sentences");
        };
        
        if !matches!(first, AstNode::Symbol { .. } | AstNode::Variable { .. } | AstNode::RowVariable { .. } | AstNode::Operator { .. }) {
            unreachable!("The parser should have caught sentences which did not start with a symbol");
        }

        for (i, el) in elements_ast.iter().enumerate() {
            if i > 0 {
                if let AstNode::Operator { .. } = el {
                    unreachable!("The parser should have caught sentences where the symbol did not appear in the first term of a sentence");
                }
            }
        }

        // A quantifier body inherits the free-variable scope but overrides its
        // bound variables to a freshly minted scope.
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
            child_ctx = ctx.child_for_quantifier(bound);
            &child_ctx
        } else {
            ctx
        };

        let mut elements: ElementVec = ElementVec::with_capacity(elements_ast.len());
        let mut collected_syms = Vec::new();
        let mut sub_sentences = Vec::new();
        for el in elements_ast {
            let (elem, subs, syms) = Element::from_node(el, body_ctx)?;
            sub_sentences.extend(subs);
            collected_syms.extend(syms);
            elements.push(elem);
        }
        let new_sent = Sentence { parent: Vec::new(), elements };
        let new_id = new_sent.hash();
        let sub_sentences = sub_sentences.into_iter().map(|mut sent| {
            if sent.parent.is_empty() {
                sent.parent = vec![new_id];
            }
            sent
        }).collect();
        Some((new_sent, sub_sentences, collected_syms))
    }
}

/// Build the content-addressed sentence (plus its sub-sentences, children
/// first) for one normalized root AST node without touching any store.
///
/// Variable scopes are minted from a fresh counter, so ids are only
/// self-consistent within this build.
pub(crate) fn build_detached(node: &AstNode) -> Option<(Sentence, Vec<Sentence>)> {
    let ctx = ScopeCtx::new(Arc::new(AtomicU64::new(0)));
    let (root, subs, _syms) = Sentence::from_node(node, &ctx)?;
    Some((root, subs))
}

/// Variable-scope context for one `from_node` build.
///
/// `default` is this root's scope for free (unbound) variables, fixed for the
/// whole build. Each quantifier mints a fresh scope from the shared `counter`
/// for its bound variables (recorded in `overrides`), so every free occurrence
/// of `?X` in a root shares one scoped id while different roots get distinct ones.
pub(in super::super) struct ScopeCtx {
    /// Shared scope-id allocator (the store's `scope_counter`).
    counter:   Arc<AtomicU64>,
    /// This root's free-variable scope (fixed).
    default:   u64,
    /// Quantifier-bound variable name → its minted scope id.
    overrides: HashMap<String, u64>,
}

impl ScopeCtx {
    /// Start a fresh root context, minting this root's free-variable scope.
    pub(in super::super) fn new(counter: Arc<AtomicU64>) -> Self {
        let default = counter.fetch_add(1, Ordering::Relaxed);
        Self { counter, default, overrides: HashMap::new() }
    }

    /// Scope id for `var_name`: a quantifier override if bound, else this root's
    /// free-variable scope.
    fn scope_for(&self, var_name: &str) -> u64 {
        self.overrides.get(var_name).copied().unwrap_or(self.default)
    }

    /// Child context for a quantifier body: same free-var `default`, with each
    /// `bound` name overridden to a single freshly minted scope.
    fn child_for_quantifier(&self, bound: impl IntoIterator<Item = String>) -> Self {
        let q = self.counter.fetch_add(1, Ordering::Relaxed);
        let mut overrides = self.overrides.clone();
        overrides.extend(bound.into_iter().map(|v| (v, q)));
        Self { counter: self.counter.clone(), default: self.default, overrides }
    }
}