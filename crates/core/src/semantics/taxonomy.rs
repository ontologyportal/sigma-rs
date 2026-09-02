// crates/core/src/semantics/taxonomy.rs
//
// Manage and construct taxonomies for KB from SemanticLayer

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

use crate::semantics::consts::{
    INSTANCE_RELATION, SUBATTRIBUTE_RELATION, SUBCLASS_RELATION, SUBRELATION_RELATION,
};
use crate::types::{Symbol, SymbolId};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Hash)]
pub enum TaxRelation {
    Subclass,
    Instance,
    Subrelation,
    SubAttribute,
}

impl std::str::FromStr for TaxRelation {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            s if *SUBCLASS_RELATION.name() == *s => Ok(TaxRelation::Subclass),
            s if *INSTANCE_RELATION.name() == *s => Ok(TaxRelation::Instance),
            s if *SUBRELATION_RELATION.name() == *s => Ok(TaxRelation::Subrelation),
            s if *SUBATTRIBUTE_RELATION.name() == *s => Ok(TaxRelation::SubAttribute),
            _ => Err(()),
        }
    }
}

impl TaxRelation {
    pub fn from_id(s: SymbolId) -> Option<Self> {
        match s {
            s if SUBCLASS_RELATION.id() == s => Some(TaxRelation::Subclass),
            s if INSTANCE_RELATION.id() == s => Some(TaxRelation::Instance),
            s if SUBRELATION_RELATION.id() == s => Some(TaxRelation::Subrelation),
            s if SUBATTRIBUTE_RELATION.id() == s => Some(TaxRelation::SubAttribute),
            _ => None,
        }
    }

    /// Return the KIF relation name for this variant.
    pub(crate) fn as_sym(&self) -> &'static Lazy<Symbol> {
        match self {
            TaxRelation::Subclass => &SUBCLASS_RELATION,
            TaxRelation::Instance => &INSTANCE_RELATION,
            TaxRelation::Subrelation => &SUBRELATION_RELATION,
            TaxRelation::SubAttribute => &SUBATTRIBUTE_RELATION,
        }
    }
}

/// An enum definiting the edge's origin (is it coming or going from
/// this item)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum TaxDirection {
    /// The taxonomy relationship indicates that the given symbol is an
    /// originator for the taxonomy relationship
    From(SymbolId),
    /// The taxonomy relationship indicates that the given symbol is the
    /// consumer for the taxonomy relationship
    To(SymbolId),
}

#[cfg(test)]
mod tests {
    // use super::*;
}
