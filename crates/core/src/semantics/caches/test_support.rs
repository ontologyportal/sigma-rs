// crates/core/src/semantics/caches/test_support.rs
//
// Shared fixtures for the per-cache test modules.  Each cache's tests live in
// its own file (`is_instance.rs`, `arity.rs`, …) and pull `BASE` / `kif_layer`
// / `base_layer` from here so the fixture is defined once.

use crate::semantics::SemanticLayer;
use crate::syntactic::SyntacticLayer;

/// A small SUMO-shaped ontology covering relations, predicates, and a couple of
/// class hierarchies — enough to exercise the IS-A / relation-metadata caches.
pub(crate) const BASE: &str = "
    (subclass Relation Entity)
    (subclass BinaryRelation Relation)
    (subclass Predicate Relation)
    (subclass BinaryPredicate Predicate)
    (subclass BinaryPredicate BinaryRelation)
    (instance subclass BinaryRelation)
    (domain subclass 1 Class)
    (domain subclass 2 Class)
    (instance instance BinaryPredicate)
    (domain instance 1 Entity)
    (domain instance 2 Class)
    (subclass Animal Entity)
    (subclass Human Entity)
    (subclass Human Animal)
";

/// Build a `SemanticLayer` from a KIF string (file tag `base`).
pub(crate) fn kif_layer(kif_str: &str) -> SemanticLayer {
    let mut store = SyntacticLayer::default();
    store.load_kif(kif_str, "base");
    SemanticLayer::new(store)
}

/// A `SemanticLayer` over [`BASE`].
pub(crate) fn base_layer() -> SemanticLayer { kif_layer(BASE) }

/// Build a `SemanticLayer` by ingesting `text` (tagged `file`) through the
/// TPTP parser (CNF `cnf(...)` / FOF `fof(...)`), driving the same
/// `SourceAdded` cascade `load_kif`/`load_kif_assert` use for KIF but with
/// `Parser::Tptp` — the `.p`/`.tptp` dialect and options `Parser::from_filename`
/// selects for those extensions.  Used by tests that need to inspect
/// post-ingest root-sentence shapes for TPTP input (e.g. clausal `(or …)`
/// roots), which are NOT reachable through `kif_layer`.
#[cfg(feature = "native-prover")]
pub(crate) fn tptp_layer(text: &str, file: &str) -> SemanticLayer {
    use crate::cache::events::Event;
    use crate::layer::Layer;
    use crate::parse::{Parser, TptpParseOptions};
    use crate::types::{FileOrigin, LocalProvenance, SourceFile};

    let store = SyntacticLayer::default();
    let source = SourceFile {
        parser: Parser::Tptp { options: Some(TptpParseOptions {
            formulas_only: false, keep_conjectures: true, ..TptpParseOptions::default()
        }) },
        name: file.to_owned(),
        path: std::path::PathBuf::from(file),
        origin: FileOrigin::Local(LocalProvenance::UNKNOWN),
        contents: text.to_owned(),
        prebuilt: None,
    };
    let _ = store.cascade(vec![Event::SourceAdded {
        session: std::sync::Arc::new(file.to_owned()),
        file:    source,
        staged:  false,
    }]);
    let _ = store.cascade(vec![Event::SessionAxiomatized { session: file.to_owned() }]);
    SemanticLayer::new(store)
}
