// crates/sumo-lsp/src/handlers/taxonomy.rs
//
// Custom LSP request: `sumo/taxonomy`.
//
// Given a symbol name, returns the upward taxonomy graph (the
// symbol plus every ancestor reachable via `subclass`,
// `instance`, `subrelation`, and `subAttribute` edges), along
// with the symbol's documentation entries.  The client renders
// this as a Mermaid graph in a webview.
//
// The server does the BFS once and ships the whole graph in one
// response -- cheaper than N round-trips for a deep ancestry,
// and lets the client stay stateless with respect to the KB.
//
// Traversal is upward-only (child -> parent).  A downward pass
// (children, grandchildren, ...) would be trivially symmetrical,
// but SUMO taxonomies fan out enormously downward, so surfacing
// that by default would drown the diagram.  We can always add a
// `direction: "up" | "down" | "both"` parameter later if a client
// wants to opt in.

use std::collections::{HashSet, VecDeque};

use lsp_types::request::Request;
use serde::{Deserialize, Serialize};

use crate::state::GlobalState;

/// LSP method name for the taxonomy request.
pub const METHOD: &str = "sumo/taxonomy";

/// Typed custom request.  Implementing `lsp_types::request::Request`
/// lets us route through the existing `dispatch` helper in
/// `server.rs` instead of hand-rolling deserialisation.
pub enum TaxonomyRequest {}

impl Request for TaxonomyRequest {
    type Params = TaxonomyParams;
    type Result = TaxonomyResponse;
    const METHOD: &'static str = METHOD;
}

/// Request payload: the symbol to root the taxonomy at.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaxonomyParams {
    /// Symbol name to root the taxonomy at.
    pub symbol: String,
}

/// Response payload: the root symbol's documentation plus every
/// taxonomy edge reachable upward from it.
///
/// `unknown: true` signals "the symbol is not in the active KB";
/// in that case `documentation` and `edges` are empty and the
/// client renders an error panel instead of a graph.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaxonomyResponse {
    /// Echo of the root symbol name.
    pub symbol:        String,
    /// True iff the symbol is not interned in the KB.
    pub unknown:       bool,
    /// Documentation entries for the root symbol, one per language.
    pub documentation: Vec<DocEntryDto>,
    /// Every edge discovered by upward BFS from the root.  `from`
    /// is the child, `to` is the parent -- an arrow in a top-down
    /// graph points `from -> to`.
    pub edges:         Vec<TaxonomyEdgeDto>,
}

/// Serialisable mirror of `sumo_kb::DocEntry`.  Mirrored (not
/// re-exported) so the LSP wire format is decoupled from the KB's
/// internal types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocEntryDto {
    pub language: String,
    pub text:     String,
}

/// One edge in the taxonomy graph.  `relation` is the KIF head
/// that introduced it (`subclass` / `instance` / `subrelation` /
/// `subAttribute`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaxonomyEdgeDto {
    pub from:     String,
    pub to:       String,
    pub relation: String,
}

// -- Handler ------------------------------------------------------------------

/// Upper bound on nodes visited during BFS.  A healthy SUMO
/// ancestry is ~12 deep with small fan-out; two thousand is
/// comfortably beyond that and still protects the server from a
/// pathological cycle-laden KB.
const MAX_NODES: usize = 2048;

/// Handle a `sumo/taxonomy` request.
///
/// Always returns a response (never `None` at the dispatch layer);
/// an unknown symbol is signalled via `TaxonomyResponse.unknown = true`
/// with empty payload fields.
pub fn handle_taxonomy(state: &GlobalState, params: TaxonomyParams) -> TaxonomyResponse {
    let root = params.symbol;

    let Ok(kb) = state.kb.read() else {
        log::warn!(target: "sumo_lsp::taxonomy", "kb lock poisoned");
        return TaxonomyResponse {
            symbol: root, unknown: true, ..Default::default()
        };
    };

    // Root lookup also validates existence; if the symbol isn't in
    // the KB we bail without a BFS.
    let Some(root_page) = kb.manpage(&root) else {
        return TaxonomyResponse {
            symbol: root, unknown: true, ..Default::default()
        };
    };

    let documentation: Vec<DocEntryDto> = root_page.documentation.iter()
        .map(|d| DocEntryDto { language: d.language.clone(), text: d.text.clone() })
        .collect();

    // Upward BFS.  `visited` protects against the multi-inheritance
    // diamonds that crop up when a class is simultaneously a
    // subclass and an instance (SUMO has several such cases).
    let mut edges:   Vec<TaxonomyEdgeDto> = Vec::new();
    let mut visited: HashSet<String>     = HashSet::new();
    let mut queue:   VecDeque<String>    = VecDeque::new();

    visited.insert(root.clone());

    // Seed the BFS with the root's parents (taken from the ManPage
    // we already fetched above, so we save a re-query).  Subsequent
    // layers are expanded inside the loop.
    push_parent_edges(&root, &root_page.parents, &mut edges, &mut visited, &mut queue);

    while let Some(current) = queue.pop_front() {
        if visited.len() >= MAX_NODES {
            log::warn!(target: "sumo_lsp::taxonomy",
                "taxonomy BFS truncated at {} nodes (root='{}')",
                MAX_NODES, root);
            break;
        }

        let Some(page) = kb.manpage(&current) else { continue; };
        push_parent_edges(&current, &page.parents, &mut edges, &mut visited, &mut queue);
    }

    TaxonomyResponse {
        symbol: root,
        unknown: false,
        documentation,
        edges,
    }
}

/// Append an edge per parent of `child` and enqueue newly-seen
/// parents.  Factored out because the root is seeded before the
/// BFS loop starts.
fn push_parent_edges(
    child:   &str,
    parents: &[sumo_kb::ParentEdge],
    edges:   &mut Vec<TaxonomyEdgeDto>,
    visited: &mut HashSet<String>,
    queue:   &mut VecDeque<String>,
) {
    for p in parents {
        edges.push(TaxonomyEdgeDto {
            from:     child.to_string(),
            to:       p.parent.clone(),
            relation: p.relation.clone(),
        });
        if visited.insert(p.parent.clone()) {
            queue.push_back(p.parent.clone());
        }
    }
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::GlobalState;

    fn load(kb_text: &str) -> GlobalState {
        let state = GlobalState::new();
        {
            let mut kb = state.kb.write().expect("kb not poisoned");
            let _ = kb.load_kif(kb_text, "test.kif", None);
        }
        state
    }

    #[test]
    fn unknown_symbol_flags_response() {
        let state = load("");
        let resp  = handle_taxonomy(&state, TaxonomyParams {
            symbol: "DoesNotExist".to_string(),
        });
        assert!(resp.unknown);
        assert!(resp.edges.is_empty());
        assert!(resp.documentation.is_empty());
        assert_eq!(resp.symbol, "DoesNotExist");
    }

    #[test]
    fn linear_chain_surfaces_all_ancestors() {
        let kb = r#"
            (subclass Human Hominid)
            (subclass Hominid Primate)
            (subclass Primate Mammal)
            (subclass Mammal Animal)
        "#;
        let state = load(kb);
        let resp  = handle_taxonomy(&state, TaxonomyParams {
            symbol: "Human".into(),
        });
        assert!(!resp.unknown);

        let pairs: Vec<(String, String, String)> = resp.edges.iter()
            .map(|e| (e.from.clone(), e.to.clone(), e.relation.clone()))
            .collect();
        assert!(pairs.contains(&("Human".into(),   "Hominid".into(),  "subclass".into())));
        assert!(pairs.contains(&("Hominid".into(), "Primate".into(),  "subclass".into())));
        assert!(pairs.contains(&("Primate".into(), "Mammal".into(),   "subclass".into())));
        assert!(pairs.contains(&("Mammal".into(),  "Animal".into(),   "subclass".into())));
    }

    #[test]
    fn multi_inheritance_does_not_duplicate_nodes() {
        // Dog is a subclass of Mammal *and* an instance of Species
        // (contrived but exercises the visited-set).
        let kb = r#"
            (subclass Dog    Mammal)
            (instance Dog    Species)
            (subclass Mammal Animal)
            (subclass Species Class)
        "#;
        let state = load(kb);
        let resp  = handle_taxonomy(&state, TaxonomyParams {
            symbol: "Dog".into(),
        });
        assert!(!resp.unknown);

        // Two outgoing edges from Dog.
        let from_dog: Vec<&TaxonomyEdgeDto> = resp.edges.iter()
            .filter(|e| e.from == "Dog").collect();
        assert_eq!(from_dog.len(), 2);

        // Each parent is visited at most once.
        let parent_visits: std::collections::HashMap<String, usize> =
            resp.edges.iter().fold(Default::default(), |mut acc, e| {
                *acc.entry(e.from.clone()).or_insert(0) += 1; acc
            });
        assert_eq!(parent_visits.get("Mammal").copied().unwrap_or(0), 1);
        assert_eq!(parent_visits.get("Species").copied().unwrap_or(0), 1);
    }

    #[test]
    fn documentation_is_surfaced_for_the_root_only() {
        let kb = r#"
            (subclass Human Hominid)
            (documentation Human  EnglishLanguage "A species of hominid.")
            (documentation Human  FrenchLanguage  "Une espece d'hominide.")
            (documentation Hominid EnglishLanguage "A family of primates.")
        "#;
        let state = load(kb);
        let resp  = handle_taxonomy(&state, TaxonomyParams {
            symbol: "Human".into(),
        });
        assert_eq!(resp.documentation.len(), 2);
        assert!(resp.documentation.iter().any(|d| d.language == "EnglishLanguage"));
        assert!(resp.documentation.iter().any(|d| d.language == "FrenchLanguage"));
        // No Hominid doc leaked into the root's entry.
        assert!(resp.documentation.iter().all(|d| !d.text.contains("family")));
    }

    #[test]
    fn cycle_does_not_loop_forever() {
        // Illegal but defensive: if the KB somehow ends up with a
        // cycle, the BFS must still terminate.
        let kb = r#"
            (subclass A B)
            (subclass B A)
        "#;
        let state = load(kb);
        let resp  = handle_taxonomy(&state, TaxonomyParams {
            symbol: "A".into(),
        });
        assert!(!resp.unknown);
        // Both edges present, no infinite recursion.
        assert_eq!(resp.edges.len(), 2);
    }
}
