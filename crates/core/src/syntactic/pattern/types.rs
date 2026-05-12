//! Pattern data types (`MatchKey`, `PatternElement`, `SentencePattern`,
//! `Bindings`), span-free element comparison, and pattern instantiation.

use std::collections::HashMap;

use crate::parse::ast::OpKind;
use crate::types::{Element, ElementVec, InternedSym, Literal, SentenceId, Symbol, SymbolId};

// ---------------------------------------------------------------------------
// MatchKey — span-free key for Exact matching
// ---------------------------------------------------------------------------

/// Span-free representation of what an `Exact` pattern position must match.
///
/// [`Element`] carries a span and has no `PartialEq`; `MatchKey` is the
/// comparable, hashable alternative used in patterns.
///
/// `MatchKey` is flat — it covers `Symbol`, `Op`, and `Literal` positions only.
/// Recursive sub-sentence matching is handled by the separate
/// [`PatternElement::SubPattern`] variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MatchKey {
    /// A ground symbol position, keyed by its [`Symbol`] name.
    Symbol(Symbol),
    /// A variable position, keyed by its scope-qualified [`SymbolId`].
    Var(SymbolId),
    /// An operator position.
    Op(OpKind),
    /// A literal position.
    Literal(Literal),
}

impl MatchKey {
    /// Returns `true` if `elem` structurally matches this key (ignoring span).
    pub(crate) fn matches(&self, elem: &Element) -> bool {
        match (self, elem) {
            (MatchKey::Symbol(sym), Element::Symbol(esym))       => sym == &**esym,
            (MatchKey::Var(id),     Element::Variable { id: eid, .. }) => id == eid,
            (MatchKey::Op(op),      Element::Op(eop))            => op == eop,
            (MatchKey::Literal(l),  Element::Literal(el))        => l == el,
            _ => false,
        }
    }

    /// Constructs a synthetic [`Element`] from this key.
    #[allow(dead_code)]
    pub(crate) fn to_element(&self) -> Element {
        match self {
            MatchKey::Symbol(sym) => Element::Symbol(InternedSym(sym.clone())),
            // A variable key cannot reconstruct a full `Element::Variable`
            // (no display name / var_index here); emit it as a symbol leaf.
            MatchKey::Var(id)     => Element::Symbol(InternedSym(Symbol::from(format!("{id:#x}")))),
            MatchKey::Op(op)      => Element::Op(op.clone()),
            MatchKey::Literal(l)  => Element::Literal(l.clone()),
        }
    }
}

/// Convert an [`Element`] to a [`MatchKey`] for span-free comparison.
///
/// Returns `None` for `Sub` elements — those are not representable as a flat
/// `MatchKey`.  Use [`PatternElement::SubPattern`] to match a `Sub` by its
/// contents, or [`PatternElement::AnySubSentence`] to capture its id.
fn element_to_match_key(elem: &Element) -> Option<MatchKey> {
    match elem {
        Element::Symbol(sym)         => Some(MatchKey::Symbol((**sym).clone())),
        Element::Op(op)              => Some(MatchKey::Op(op.clone())),
        Element::Literal(lit)        => Some(MatchKey::Literal(lit.clone())),
        // Variables key on their scope-qualified id (distinct from symbols).
        Element::Variable { id, .. } => Some(MatchKey::Var(*id)),
        _ => None,
    }
}

/// Compare two elements for structural equality, ignoring spans.
///
/// Returns `false` for `Sub` elements — comparing sub-sentences by content
/// requires traversing the syntactic store, which is outside the scope of this
/// function.  Use [`PatternElement::SubPattern`] for structural sub-sentence
/// matching.
pub(crate) fn elements_eq_span_free(a: &Element, b: &Element) -> bool {
    match (element_to_match_key(a), element_to_match_key(b)) {
        (Some(ka), Some(kb)) => ka == kb,
        _ => false,
    }
}

/// Like [`elements_eq_span_free`] but also handles `Sub` elements by comparing
/// their [`SentenceId`]s.  Used for [`PatternElement::AnyElement`] consistency
/// checks.
pub(crate) fn elements_eq_any_span_free(a: &Element, b: &Element) -> bool {
    match (a, b) {
        (Element::Sub(sa), Element::Sub(sb)) => sa == sb,
        _ => elements_eq_span_free(a, b),
    }
}

// ---------------------------------------------------------------------------
// Pattern types
// ---------------------------------------------------------------------------

/// One position in a [`SentencePattern`].
///
/// The five variants cover all matching strategies:
///
/// | Variant | Matches | Context needed? |
/// |---------|---------|-----------------|
/// | `Exact(key)` | A specific Symbol, Op, or Literal | No |
/// | `SubPattern(pat)` | A `Sub` whose sentence matches `pat` recursively | Yes — `SyntacticLayer` in `match_pattern` |
/// | `AnyCapture(idx)` | Any non-`Sub` element; binds to slot `idx` | No |
/// | `AnySubSentence(idx)` | Any `Sub`; binds its `SentenceId` to slot `idx` | No |
/// | `AnyElement(idx)` | Any element including `Sub`; binds to slot `idx` | No |
///
/// **Slot discipline for `SubPattern`:** bindings from the inner pattern are
/// merged flat into the outer `Bindings`.  Assign non-overlapping slot numbers
/// across the entire pattern (inner and outer levels combined) to prevent silent
/// overwrites on collision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PatternElement {
    /// Must match exactly (span-free, via [`MatchKey`]).
    Exact(MatchKey),
    /// The element must be a `Sub`; the sub-sentence's elements must match
    /// `inner_pat` recursively.  Bindings from the inner match are merged into
    /// the outer [`Bindings`] using the **same flat slot namespace**.
    SubPattern(Box<SentencePattern>),
    /// Matches any single non-`Sub` element; binds it to capture slot `idx`.
    ///
    /// On a second occurrence of the same `idx`, the element must equal the
    /// first binding (consistency check).
    AnyCapture(usize),
    /// Matches a `Sub` element; binds its [`SentenceId`] to slot `idx`.
    #[allow(dead_code)]
    AnySubSentence(usize),
    /// Matches **any** element, including `Sub`; binds the raw [`Element`] to
    /// capture slot `idx` in [`Bindings::elements`].
    ///
    /// Unlike [`AnyCapture`], this variant also accepts `Sub` elements.
    ///
    /// On a second occurrence of the same `idx`, the element must equal the
    /// first binding (consistency check; `Sub` elements are compared by
    /// [`SentenceId`], all others span-free via [`elements_eq_span_free`]).
    AnyElement(usize),
    /// Variable-arity wildcard: consumes **0 or more** consecutive elements that
    /// do not match the *next* pattern element, stopping at the first element
    /// the remainder of the pattern matches (lazy, first-match).  A trailing
    /// `Glob` consumes everything left.
    ///
    /// `[Exact(R), Glob, Exact(T), Glob]` matches any `(R … T …)` regardless of
    /// arity.  Non-capturing — it binds no slot.  A pattern containing `Glob`
    /// matches via the variable-length engine rather than the strict zip; a
    /// `Glob` cannot be instantiated (see [`instantiate_pattern`]).
    Glob,
    /// Like [`Glob`](Self::Glob), but records the number of elements it consumed
    /// into [`Bindings::glob_lens`] at slot `idx`, recovering an argument
    /// *position* without a re-scan.  Same lazy first-match semantics and same
    /// "matches via the variable-length engine / cannot be instantiated" rules.
    #[allow(dead_code)]
    GlobCapture(usize),
}

/// A flat pattern matched against a sentence's element list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SentencePattern(pub Vec<PatternElement>);

// ---------------------------------------------------------------------------
// Bindings
// ---------------------------------------------------------------------------

/// Captured elements from a successful pattern match.
#[derive(Debug, Clone, Default)]
pub(crate) struct Bindings {
    /// `AnyCapture` bindings: slot index → captured [`Element`].
    pub elements: HashMap<usize, Element>,
    /// `AnySubSentence` (and `SubPattern` side-channel) bindings:
    /// slot index → captured [`SentenceId`].
    pub sub_sids: HashMap<usize, SentenceId>,
    /// `GlobCapture` bindings: slot index → number of elements the glob consumed.
    /// For a pattern `[Exact(R), GlobCapture(0), Exact(T), …]`, `T`'s index is
    /// `1 + glob_lens[0]`.
    pub glob_lens: HashMap<usize, usize>,
}

/// Instantiate a `SentencePattern` using `bindings`, producing an [`ElementVec`].
///
/// Returns `None` if any `AnyCapture` or `AnySubSentence` slot referenced by
/// the pattern is absent from `bindings`, or if a [`PatternElement::SubPattern`]
/// position is encountered.  `SubPattern` carries a structural *constraint*, not
/// a concrete element to emit — use [`PatternElement::AnySubSentence`] to
/// capture and re-emit a matched sub-sentence.
#[allow(dead_code)]
pub(crate) fn instantiate_pattern(pat: &SentencePattern, bindings: &Bindings) -> Option<ElementVec> {
    let mut out: ElementVec = ElementVec::with_capacity(pat.0.len());
    for p in &pat.0 {
        let elem = match p {
            PatternElement::Exact(key) => key.to_element(),
            PatternElement::SubPattern(_) => {
                // SubPattern positions cannot be instantiated.
                return None;
            }
            PatternElement::AnyCapture(idx) => {
                bindings.elements.get(idx)?.clone()
            }
            PatternElement::AnySubSentence(idx) => {
                let sid = *bindings.sub_sids.get(idx)?;
                Element::Sub(sid)
            }
            PatternElement::AnyElement(idx) => {
                bindings.elements.get(idx)?.clone()
            }
            PatternElement::Glob | PatternElement::GlobCapture(_) => {
                // A glob binds no single element and cannot be instantiated.
                return None;
            }
        };
        out.push(elem);
    }
    Some(out)
}
