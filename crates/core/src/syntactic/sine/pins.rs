//! Which axioms SInE pins to one of their arguments, and to which.
//!
//! Under pinned selection (`SineParams::pinned_ownership`) a pinned axiom is
//! triggered only by its owning argument, regardless of tolerance and of
//! which symbol is least general.  Two groups of relations are always pinned,
//! each listed below with the symbols taken from `config.toml` via
//! `semantics::consts`; a third group, the relations the KB declares
//! transitive, is derived from the ontology (`semantic::transitive_relations`)
//! and pinned with the shape given by [`TRANSITIVE_PIN_OWNER_ARG`] and
//! [`TRANSITIVE_PIN_DEPTH_EXEMPT`].

use std::collections::HashMap;
use std::sync::OnceLock;

use once_cell::sync::Lazy;

use crate::semantics::consts::{
    DOMAIN_RELATION, DOMAIN_SUBCLASS_RELATION, INSTANCE_RELATION, RANGE_RELATION,
    RANGE_SUB_REL_CLASS, SUBATTRIBUTE_RELATION, SUBCLASS_RELATION, SUBRELATION_RELATION,
};
use crate::types::{Element, Sentence, Symbol, SymbolId};

/// A relation whose ground facts SInE pins to one of their arguments.
#[derive(Debug)]
pub(crate) struct PinnedRelation {
    /// The relation (head symbol) whose facts are pinned.
    pub relation: &'static Lazy<Symbol>,
    /// 1-based argument that owns the fact (`1` is the first argument after
    /// the head).  The fact is only pinned when that argument is a plain
    /// symbol.
    pub owner_arg: usize,
    /// Whether the fact's other symbols are traversed outside the depth
    /// limit.  When `false` they take an ordinary wave like any other
    /// reached symbol.
    pub depth_exempt: bool,
}

impl PinnedRelation {
    pub(crate) const fn new(
        relation: &'static Lazy<Symbol>,
        owner_arg: usize,
        depth_exempt: bool,
    ) -> Self {
        assert!(owner_arg >= 1, "owner_arg is 1-based");
        Self {
            relation,
            owner_arg,
            depth_exempt,
        }
    }

    /// The symbol that owns `sentence` under this pin: the sentence's
    /// `owner_arg`th argument, provided this relation heads it and that
    /// argument is a plain symbol.
    pub(crate) fn owner_of(&self, sentence: &Sentence) -> Option<SymbolId> {
        let Some(Element::Symbol(head)) = sentence.elements.first() else {
            return None;
        };
        if head.id() != self.relation.id() {
            return None;
        }
        owner_at(sentence, self.owner_arg)
    }
}

/// The plain symbol at 1-based argument `arg` of `sentence`, if there is one.
pub(crate) fn owner_at(sentence: &Sentence, arg: usize) -> Option<SymbolId> {
    match sentence.elements.get(arg) {
        Some(Element::Symbol(owner)) => Some(owner.id()),
        _ => None,
    }
}

/// Taxonomy edges, always pinned.  The child (first argument) owns the edge,
/// so seeding a class walks up its superclass chain.
pub(crate) static TAXONOMY_PINS: &[PinnedRelation] = &[
    PinnedRelation::new(&SUBCLASS_RELATION, 1, true),
    PinnedRelation::new(&INSTANCE_RELATION, 1, true),
    PinnedRelation::new(&SUBRELATION_RELATION, 1, true),
    PinnedRelation::new(&SUBATTRIBUTE_RELATION, 1, true),
];

/// Signature declarations, always pinned.  The relation they describe (first
/// argument) owns the declaration, and the argument class joins the fan-out.
pub(crate) static SIGNATURE_PINS: &[PinnedRelation] = &[
    PinnedRelation::new(&DOMAIN_RELATION, 1, true),
    PinnedRelation::new(&DOMAIN_SUBCLASS_RELATION, 1, true),
    PinnedRelation::new(&RANGE_RELATION, 1, true),
    PinnedRelation::new(&RANGE_SUB_REL_CLASS, 1, true),
];

/// Owning argument of a fact of a relation the KB declares transitive: the
/// first argument is the smaller one, so a forward chain `a -> b -> c` is
/// walked from `a`.
pub(crate) const TRANSITIVE_PIN_OWNER_ARG: usize = 1;

/// Whether the symbols reached through a transitive relation's facts are
/// traversed outside the depth limit.
pub(crate) const TRANSITIVE_PIN_DEPTH_EXEMPT: bool = true;

/// The always-pinned relations ([`TAXONOMY_PINS`] and [`SIGNATURE_PINS`])
/// keyed by head id, for fast lookup.
pub(crate) fn always_pinned_by_head() -> &'static HashMap<SymbolId, &'static PinnedRelation> {
    static MAP: OnceLock<HashMap<SymbolId, &'static PinnedRelation>> = OnceLock::new();
    MAP.get_or_init(|| {
        TAXONOMY_PINS
            .iter()
            .chain(SIGNATURE_PINS)
            .map(|pin| (pin.relation.id(), pin))
            .collect()
    })
}
