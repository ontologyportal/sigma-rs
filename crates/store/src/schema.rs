/// Persistent data types stored in LMDB.
///
/// `SymbolId` and `FormulaId` are stable 64-bit integers assigned by LMDB
/// auto-increment sequences.  They are the same `u64` type as
/// `sumo_parser_core::store::SymbolId` / `SentenceId` so that they can be
/// used interchangeably after ID remapping at commit time.
use serde::{Deserialize, Serialize};
use sumo_parser_core::tokenizer::OpKind;

pub type SymbolId  = u64;
pub type FormulaId = u64;

// ── Stored symbol ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSymbol {
    pub id:            SymbolId,
    pub name:          String,
    /// True for Skolem function/constant symbols generated during CNF conversion.
    pub is_skolem:     bool,
    /// Arity of a Skolem function (None for ordinary symbols).
    pub skolem_arity:  Option<usize>,
}

// ── Stored formula ────────────────────────────────────────────────────────────

/// A formula as stored in LMDB.  The `elements` field allows reconstruction
/// of an in-memory `KifStore` for semantic validation; the `clauses` field
/// holds the pre-computed CNF for theorem-prover queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredFormula {
    pub id:       FormulaId,
    /// The element list using *persistent* SymbolIds (after ID remapping).
    /// Sub-formulas are stored inline (recursively embedded) — no separate
    /// LMDB entries for sub-sentences.
    pub elements: Vec<StoredElement>,
    /// Pre-computed CNF clauses (Skolemized, variable-standardised).
    pub clauses:  Vec<Clause>,
    /// Session key; `None` for base-KB formulas.
    pub session:  Option<String>,
}

// ── Stored element ────────────────────────────────────────────────────────────

/// Like `sumo_parser_core::store::Element` but without `Span` information
/// (discarded at commit time) and with sub-sentences stored inline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StoredElement {
    Symbol(SymbolId),
    Variable { id: SymbolId, name: String, is_row: bool },
    Literal(StoredLiteral),
    /// Inlined sub-formula (was `Element::Sub(SentenceId)` in the in-memory store).
    Sub(Box<StoredFormula>),
    Op(OpKind),
}

/// String/number literal — mirrors `sumo_parser_core::store::Literal` but
/// independently defined so the store crate does not depend on core's internals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StoredLiteral {
    Str(String),
    Number(String),
}

// ── CNF types ─────────────────────────────────────────────────────────────────

/// A CNF clause — a disjunction of literals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clause {
    pub literals: Vec<CnfLiteral>,
}

/// A single CNF literal (positive or negative atom).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CnfLiteral {
    pub positive: bool,
    /// The predicate.  Usually `CnfTerm::Const(id)`, but can be
    /// `CnfTerm::Var(id)` for higher-order propositional variables.
    pub pred: CnfTerm,
    pub args: Vec<CnfTerm>,
}

/// A CNF term — an argument or predicate position in a literal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CnfTerm {
    /// A ground constant (ordinary symbol or class name).
    Const(SymbolId),
    /// A universally quantified variable (scope-named, e.g. `X@5`).
    Var(SymbolId),
    /// A Skolem function application.
    SkolemFn { id: SymbolId, args: Vec<CnfTerm> },
    /// A numeric literal.
    Num(String),
    /// A string literal (includes surrounding quotes).
    Str(String),
}
