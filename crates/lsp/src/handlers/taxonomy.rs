//! Custom LSP request `sumo/taxonomy`.
//!
//! Given a symbol name, returns the upward taxonomy graph (the symbol plus
//! every ancestor reachable via `subclass`, `instance`, `subrelation`, and
//! `subAttribute` edges), along with the symbol's documentation entries.
//! Traversal is upward-only (child -> parent).

use std::collections::{HashSet, VecDeque};

use lsp_types::request::Request;
use serde::{Deserialize, Serialize};
use sigmakee_rs_sdk::{DocBlock, DocSpan};

use crate::state::GlobalState;

/// LSP method name for the taxonomy request.
pub const METHOD: &str = "sumo/taxonomy";

/// Typed custom request for the `sumo/taxonomy` method.
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
/// When `unknown` is true the symbol is not in the active KB, and
/// `documentation` and `edges` are empty.
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
    /// is the child, `to` is the parent.
    pub edges:         Vec<TaxonomyEdgeDto>,
}

/// Serialisable documentation entry for the taxonomy DTO.  The `text`
/// field is free of `&%CrossRef` marker syntax (e.g. `"&%Animal"`
/// becomes `"Animal"`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocEntryDto {
    /// IETF-style language tag (e.g. `"EnglishLanguage"`).
    pub language: String,
    /// Plain-text rendering of the documentation block, with all
    /// `&%CrossRef` markers stripped.  Each cross-referenced
    /// symbol's bare name remains in the text where the marker
    /// used to be.
    pub text:     String,
}

/// One edge in the taxonomy graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaxonomyEdgeDto {
    /// The child symbol.
    pub from:     String,
    /// The parent symbol.
    pub to:       String,
    /// The KIF head that introduced the edge (`subclass` / `instance` /
    /// `subrelation` / `subAttribute`).
    pub relation: String,
}

// -- Handler ------------------------------------------------------------------

/// Upper bound on nodes visited during BFS, protecting the server from a
/// cycle-laden KB.
const MAX_NODES: usize = 2048;

/// Handle a `sumo/taxonomy` request.
///
/// Always returns a response; an unknown symbol is signalled via
/// `TaxonomyResponse.unknown = true` with empty payload fields.
pub fn handle_taxonomy(state: &GlobalState, params: TaxonomyParams) -> TaxonomyResponse {
    let root = params.symbol;

    let Ok(kb) = state.kb.read() else {
        log::warn!(target: "sumo_lsp::taxonomy", "kb lock poisoned");
        return TaxonomyResponse {
            symbol: root, unknown: true, ..Default::default()
        };
    };

    let Some(root_view) = sigmakee_rs_sdk::manpage_view(&kb, &root) else {
        return TaxonomyResponse {
            symbol: root, unknown: true, ..Default::default()
        };
    };

    let documentation: Vec<DocEntryDto> = root_view.documentation.iter()
        .map(|block| DocEntryDto {
            language: block.language.clone(),
            text:     flatten_doc_block(block),
        })
        .collect();

    // `visited` protects against multi-inheritance diamonds (a class that is
    // simultaneously a subclass and an instance).
    let mut edges:   Vec<TaxonomyEdgeDto> = Vec::new();
    let mut visited: HashSet<String>     = HashSet::new();
    let mut queue:   VecDeque<String>    = VecDeque::new();

    visited.insert(root.clone());

    push_parent_edges(&root, &root_view.parents, &mut edges, &mut visited, &mut queue);

    while let Some(current) = queue.pop_front() {
        if visited.len() >= MAX_NODES {
            log::warn!(target: "sumo_lsp::taxonomy",
                "taxonomy BFS truncated at {} nodes (root='{}')",
                MAX_NODES, root);
            break;
        }

        let Some(view) = sigmakee_rs_sdk::manpage_view(&kb, &current) else { continue; };
        push_parent_edges(&current, &view.parents, &mut edges, &mut visited, &mut queue);
    }

    TaxonomyResponse {
        symbol: root,
        unknown: false,
        documentation,
        edges,
    }
}

/// Flatten a [`DocBlock`]'s structured spans to plain text for the wire DTO.
///
/// `Text` spans pass through verbatim; `Link` spans emit just their visible
/// label (the symbol name, without the `&%` marker).
fn flatten_doc_block(block: &DocBlock) -> String {
    let mut out = String::new();
    for span in &block.spans {
        match span {
            DocSpan::Text(t)            => out.push_str(t),
            DocSpan::Link { text, .. }  => out.push_str(text),
        }
    }
    out
}

/// Append an edge per parent of `child` and enqueue newly-seen parents.
fn push_parent_edges(
    child:   &str,
    parents: &[sigmakee_rs_core::ParentEdge],
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
            let _ = kb.load(sigmakee_rs_core::SourceFile::kif(std::path::PathBuf::from("test.kif"), kb_text.to_string()), "test.kif");
            // Man-page introspection reads the `Base` scope; a freshly-loaded
            // file sits in its own session until promoted.
            let _ = kb.make_session_axiomatic("test.kif", None, None, None);
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
        // Dog is a subclass of Mammal *and* an instance of Species.
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

    #[test]
    fn cross_refs_in_documentation_are_stripped_to_bare_names() {
        // SDK's `manpage_view` resolves `&%Symbol` markers into
        // structured `DocSpan::Link` entries; `flatten_doc_block`
        // then renders each link as just its visible label.  The
        // wire DTO must therefore not contain any `&%` markers.
        let kb = r#"
            (subclass Human Hominid)
            (documentation Human EnglishLanguage
                "A member of the species &%HomoSapiens, distinguished from &%Plant.")
        "#;
        let state = load(kb);
        let resp  = handle_taxonomy(&state, TaxonomyParams {
            symbol: "Human".into(),
        });
        assert!(!resp.unknown);
        assert_eq!(resp.documentation.len(), 1);
        let doc = &resp.documentation[0];
        // The cross-referenced symbol names survive verbatim;
        // the marker prefix does not.
        assert!(doc.text.contains("HomoSapiens"),
            "cross-ref label missing from rendered doc: {}", doc.text);
        assert!(doc.text.contains("Plant"),
            "cross-ref label missing from rendered doc: {}", doc.text);
        assert!(!doc.text.contains("&%"),
            "raw cross-ref marker leaked into wire DTO: {}", doc.text);
        // Surrounding prose is preserved.
        assert!(doc.text.contains("species"));
        assert!(doc.text.contains("distinguished"));
    }
}
