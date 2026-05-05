// crates/core/src/relation.rs
//
// How to deal with relations semantically. This submodule
// deals with relation building - domains, ranges, arity, etc.

// use super::SemanticLayer;

use crate::SymbolId;

pub(crate) enum RelationRelation {
    Domain,
    DomainSubclass,
    Range,
    RangeSubclass
}

impl RelationRelation {
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
}

impl RelationDomain {
    pub(crate) fn id(&self) -> SymbolId {
        match self {
            Self::Domain(id) | Self::DomainSubclass(id) => *id,
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
}

impl RelationRange {
    #[allow(dead_code)]
    pub(crate) fn id(&self) -> SymbolId {
        match self {
            Self::Range(id) | Self::RangeSubclass(id) => *id,
        }
    }
}

