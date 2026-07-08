//! Sentence element types: symbols, variables, literals, sub-sentences,
//! and operators.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Display;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{AstNode, SentenceId, OpKind};
use super::literal::Literal;
use super::ScopeCtx;
use super::Sentence;

/// Stable symbol identifier — the content hash of the symbol's name
/// (`Symbol::id`). The same name always yields the same id, KB-independent.
/// When persistence is enabled this is also the blob key.
pub type SymbolId = u64;

/// An interned symbol name (`Arc<str>`), identified by the content hash of
/// its name.
#[derive(Clone, Debug, Eq, Serialize, Deserialize)]
pub struct Symbol(Arc<str>);

impl PartialEq for Symbol {
    fn eq(&self, other: &Self) -> bool { self.0 == other.0 }
}

// Hash on the id (the name's content hash), not the `Arc<str>` bytes, so a
// `Symbol` and its `SymbolId` hash identically. Must stay consistent with
// `PartialEq`: equal names → equal ids.
impl Hash for Symbol {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.id());
    }
}

impl From<&String> for Symbol {
    fn from(value: &String) -> Self {
        Self(Arc::from(value.clone().into_boxed_str()))
    }
}

impl From<String> for Symbol {
    fn from(value: String) -> Self {
        Self(Arc::from(value.into_boxed_str()))
    }
}

impl From<&str> for Symbol {
    fn from(value: &str) -> Self {
        Self(Arc::from(value.to_string().into_boxed_str()))
    }
}

impl Display for Symbol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Symbol {
    /// This symbol's stable id: the content hash of its name.
    pub fn id(&self) -> u64 {
        xxhash_rust::xxh64::xxh64(self.0.as_bytes(), 0)
    }

    /// The id a symbol with this name would have, without interning it.
    pub fn hash_name(name: &str) -> u64 {
        xxhash_rust::xxh64::xxh64(name.as_bytes(), 0)
    }

    /// The symbol's name as a shared `Arc<str>`.
    pub fn name(&self) -> Arc<str> {
        self.0.clone()
    }

    /// Borrow the name — no refcount traffic.  Prefer this in hot paths:
    /// `name()` pays an atomic inc/dec pair per call.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// InternedSym — the symbol leaf stored in `Element::Symbol`
// ---------------------------------------------------------------------------

/// A `Symbol` as it lives inside the AST (`Element::Symbol`).
///
/// In memory it is just a `Symbol` (an `Arc<str>`), so every occurrence of the
/// same symbol shares one allocation.  On the wire, though, it serializes as its
/// 8-byte [`SymbolId`] *only* — the name is never written per-occurrence (it
/// lives once in the `syntactic::symbols` table).  On deserialize the id is
/// resolved back to the shared `Arc` via [`THAW_POOL`], so a restore reproduces
/// the in-memory sharing instead of allocating a fresh string per occurrence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InternedSym(pub Symbol);

impl std::ops::Deref for InternedSym {
    type Target = Symbol;
    fn deref(&self) -> &Symbol { &self.0 }
}

impl From<Symbol> for InternedSym {
    fn from(s: Symbol) -> Self { InternedSym(s) }
}

impl Serialize for InternedSym {
    /// Write the id only — the string is recovered from the symbol table on thaw.
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(self.0.id())
    }
}

impl<'de> Deserialize<'de> for InternedSym {
    /// Read the id and resolve it against the thaw pool, cloning the one shared
    /// `Arc<str>` for this symbol.  Deserialization is only valid inside a seeded
    /// [`THAW_POOL`] scope (the sentence-cache `thaw`, which runs after the
    /// symbols table is restored); an unseeded pool or an unknown id is an error.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let id = u64::deserialize(d)?;
        THAW_POOL.with(|p| {
            match p.borrow().as_ref() {
                None => Err(D::Error::custom(
                    "InternedSym deserialized outside a seeded THAW_POOL scope",
                )),
                Some(map) => map.get(&id).cloned().map(InternedSym).ok_or_else(|| {
                    D::Error::custom(format!("symbol id {id:#x} absent from THAW_POOL"))
                }),
            }
        })
    }
}

thread_local! {
    /// Deserialize-time `SymbolId -> Symbol` pool, seeded from the restored
    /// `syntactic::symbols` table just before the sentence store is thawed and
    /// cleared right after.  `None` outside a thaw scope.  The seam that
    /// brackets sentence-store deserialization owns the lifetime (see
    /// `SyntacticLayer::restore_caches_from`).
    static THAW_POOL: RefCell<Option<HashMap<SymbolId, Symbol>>> = const { RefCell::new(None) };
}

/// Seed the thaw pool with the restored symbol table.  Call after the symbols
/// table is thawed and before the sentence store is thawed.
pub(crate) fn seed_thaw_pool(symbols: HashMap<SymbolId, Symbol>) {
    THAW_POOL.with(|p| *p.borrow_mut() = Some(symbols));
}

/// Tear down the thaw pool once the sentence store is restored.
pub(crate) fn clear_thaw_pool() {
    THAW_POOL.with(|p| *p.borrow_mut() = None);
}

/// One element in a sentence's term list.
///
/// Consumers that construct Elements without source origin (CNF clausifier,
/// macro expansions, test fixtures) use [`Span::synthetic`] so position
/// queries skip them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Element {
    /// A ground symbol.  Holds the interned [`Symbol`] (shared `Arc<str>`);
    /// serializes as its [`SymbolId`] only (see [`InternedSym`]).
    Symbol(InternedSym),
    /// A logical variable or row-variable.
    /// `id` is the interned symbol id for the scope-qualified name (e.g. `x@3`).
    Variable {
        id:     SymbolId,
        name:   String,
        is_row: bool,
        /// Per-root-formula variable index: a 0-based ordinal assigned to each
        /// distinct scoped variable in its enclosing root formula, in
        /// first-appearance order, shared by every occurrence of that variable.
        /// Stamped at sentence-build time by `assign_var_indices`.
        var_index: u32,
    },
    /// A string or numeric literal.
    Literal(Literal),
    /// A nested sub-sentence.  The id indexes into the same flat sentence Vec
    /// owned by SyntacticLayer.
    Sub(SentenceId),
    /// A logical operator (always at index 0 in operator sentences).
    Op(OpKind),
}

impl Element {
    /// Build an [`Element`] from an [`AstNode`], returning the element along
    /// with any nested sub-sentences and symbols to be interned. Returns
    /// `None` if the node cannot form an element.
    pub(super) fn from_node(
        node: &AstNode,
        ctx: &ScopeCtx,
    ) -> Option<(Self, Vec<Sentence>, Vec<Symbol>)> {
        #[cfg(debug_assertions)]
        crate::log!(Trace, "sigmakee_rs_core::syntactic", format!("building element: {}", node));
        match node {
            AstNode::Symbol { name, .. } => {
                let sym = Symbol::from(name);
                Some((Element::Symbol(InternedSym(sym.clone())), vec![], vec![sym]))
            },
            AstNode::Variable { name, .. } => {
                let scope = ctx.scope_for(name);
                let sym = Symbol::from(format!("{}__{}", name, scope));
                Some((Element::Variable {
                    id:        sym.id(),
                    name:      name.clone(),
                    is_row:    false,
                    var_index: 0,
                }, vec![], vec![sym]))
            }
            AstNode::RowVariable { .. } => {
                unreachable!("Row variables should have been collapsed by this point")
            }
            AstNode::Str    { value, .. } => Some((Element::Literal(Literal::Str(value.clone())), vec![], vec![])),
            AstNode::Number { value, .. } => Some((Element::Literal(Literal::Number(value.clone())), vec![], vec![])),
            AstNode::Operator { op, .. } => Some((Element::Op(op.clone()), vec![], vec![])),
            AstNode::List { .. } => {
                let (sub_sent, mut subs, syms) = Sentence::from_node(node, ctx)?;
                let sub_id = sub_sent.hash();
                subs.push(sub_sent);
                Some((Element::Sub(sub_id), subs, syms))
            }
            AstNode::Annotated { .. } => {
                unreachable!("Annotated statements should be stripped before sentence building")
            }
        }
    }
}