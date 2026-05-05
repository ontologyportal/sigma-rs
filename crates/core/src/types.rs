// crates/core/src/types.rs
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
use crate::parse::ast::Span;

// -- Id types -----------------------------------------------------------------

/// Stable symbol identifier.  Unique within a KnowledgeBase for its lifetime.
/// When persistence is enabled this value is identical to the LMDB sequence
/// key -- no remapping is ever required.
pub type SymbolId = u64;

/// Stable sentence / formula identifier.
pub type SentenceId = u64;

#[cfg(feature = "cnf")]
pub use crate::cnf::clause::{CnfLiteral, CnfTerm, Clause, ClauseId};

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
///
/// Every variant carries a [`Span`] locating the element in its
/// source file.  Spans are `#[serde(skip)]`-transparent to the
/// LMDB bincode format -- the on-disk payload is the same as
/// before per-element spans were added.  Rehydrated-from-LMDB
/// sentences have their spans defaulted to `Span::default()`,
/// which is effectively synthetic.
///
/// Consumers that construct Elements without source origin
/// (CNF clausifier, macro expansions, test fixtures) use
/// [`Span::synthetic`] so position queries skip them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Element {
    /// A ground symbol, referenced by its stable id.
    Symbol {
        id:   SymbolId,
        #[serde(skip)]
        span: Span,
    },
    /// A logical variable or row-variable.
    /// `id` is the interned symbol id for the scope-qualified name (e.g. `x@3`).
    Variable {
        id:     SymbolId,
        name:   String,
        is_row: bool,
        #[serde(skip)]
        span:   Span,
    },
    /// A string or numeric literal.
    Literal {
        lit:  Literal,
        #[serde(skip)]
        span: Span,
    },
    /// A nested sub-sentence.  The id indexes into the same flat sentence Vec
    /// owned by SyntacticLayer.
    Sub {
        sid:  SentenceId,
        #[serde(skip)]
        span: Span,
    },
    /// A logical operator (always at index 0 in operator sentences).
    Op {
        op:   OpKind,
        #[serde(skip)]
        span: Span,
    },
}

impl Element {
    /// Source range covering this element.  Returns the stored
    /// span verbatim -- no fallback / merging logic.  Synthetic
    /// elements (see [`Span::synthetic`]) return a synthetic span.
    pub fn span(&self) -> &Span {
        match self {
            Self::Symbol   { span, .. } => span,
            Self::Variable { span, .. } => span,
            Self::Literal  { span, .. } => span,
            Self::Sub      { span, .. } => span,
            Self::Op       { span, .. } => span,
        }
    }
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
    pub span: Span,
}

impl Sentence {
    /// True if this is an operator sentence (and, or, not, =>, <=>, forall, exists).
    pub fn is_operator(&self) -> bool {
        matches!(self.elements.first(), Some(Element::Op { .. }))
    }

    /// The operator kind, if this is an operator sentence.
    pub fn op(&self) -> Option<&OpKind> {
        match self.elements.first() {
            Some(Element::Op { op, .. }) => Some(op),
            _ => None,
        }
    }

    /// The head symbol id, if this is a symbol-headed sentence.
    pub fn head_symbol(&self) -> Option<SymbolId> {
        match self.elements.first() {
            Some(Element::Symbol { id, .. }) => Some(*id),
            _ => None,
        }
    }

    /// get the arity of the sentence (number of arguments)
    pub fn arity(&self) -> usize {
        self.elements.len() - 1
    }
}

// -- Occurrence ----------------------------------------------------------------

/// Position of a single symbol reference inside the knowledge base.
///
/// Produced by the occurrence index populated during
/// `SyntacticLayer::load` / `append_root_sentence`.  Every `Element::Symbol`
/// in a non-synthetic sentence generates one `Occurrence` entry;
/// rehydrated-from-LMDB and CNF-synthesised elements carry synthetic
/// spans and are filtered out of the index.
///
/// Useful beyond LSP — CLI tools ("sumo find-refs Human"), test
/// coverage reporters, code-walkers of any kind.  The shape is
/// deliberately small so it can be stored densely in the
/// per-symbol index.
#[derive(Debug, Clone)]
pub struct Occurrence {
    /// Sentence containing the reference.
    pub sid:  SentenceId,
    /// Index within the sentence's `elements` vector.
    pub idx:  usize,
    /// Source range of the symbol reference itself (not the
    /// containing sentence).
    pub span: Span,
    /// Role the symbol plays within the sentence.
    pub kind: OccurrenceKind,
}

/// Classification of a symbol occurrence by its position inside the
/// sentence.  `Head` means the symbol is `elements[0]` of a
/// top-level or nested form; `Arg` means it appears as any
/// subsequent argument (including deeply nested under `Sub`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OccurrenceKind {
    Head,
    Arg,
}

// -- Symbol --------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    /// Root sentences where this symbol is the predicate / function head.
    pub head_sentences: Vec<SentenceId>,
    /// **Axiom** sentences in which this symbol appears anywhere (including
    /// transitively through sub-sentences).  De-duplicated per axiom —
    /// `(subclass Dog Dog)` counts `Dog` once, not twice.
    ///
    /// "Axiom" means a promoted root sentence (fingerprint `session = None`).
    /// Session assertions do NOT update this index; the entry is a true
    /// reflection of the permanent axiom base.
    ///
    /// Consequence: `symbol.all_sentences.len()` is the symbol's generality
    /// in the SInE sense (number of axioms it appears in) and is always
    /// live — no recomputation needed on query.
    ///
    /// Populated in `KnowledgeBase::{make_session_axiomatic,
    /// promote_assertions_unchecked, open}` via
    /// `SyntacticLayer::register_axiom_symbols`.
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
