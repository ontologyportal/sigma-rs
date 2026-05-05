// crates/core/src/cnf/clause.rs
//
// Types for CNF Clauses, Terms, and Literals

use serde::{Serialize, Deserialize};
use crate::SymbolId;

/// Stable clause identifier.  Allocated by the `clause_id` sequence in
/// the LMDB persistence layer when a new, canonical-hash-fresh clause
/// is first interned; existing clauses retain their id across reopens.
/// Used as the deduped key type in `StoredFormula.clause_ids`.
pub type ClauseId = u64;

/// A CNF clause: a disjunction of literals.
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
///   `Var` / `SkolemFn` / `Num` / `Str`.  Variables carry a `SyntacticLayer`
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
    /// term-level functor.  `id` interns to an ordinary `SyntacticLayer`
    /// symbol.
    Fn { id: SymbolId, args: Vec<CnfTerm> },
    /// A Skolem function application.
    SkolemFn { id: SymbolId, args: Vec<CnfTerm> },
    /// A numeric literal.
    Num(String),
    /// A string literal (includes surrounding double-quotes).
    Str(String),
}