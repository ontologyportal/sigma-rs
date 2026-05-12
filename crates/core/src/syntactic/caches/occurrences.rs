//! `syntactic::occurrences` — reverse index from a symbol *name* to every
//! occurrence of it in the KB (`String → HashSet<Occurrence>`).
//!
//! Reacts to `FormulaAdded` / `FormulaRemoved` (keyed by formula fingerprint),
//! resolves each fingerprint to its `AstNode` via the `SourceStore`, and walks
//! the tree recording one `Occurrence` per `AstNode::Symbol`.
//!
//! Keyed by symbol *name* (not `SymbolId`): the raw AST carries names.
//! Variables are skipped.
//!
//! There is no compute-on-miss — a missing key means "no occurrences".

use std::collections::HashSet;
use std::sync::Arc;

use crate::syntactic::caches::source::SourceCache;
use crate::{AstNode, OccurrenceKind};
use crate::cache::events::{Event, EventKind};
use crate::cache::{EagerMapBehavior, EntryCache};
use crate::syntactic::SyntacticLayer;
use crate::types::Occurrence;

/// Behavior for the `syntactic::occurrences` eager keyed index.
#[derive(Debug, Default)]
pub(crate) struct OccurrenceIndex;

impl EagerMapBehavior for OccurrenceIndex {
    type Parent = SyntacticLayer;
    type Key    = String;
    type Value  = Arc<HashSet<Occurrence>>;
    type Side   = ();
    type SideSnapshot = ();

    const NAME: &'static str = "syntactic::occurrences";

    fn consumes(&self) -> &'static [EventKind] {
        &[EventKind::FormulaAdded, EventKind::FormulaRemoved]
    }

    fn reads(&self) -> &'static [&'static str] {
        &[SourceCache::NAME]
    }

    fn react(
        &self,
        parent: &SyntacticLayer,
        events: &[&Event],
        store:  &EntryCache<String, Arc<HashSet<Occurrence>>>,
        _side:  &(),
    ) -> Vec<Event> {
        for e in events {
            match e {
                Event::FormulaAdded { node, .. } => {
                    let Some(ast) = parent.source.get(node)
                    else { continue };
                    let mut entries: Vec<(String, Occurrence)> = Vec::new();
                    index_ast(*node, &ast, &mut entries);
                    for (name, occ) in entries {
                        store.modify_entry(name, |v| { Arc::make_mut(v).insert(occ); });
                    }
                }
                Event::FormulaRemoved { node } => {
                    // Drop this formula's occurrences, and any name whose set empties.
                    store.retain(|_, occs| {
                        let set = Arc::make_mut(occs);
                        set.retain(|o| o.node != *node);
                        !set.is_empty()
                    });
                }
                _ => {}
            }
        }
        Vec::new()
    }
}

/// Walk `ast` (a formula tree rooted at fingerprint `node`), pushing one
/// `(name, Occurrence)` per non-synthetic `AstNode::Symbol`.  `kind` is `Head`
/// for the first element of a list, `Arg` otherwise.  Lists recurse; variables,
/// operators, and literals are not indexed.
fn index_ast(node: u64, ast: &AstNode, out: &mut Vec<(String, Occurrence)>) {
    let AstNode::List { elements, .. } = ast else { return };
    for (i, el) in elements.iter().enumerate() {
        match el {
            AstNode::Symbol { name, span } if !span.is_synthetic() => {
                let kind = if i == 0 { OccurrenceKind::Head } else { OccurrenceKind::Arg };
                out.push((name.clone(), Occurrence { node, span: span.clone(), kind }));
            }
            AstNode::List { .. } => index_ast(node, el, out),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::parse::{OpKind, Span};

    // -- builders -------------------------------------------------------------
    // Each token gets a distinct `offset` so its `Span` (the occurrence's
    // identity) is unique — otherwise same-span occurrences would dedup in the
    // `HashSet`.

    fn sp(file: &str, offset: usize) -> Span {
        Span { file: file.into(), line: 1, col: 1, offset, end_line: 1, end_col: 1, end_offset: offset + 1 }
    }
    fn sym(name: &str, file: &str, offset: usize) -> AstNode {
        AstNode::Symbol { name: name.into(), span: sp(file, offset) }
    }
    fn var(name: &str, file: &str, offset: usize) -> AstNode {
        AstNode::Variable { name: name.into(), span: sp(file, offset) }
    }
    fn op(kind: OpKind, file: &str, offset: usize) -> AstNode {
        AstNode::Operator { op: kind, span: sp(file, offset) }
    }
    fn list(elems: Vec<AstNode>) -> AstNode {
        AstNode::List { elements: elems, span: sp("x", 0) }
    }

    /// Sorted names extracted from an `index_ast` result.
    fn names(out: &[(String, Occurrence)]) -> Vec<String> {
        let mut v: Vec<String> = out.iter().map(|(n, _)| n.clone()).collect();
        v.sort();
        v
    }
    fn kind_of(out: &[(String, Occurrence)], name: &str) -> OccurrenceKind {
        out.iter().find(|(n, _)| n.as_str() == name).unwrap().1.kind
    }

    // -- index_ast: the pure source walk --------------------------------------

    #[test]
    fn indexes_head_and_args_with_node_fingerprint() {
        // (subclass Human Animal)
        let ast = list(vec![sym("subclass", "f", 1), sym("Human", "f", 2), sym("Animal", "f", 3)]);
        let mut out = Vec::new();
        index_ast(7, &ast, &mut out);

        assert_eq!(names(&out), ["Animal", "Human", "subclass"]);
        assert!(out.iter().all(|(_, o)| o.node == 7), "every occurrence carries the formula fingerprint");
        assert_eq!(kind_of(&out, "subclass"), OccurrenceKind::Head);
        assert_eq!(kind_of(&out, "Human"), OccurrenceKind::Arg);
        assert_eq!(kind_of(&out, "Animal"), OccurrenceKind::Arg);
    }

    #[test]
    fn recurses_into_nested_lists_skipping_operators_and_variables() {
        // (=> (instance ?X Dog) (barks ?X))
        let inner1 = list(vec![sym("instance", "f", 10), var("X", "f", 11), sym("Dog", "f", 12)]);
        let inner2 = list(vec![sym("barks", "f", 13), var("X", "f", 14)]);
        let ast    = list(vec![op(OpKind::Implies, "f", 9), inner1, inner2]);
        let mut out = Vec::new();
        index_ast(1, &ast, &mut out);

        // `=>` (operator) and `?X` (variable) are not indexed; nested symbols are.
        assert_eq!(names(&out), ["Dog", "barks", "instance"]);
        // Each nested list's first symbol is its Head.
        assert_eq!(kind_of(&out, "instance"), OccurrenceKind::Head);
        assert_eq!(kind_of(&out, "barks"), OccurrenceKind::Head);
        assert_eq!(kind_of(&out, "Dog"), OccurrenceKind::Arg);
    }

    #[test]
    fn skips_literals_and_synthetic_spans() {
        // (foo "str" 42 Ghost) where Ghost carries a synthetic span.
        let ast = list(vec![
            sym("foo", "f", 1),
            AstNode::Str    { value: "str".into(), span: sp("f", 2) },
            AstNode::Number { value: "42".into(),  span: sp("f", 3) },
            AstNode::Symbol { name: "Ghost".into(), span: Span::synthetic() },
        ]);
        let mut out = Vec::new();
        index_ast(1, &ast, &mut out);

        assert_eq!(names(&out), ["foo"], "literals and synthetic-span symbols are not indexed");
    }

    // -- the reactor (FormulaAdded / FormulaRemoved) --------------------------

    /// A layer whose `source` store already holds `nodes` (fingerprint → AST),
    /// as if the `source` reactor had run.
    fn layer_with(nodes: &[(u64, AstNode)]) -> SyntacticLayer {
        let layer = SyntacticLayer::default();
        for (h, ast) in nodes {
            layer.source.update(*h, ast.clone());
        }
        layer
    }
    fn sess() -> Arc<String> { Arc::new("t".to_string()) }

    #[test]
    fn formula_added_indexes_the_resolved_ast() {
        let layer = layer_with(&[(1, list(vec![
            sym("subclass", "a", 1), sym("Human", "a", 2), sym("Animal", "a", 3),
        ]))]);
        let store: EntryCache<String, Arc<HashSet<Occurrence>>> = EntryCache::default();

        OccurrenceIndex.react(&layer, &[&Event::FormulaAdded { node: 1, session: sess() }], &store, &());

        assert_eq!(store.get(&"subclass".to_string()).unwrap().len(), 1);
        assert_eq!(store.get(&"Animal".to_string()).unwrap().len(), 1);
        let human = store.get(&"Human".to_string()).expect("Human indexed");
        assert_eq!(human.len(), 1);
        assert_eq!(human.iter().next().unwrap().node, 1, "occurrence carries its formula fingerprint");
    }

    #[test]
    fn formula_added_for_unknown_node_is_a_noop() {
        let layer = layer_with(&[]);
        let store: EntryCache<String, Arc<HashSet<Occurrence>>> = EntryCache::default();

        // The formula was removed between emit and now — nothing to index.
        OccurrenceIndex.react(&layer, &[&Event::FormulaAdded { node: 99, session: sess() }], &store, &());

        assert!(store.is_empty());
    }

    #[test]
    fn formula_removed_purges_only_that_node() {
        // Two formulas both mention `Human`.
        let layer = layer_with(&[
            (1, list(vec![sym("subclass", "a", 1), sym("Human", "a", 2), sym("Animal", "a", 3)])),
            (2, list(vec![sym("instance", "b", 10), sym("Human", "b", 11), sym("Thing", "b", 12)])),
        ]);
        let store: EntryCache<String, Arc<HashSet<Occurrence>>> = EntryCache::default();
        OccurrenceIndex.react(&layer, &[
            &Event::FormulaAdded { node: 1, session: sess() },
            &Event::FormulaAdded { node: 2, session: sess() },
        ], &store, &());
        assert_eq!(store.get(&"Human".to_string()).unwrap().len(), 2, "Human referenced by both formulas");

        // Drop formula 1.
        OccurrenceIndex.react(&layer, &[&Event::FormulaRemoved { node: 1 }], &store, &());

        let human = store.get(&"Human".to_string()).expect("Human still referenced by formula 2");
        assert_eq!(human.len(), 1);
        assert!(human.iter().all(|o| o.node == 2));
        assert!(store.get(&"Animal".to_string()).is_none(), "Animal was only in formula 1 — its now-empty key is dropped");
        assert!(store.get(&"subclass".to_string()).is_none());
        assert!(store.get(&"Thing".to_string()).is_some(), "formula 2's symbols are untouched");
    }

    // -- full load pipeline (cascade-driven indexing) -------------------------

    #[test]
    fn occurrences_indexed_for_root_symbols() {
        let mut store = SyntacticLayer::default();
        store.load_kif("(subclass Human Animal)", "t.kif");
        let occs = store.occurrences.get(&"Human".to_string()).expect("Human has occurrences");
        assert_eq!(occs.len(), 1);
        assert_eq!(occs.iter().next().unwrap().kind, OccurrenceKind::Arg);

        let sub_occs = store.occurrences.get(&"subclass".to_string()).expect("subclass has occurrences");
        assert_eq!(sub_occs.iter().next().unwrap().kind, OccurrenceKind::Head);
    }

    #[test]
    fn occurrences_indexed_through_sub_sentences() {
        let mut store = SyntacticLayer::default();
        store.load_kif("(=> (P ?X) (Q ?X))", "t.kif");
        let p_occs = store.occurrences.get(&"P".to_string()).expect("P has occurrences");
        let q_occs = store.occurrences.get(&"Q".to_string()).expect("Q has occurrences");
        assert_eq!(p_occs.len(), 1);
        assert_eq!(q_occs.len(), 1);
        assert_eq!(p_occs.iter().next().unwrap().kind, OccurrenceKind::Head);
        assert_eq!(q_occs.iter().next().unwrap().kind, OccurrenceKind::Head);
    }

    #[test]
    fn cache_config_disables_occurrences() {
        use crate::cache::CacheConfig;
        let cfg = CacheConfig::default();
        cfg.disable(OccurrenceIndex::NAME);
        let mut store = SyntacticLayer::with_config(&cfg);
        store.load_kif("(subclass Human Animal)", "test");
        // Cache is disabled: occurrences stay empty even after indexing.
        assert!(store.occurrences.get(&"Human".to_string()).is_none());
    }
}
