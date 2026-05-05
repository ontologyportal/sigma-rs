// crates/core/src/layer.rs
//
// Generic stack-position trait for the layered KB architecture.
//
// The KnowledgeBase is built as a stack of layers: SyntacticLayer (raw
// parse store) at the bottom, SemanticLayer (taxonomy + semantic queries)
// in the middle, TranslationLayer (TPTP translation state) at the top.
// Each layer owns its inner — the layer directly below — so downward
// traversal via `inner()` is a direct field reference.
//
// Upward traversal via `outer()` is *not* wired into the layer values
// themselves: doing so would create self-referential structs. Callers
// that need an outer layer go through `KnowledgeBase` accessors
// (`kb.semantic()`, `kb.translation()`) instead. The `outer()` method
// is part of the trait so each layer can declare its position in the
// stack at the type level, even though it currently returns `None`.

/// Stack-position trait. Each layer announces what's directly below
/// (`Inner`) and above (`Outer`) it.
///
/// `#[allow(dead_code)]` — the trait is currently a structural marker
/// (the impls give type-level documentation of the stack); call sites
/// reach the inner layers via direct field access (`kb.layer.semantic`)
/// rather than through this trait, so neither method is invoked yet.
/// Kept on the API surface for future generic code that traverses the
/// stack uniformly.
#[allow(dead_code)]
pub(crate) trait Layer {
    type Inner: Layer;
    type Outer: Layer;

    /// Reference to the layer directly below `self`, or `None` if
    /// `self` is the bottom of the stack.
    fn inner(&self) -> Option<&Self::Inner>;

    /// Reference to the layer directly above `self`, or `None` if
    /// `self` is the top of the stack OR the back-pointer is not
    /// wired up (the current default — see module docs).
    fn outer(&self) -> Option<&Self::Outer>;
}

/// Marker terminating the stack at either end. Has no inhabitants.
#[allow(dead_code)]
pub(crate) enum NoLayer {}

impl Layer for NoLayer {
    type Inner = NoLayer;
    type Outer = NoLayer;
    fn inner(&self) -> Option<&Self::Inner> { None }
    fn outer(&self) -> Option<&Self::Outer> { None }
}
