//! Shared data types for the semantic layer.

use crate::{SentenceId, SymbolId};
use crate::types::SessionId;

// ---------------------------------------------------------------------------
// Relations (domain / range axioms)
// ---------------------------------------------------------------------------

/// Kind of domain/range axiom that constrains a relation argument.
pub(crate) enum RelationRelation {
    /// `(domain rel n class)`.
    Domain,
    /// `(domainSubclass rel n class)`.
    DomainSubclass,
    /// `(range rel class)`.
    Range,
    /// `(rangeSubclass rel class)`.
    RangeSubclass,
}

impl RelationRelation {
    /// Parses a relation keyword into a [`RelationRelation`], or `None` if the
    /// keyword is not one of `domain`, `domainSubclass`, `range`, `rangeSubclass`.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "domain"         => Some(RelationRelation::Domain),
            "domainSubclass" => Some(RelationRelation::DomainSubclass),
            "range"          => Some(RelationRelation::Range),
            "rangeSubclass"  => Some(RelationRelation::RangeSubclass),
            _ => None,
        }
    }
}

/// Describes the expected type of a relation argument value.
#[derive(Debug, Clone)]
pub(crate) enum RelationDomain {
    /// Argument must be an instance of this class.
    Domain(SymbolId),
    /// Argument must be a subclass of this class.
    DomainSubclass(SymbolId),
    /// The domain of the relation could not be determined.
    Unknown,
}

impl RelationDomain {
    /// Returns the constraining class's [`SymbolId`], or `None` when unknown.
    pub(crate) fn id(&self) -> Option<SymbolId> {
        match self {
            Self::Domain(id) | Self::DomainSubclass(id) => Some(*id),
            _ => None
        }
    }
}

/// Describes the expected type of a relation return value.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) enum RelationRange {
    /// Argument must be an instance of this class.
    Range(SymbolId),
    /// Argument must be a subclass of this class.
    RangeSubclass(SymbolId),
    /// The range of the relation could not be determined.
    Unknown,
}

impl RelationRange {
    /// Returns the constraining class's [`SymbolId`], or `None` when unknown.
    #[allow(dead_code)]
    pub(crate) fn id(&self) -> Option<SymbolId> {
        match self {
            Self::Range(id) | Self::RangeSubclass(id) => Some(*id),
            _ => None
        }
    }
}

pub use super::taxonomy::{TaxDirection, TaxRelation};

// ---------------------------------------------------------------------------
// Documentation entries
// ---------------------------------------------------------------------------

/// A single documentation blurb as authored in the ontology.
///
/// `text` has the surrounding quotes of the KIF string literal stripped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocEntry {
    /// The documentation relation symbol (e.g. `documentation`).
    pub rel:      SymbolId,
    /// Language tag for the blurb (e.g. `EnglishLanguage`).
    pub language: String,
    /// The blurb text, with surrounding KIF string quotes stripped.
    pub text:     String,
}

// ---------------------------------------------------------------------------
// Type inference
// ---------------------------------------------------------------------------

/// Helper Enum. Used to wrap output of [`SemanticLayer::infer_class`](super::SemanticLayer::infer_class)
#[derive(Debug, Clone)]
pub(crate) enum ClassInference {
    /// The inference could not determine the type of the symbol
    Unknown,
    /// The symbol is a class and not an instance of any particular class
    Class,
    /// The symbol is definitively an instance of a single class. Alternatively, a
    /// symbol was declared an instance of multiple classes, but those classes have a
    /// common derivation and one class is the most specific
    ///
    /// *For example*:
    /// ```text
    /// (subclass Physical Entity)
    /// (subclass Animal Physical)
    /// (subclass Mammal Animal)
    /// (subclass Primate Mammal)
    /// (subclass Human Mammal)
    /// (instance Bobo Primate)
    /// (instance Bobo Animal)
    /// ```
    ///
    /// Calling `SemanticLayer::infer_class` on Bobo would return `ClassInference::Single(Primate)`
    /// as both `Primate` and `Animal` belong to the same taxonomy and `Primate` is the most specific
    Single(SymbolId),
    /// The symbol belongs to multiple classes that could not be collapsed to a single class
    ///
    /// *For example*:
    /// ```text
    /// (subclass Physical Entity)
    /// (subclass Animal Physical)
    /// (subclass Mammal Animal)
    /// (subclass Primate Mammal)
    /// (subclass Human Primate)
    /// (subclass Doctor Human)
    /// (subclass Singer Human)
    /// (subclass PopStar Singer)
    /// (instance Bob Primate)
    /// (instance Bob Doctor)
    /// (instance Bob Singer)
    /// ```
    ///
    /// If `SemanticLayer::infer_class` is called on `Bob` it would return
    /// `ClassInference::Multiple([Singer, Doctor])` as `Primate` is more general
    /// than both and could be collapsed, but `Doctor` and `Singer` are not
    /// mutual super/sub classes and therefore could not be collapsed
    Multiple(Vec<SymbolId>),
}

/// Where a class classification was derived — distinguishes an unconditional
/// ground fact from one that only holds inside a formula's logical structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClassScope {
    /// From a ground, unconditional atom (a root assertion such as
    /// `(mother Mary Jesus)`) or the taxonomy — true everywhere.
    Global,
    /// From within a logical context (a rule hypothesis, a quantifier body):
    /// valid only for reasoning *local* to the binding formula identified by the
    /// `SentenceId`.  This is how a variable's class is recovered — `?X` in
    /// `(=> (instance ?X Animal) …)` is an `Animal` only within that rule.
    Local(SentenceId),
}

/// A symbol/variable's inferred class together with the scope it holds in.
/// Returned by [`crate::semantics::SemanticLayer::classify_formula`].
#[derive(Debug, Clone)]
pub(crate) struct ScopedClass {
    /// The inferred class.
    pub class: ClassInference,
    /// The scope in which the classification holds.
    pub scope: ClassScope,
}

// ---------------------------------------------------------------------------
// Query scope (base taxonomy vs per-session overlay)
// ---------------------------------------------------------------------------

/// Which logical KB a taxonomy / class query reasons over.
///
/// The taxonomy and its derived class caches are split into a shared, stable
/// **base** (promoted axioms only) plus per-session **overlays** (a session's
/// un-promoted transient edges).  A query in `Session(id)` sees `Base` ∪ that
/// session's overlay, so concurrent sessions never observe each other's
/// hypotheses and the base is never polluted by transient asserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(crate) enum Scope {
    /// Promoted axioms only — the shared, stable base KB.
    Base,
    /// `Base` ∪ this session's un-promoted transient edges.
    Session(SessionId),
}

/// A cache key `K` tagged with the [`Scope`] it was computed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(crate) struct Scoped<K> {
    /// Scope the key was computed in.
    pub scope: Scope,
    /// The unscoped cache key.
    pub key:   K,
}
