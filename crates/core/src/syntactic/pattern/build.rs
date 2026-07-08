//! Build a `SentencePattern` from a KIF expression string.

use std::collections::HashMap;

use crate::types::{Element, SentenceId, SymbolId};

use super::super::SyntacticLayer;
use super::matcher::PatternMatcher;
use super::types::{MatchKey, PatternElement, SentencePattern};

/// Error returned by [`SyntacticLayer::pattern_from_kif`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PatternFromKifError {
    /// The KIF string was empty or did not parse into any root sentence.
    NoRootSentence,
    /// A ground symbol in the pattern is not present in the KB's symbol table.
    ///
    /// The `String` is the offending symbol name exactly as it appeared in the
    /// KIF string.
    UnknownSymbol(String),
}

impl std::fmt::Display for PatternFromKifError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PatternFromKifError::NoRootSentence =>
                write!(f, "KIF pattern produced no root sentence"),
            PatternFromKifError::UnknownSymbol(name) =>
                write!(f, "unknown symbol '{name}' in KIF pattern"),
        }
    }
}

/// Recursively convert sentence `sid` from the temporary parse store into a
/// [`SentencePattern`].
///
/// Ground symbols are resolved by name against `real`.  Variables receive
/// [`PatternElement::AnyElement`] with a slot from `slot_map` (one slot per
/// distinct scoped id; same variable → same slot).
///
/// Returns `Err(name)` where `name` is the first ground symbol that could not
/// be resolved against `real`.
fn sentence_to_pattern(
    temp:      &SyntacticLayer,
    real:      &SyntacticLayer,
    sid:       SentenceId,
    slot_map:  &mut HashMap<SymbolId, usize>,
    next_slot: &mut usize,
) -> Result<SentencePattern, String> {
    let sentence = temp.sentence(sid)
        .ok_or_else(|| format!("<sid {sid}>"))?;
    let mut elems: Vec<PatternElement> = Vec::with_capacity(sentence.elements.len());

    // Snapshot to release the borrow before the recursive calls below.
    let elements: Vec<Element> = sentence.elements.iter().cloned().collect();

    for elem in &elements {
        let pat = match elem {
            Element::Symbol(sym) => {
                // A pattern referencing an unknown symbol could never match.
                let name = sym.name();
                if real.sym_id(&name).is_none() {
                    return Err(name.to_string());
                }
                PatternElement::Exact(MatchKey::Symbol((**sym).clone()))
            }
            Element::Variable { id, .. } => {
                // Each distinct scoped variable id maps to one capture slot, so the
                // same `?X` always binds to the same value.
                let slot = *slot_map.entry(*id).or_insert_with(|| {
                    let s = *next_slot;
                    *next_slot += 1;
                    s
                });
                PatternElement::AnyElement(slot)
            }
            Element::Op(op) => {
                PatternElement::Exact(MatchKey::Op(op.clone()))
            }
            Element::Literal(lit) => {
                PatternElement::Exact(MatchKey::Literal(lit.clone()))
            }
            Element::Sub(sub_sid) => {
                let inner = sentence_to_pattern(temp, real, *sub_sid, slot_map, next_slot)?;
                PatternElement::SubPattern(Box::new(inner))
            }
        };
        elems.push(pat);
    }

    Ok(SentencePattern(elems))
}

impl<'a> PatternMatcher<'a> {
    /// Build a [`SentencePattern`] from a KIF expression string.
    ///
    /// The string is parsed into a temporary [`SyntacticLayer`]; the first
    /// root sentence becomes the pattern template.  Each KIF variable
    /// (`?X`, `@Row`, etc.) is assigned a unique [`PatternElement::AnyElement`]
    /// slot — the same variable always gets the same slot, so
    /// `(instance ?X ?X)` generates a consistency check across both positions.
    ///
    /// Nested sub-formulas are converted to [`PatternElement::SubPattern`]
    /// recursively, so `(=> (instance ?X C) ?Q)` produces a two-level pattern
    /// with an inner `SubPattern` for the antecedent and an `AnyElement` for
    /// the consequent variable.
    ///
    /// # Errors
    ///
    /// - [`PatternFromKifError::NoRootSentence`] — `kif` was empty or did not
    ///   parse into a root sentence.
    /// - [`PatternFromKifError::UnknownSymbol`] — a ground symbol in `kif` is
    ///   absent from `self`'s symbol table.
    pub(crate) fn pattern_from_kif(&self, kif: &str) -> Result<SentencePattern, PatternFromKifError> {

        let mut temp = SyntacticLayer::default();
        temp.load_kif(kif, "_pattern_");

        // A single-formula pattern has exactly one root.
        let root_sid = temp.root_sids().into_iter().next()
            .ok_or(PatternFromKifError::NoRootSentence)?;

        let mut slot_map: HashMap<SymbolId, usize> = HashMap::new();
        let mut next_slot: usize = 0;

        sentence_to_pattern(&temp, self.store, root_sid, &mut slot_map, &mut next_slot)
            .map_err(PatternFromKifError::UnknownSymbol)
    }
}
