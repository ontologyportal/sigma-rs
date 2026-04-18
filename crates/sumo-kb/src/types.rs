// crates/sumo-kb/src/types.rs
//
// Single canonical definition of every shared data type.
// Merges sumo-parser-core/src/store.rs and the relevant parts of
// sumo-store/src/schema.rs into one place.  No other module in this
// crate re-defines these types.

use std::fmt;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

// Re-export OpKind so consumers only need to import from types.
pub use crate::parse::ast::OpKind;

// -- Id types -----------------------------------------------------------------

/// Stable symbol identifier.  Unique within a KnowledgeBase for its lifetime.
/// When persistence is enabled this value is identical to the LMDB sequence
/// key -- no remapping is ever required.
pub type SymbolId = u64;

/// Stable sentence / formula identifier.
pub type SentenceId = u64;

/// Stable clause identifier.  Allocated by the `clause_id` sequence in
/// the LMDB persistence layer when a new, canonical-hash-fresh clause
/// is first interned; existing clauses retain their id across reopens.
/// Used as the deduped key type in `StoredFormula.clause_ids`.
#[cfg(feature = "cnf")]
pub type ClauseId = u64;

// -- Literal ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Literal {
    /// String literal -- includes surrounding double-quotes as stored in source.
    Str(String),
    /// Numeric literal (integer or decimal) as a raw string.
    Number(String),
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Literal::Str(s)    => write!(f, "{}", s),
            Literal::Number(n) => write!(f, "{}", n),
        }
    }
}

// -- Element -------------------------------------------------------------------

/// One element in a sentence's term list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Element {
    /// A ground symbol, referenced by its stable id.
    Symbol(SymbolId),
    /// A logical variable or row-variable.
    /// `id` is the interned symbol id for the scope-qualified name (e.g. `x@3`).
    Variable { id: SymbolId, name: String, is_row: bool },
    /// A string or numeric literal.
    Literal(Literal),
    /// A nested sub-sentence.  The id indexes into the same flat sentence Vec
    /// owned by KifStore.
    Sub(SentenceId),
    /// A logical operator (always at index 0 in operator sentences).
    Op(OpKind),
}

// -- Sentence ------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sentence {
    /// The term list.  `elements[0]` is the head (Symbol or Op).
    /// Up to 4 elements fit inline without heap allocation (covers the common
    /// case of a 3-argument predicate: head + 3 args = 4 elements total).
    pub elements: SmallVec<[Element; 4]>,
    /// Source file tag -- used to group sentences for session management.
    pub file: String,
    /// Source location of the opening parenthesis.
    pub span: crate::error::Span,
}

impl Sentence {
    /// True if this is an operator sentence (and, or, not, =>, <=>, forall, exists).
    pub fn is_operator(&self) -> bool {
        matches!(self.elements.first(), Some(Element::Op(_)))
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
            Some(Element::Symbol(id)) => Some(*id),
            _ => None,
        }
    }
}

// -- Symbol --------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    /// Root sentences where this symbol is the predicate / function head.
    pub head_sentences: Vec<SentenceId>,
    /// All root sentences (and sub-sentences) where this symbol appears anywhere.
    pub all_sentences: Vec<SentenceId>,
    /// True for Skolem function/constant symbols generated during CNF conversion.
    /// Always false for ordinary KB symbols.
    pub is_skolem: bool,
    /// Arity of a Skolem function symbol.  None for constants and ordinary symbols.
    pub skolem_arity: Option<usize>,
}

impl Default for Symbol {
    fn default() -> Self {
        Self {
            name: String::new(),
            head_sentences: Vec::new(),
            all_sentences: Vec::new(),
            is_skolem: false,
            skolem_arity: None,
        }
    }
}

// -- Taxonomy ------------------------------------------------------------------

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

// -- CNF types (feature = "cnf") -----------------------------------------------

/// A CNF clause: a disjunction of literals.
#[cfg(feature = "cnf")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clause {
    pub literals: Vec<CnfLiteral>,
}

/// A single CNF literal (positive or negative atom).
#[cfg(feature = "cnf")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CnfLiteral {
    pub positive: bool,
    /// The predicate.  Usually `CnfTerm::Const(id)`.
    pub pred: CnfTerm,
    pub args: Vec<CnfTerm>,
}

/// A CNF term -- an argument or predicate position in a literal.
///
/// The enum covers both output shapes produced by the two clausifiers that
/// live in this crate:
///
/// - The hand-rolled CNF (`cnf.rs`, feature `cnf`) produces `Const` /
///   `Var` / `SkolemFn` / `Num` / `Str`.  Variables carry a `KifStore`
///   SymbolId that resolves to a scope-qualified name like `x@5`.
/// - The Vampire-backed CNF (`cnf2.rs`) additionally produces `Fn` for
///   non-skolem function applications that Vampire's clausifier may
///   introduce (e.g. equality reasoning over function terms).  Variables
///   from this path carry a *clause-local* index repurposed as a
///   SymbolId — canonical hashing renames all variables, and no index or
///   display path dereferences these ids against the store.
#[cfg(feature = "cnf")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CnfTerm {
    /// A ground constant (ordinary symbol or class name).
    Const(SymbolId),
    /// A universally-quantified variable.  For the hand-rolled clausifier
    /// this is a scope-qualified KIF name (`x@5`); for the Vampire-backed
    /// clausifier this is a clause-local integer index.
    Var(SymbolId),
    /// A non-skolem function application, produced by the Vampire
    /// clausifier when equality or theory reasoning requires exposing a
    /// term-level functor.  `id` interns to an ordinary `KifStore`
    /// symbol.
    Fn { id: SymbolId, args: Vec<CnfTerm> },
    /// A Skolem function application.
    SkolemFn { id: SymbolId, args: Vec<CnfTerm> },
    /// A numeric literal.
    Num(String),
    /// A string literal (includes surrounding double-quotes).
    Str(String),
}
