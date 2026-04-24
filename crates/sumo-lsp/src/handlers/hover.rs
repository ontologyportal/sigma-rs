// crates/sumo-lsp/src/handlers/hover.rs
//
// `textDocument/hover` handler.  Maps the cursor offset to a
// symbol via `KnowledgeBase::symbol_at_offset`, then renders the
// symbol's `ManPage` as CommonMark for the client.
//
// The markdown is deliberately compact: hover tooltips are small,
// so we surface the most useful fields (kind, direct parents, one
// documentation paragraph, signature summary) and leave everything
// else for the `sumo man` CLI or a future richer UI.

use lsp_types::{Hover, HoverContents, HoverParams, MarkupContent, MarkupKind};

use sumo_kb::ManPage;

use crate::conv::{position_to_offset, span_to_range, uri_to_tag};
use crate::state::GlobalState;

/// Handle a `textDocument/hover` request.  Returns `None` when
/// the cursor isn't on a recognisable symbol or when the document
/// hasn't been opened in the server yet.
pub fn handle_hover(state: &GlobalState, params: HoverParams) -> Option<Hover> {
    let uri      = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;

    let docs = state.docs.read().ok()?;
    let doc  = docs.get(&uri)?;
    let offset = position_to_offset(&doc.rope, position);
    let tag    = uri_to_tag(&uri);

    let kb = state.kb.read().ok()?;
    let sym_name = kb.symbol_at_offset(&tag, offset)?;
    let sym_span = kb.element_at_offset(&tag, offset).map(|h| h.span);
    let man      = kb.manpage(&sym_name)?;

    let markdown = render_manpage_markdown(&man);

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind:  MarkupKind::Markdown,
            value: markdown,
        }),
        range: sym_span.as_ref().map(|s| span_to_range(&doc.rope, s)),
    })
}

// -- Markdown rendering -------------------------------------------------------

fn render_manpage_markdown(man: &ManPage) -> String {
    let mut out = String::new();

    // Heading: name + kind badges.
    out.push_str("### ");
    out.push_str(&man.name);
    out.push('\n');
    if !man.kinds.is_empty() {
        out.push('`');
        for (i, k) in man.kinds.iter().enumerate() {
            if i > 0 { out.push_str(" · "); }
            out.push_str(k.as_str());
        }
        out.push('`');
        out.push('\n');
    }
    out.push('\n');

    // Parents (taxonomic).
    if !man.parents.is_empty() {
        out.push_str("**Parents**\n\n");
        for p in &man.parents {
            out.push_str(&format!("- `{}` → `{}`\n", p.relation, p.parent));
        }
        out.push('\n');
    }

    // Signature: arity / domains / range.
    let has_sig = man.arity.is_some() || !man.domains.is_empty() || man.range.is_some();
    if has_sig {
        out.push_str("**Signature**\n\n");
        if let Some(a) = man.arity {
            let rendered = if a < 0 { "variable".to_string() } else { a.to_string() };
            out.push_str(&format!("- arity: {}\n", rendered));
        }
        for (pos, sig) in &man.domains {
            let suffix = if sig.subclass { " *(subclass-of)*" } else { "" };
            out.push_str(&format!("- arg {}: `{}`{}\n", pos, sig.class, suffix));
        }
        if let Some(sig) = &man.range {
            let suffix = if sig.subclass { " *(subclass-of)*" } else { "" };
            out.push_str(&format!("- range: `{}`{}\n", sig.class, suffix));
        }
        out.push('\n');
    }

    // Documentation (first paragraph per language).
    if !man.documentation.is_empty() {
        out.push_str("**Documentation**\n\n");
        for d in &man.documentation {
            out.push_str(&format!("*{}*\n\n{}\n\n", d.language, d.text.trim()));
        }
    }

    // termFormat -- one line per language.
    if !man.term_format.is_empty() {
        out.push_str("**Term format**\n\n");
        for t in &man.term_format {
            out.push_str(&format!("- `{}`: {}\n", t.language, t.text));
        }
        out.push('\n');
    }

    // format relation strings -- for predicates these template the
    // sentence surface form.
    if !man.format.is_empty() {
        out.push_str("**Format**\n\n");
        for f in &man.format {
            out.push_str(&format!("- `{}`: {}\n", f.language, f.text));
        }
        out.push('\n');
    }

    out
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use sumo_kb::{DocEntry, ManKind, ManPage, ParentEdge, SortSig};

    fn fixture_page() -> ManPage {
        ManPage {
            name:          "Human".to_string(),
            kinds:         vec![ManKind::Instance],
            documentation: vec![DocEntry {
                language: "EnglishLanguage".to_string(),
                text:     "A member of the species homo sapiens.".to_string(),
            }],
            term_format:   vec![DocEntry {
                language: "EnglishLanguage".to_string(),
                text:     "human".to_string(),
            }],
            format:        vec![],
            parents:       vec![ParentEdge {
                relation: "subclass".to_string(),
                parent:   "Hominid".to_string(),
            }],
            arity:   None,
            domains: vec![],
            range:   None,
            ref_args: Vec::new(),
            ref_nested: Vec::new()
        }
    }

    #[test]
    fn rendered_markdown_contains_all_sections() {
        let md = render_manpage_markdown(&fixture_page());
        assert!(md.contains("### Human"));
        assert!(md.contains("`instance`"));
        assert!(md.contains("Parents"));
        assert!(md.contains("`subclass` → `Hominid`"));
        assert!(md.contains("Documentation"));
        assert!(md.contains("species homo sapiens"));
        assert!(md.contains("Term format"));
        assert!(md.contains("human"));
    }

    #[test]
    fn signature_section_renders_arity_and_domains() {
        let page = ManPage {
            name:          "subclass".to_string(),
            kinds:         vec![ManKind::Predicate],
            documentation: vec![],
            term_format:   vec![],
            format:        vec![DocEntry {
                language: "EnglishLanguage".to_string(),
                text:     "%1 is a subclass of %2".to_string(),
            }],
            parents:       vec![],
            arity:         Some(2),
            domains:       vec![
                (1, SortSig { class: "Class".into(), subclass: true }),
                (2, SortSig { class: "Class".into(), subclass: true }),
            ],
            range:         None,
            ref_args: Vec::new(),
            ref_nested: Vec::new()
        };
        let md = render_manpage_markdown(&page);
        assert!(md.contains("Signature"));
        assert!(md.contains("arity: 2"));
        assert!(md.contains("arg 1"));
        assert!(md.contains("subclass-of"));
        assert!(md.contains("Format"));
        assert!(md.contains("%1 is a subclass of %2"));
    }

    #[test]
    fn empty_manpage_renders_minimally() {
        let page = ManPage {
            name:          "X".to_string(),
            kinds:         vec![ManKind::Individual],
            documentation: vec![],
            term_format:   vec![],
            format:        vec![],
            parents:       vec![],
            arity:         None,
            domains:       vec![],
            range:         None,
            ref_args: Vec::new(),
            ref_nested: Vec::new()
        };
        let md = render_manpage_markdown(&page);
        assert!(md.contains("### X"));
        assert!(md.contains("individual"));
        assert!(!md.contains("Parents"));
        assert!(!md.contains("Documentation"));
    }

}
