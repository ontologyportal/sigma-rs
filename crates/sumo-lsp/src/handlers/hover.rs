// crates/sumo-lsp/src/handlers/hover.rs
//
// `textDocument/hover` handler.  Maps the cursor offset to a
// symbol via `KnowledgeBase::symbol_at_offset`, then renders the
// symbol's man-page view as CommonMark for the client.
//
// The markdown is deliberately compact: hover tooltips are small,
// so we surface the most useful fields (kind, direct parents, one
// documentation paragraph, signature summary) and leave everything
// else for the `sumo man` CLI or a future richer UI.
//
// # SDK migration
//
// Hover used to consume `sumo_kb::ManPage` directly and render its
// `documentation` text verbatim — `&%Symbol` cross-refs would land
// in the rendered markdown as raw `&%Symbol` text.  Now we go
// through [`sumo_sdk::manpage_view`], which gives back a
// [`sumo_sdk::ManPageView`] whose doc/term-format/format blocks are
// pre-segmented into [`sumo_sdk::DocSpan::Text`] and
// [`sumo_sdk::DocSpan::Link { text, target }`].  We render the Text
// spans verbatim and bold the Link spans so the hover tooltip
// visually distinguishes referenced symbols.  If the cross-ref
// marker syntax ever changes (`&%X` → `[[X]]`, say), only the
// SDK changes; this handler keeps working.

use lsp_types::{Hover, HoverContents, HoverParams, MarkupContent, MarkupKind};

use sumo_sdk::{DocBlock, DocSpan, ManPageView};

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
    let view     = sumo_sdk::manpage_view(&kb, &sym_name)?;

    let markdown = render_manpage_markdown(&view);

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind:  MarkupKind::Markdown,
            value: markdown,
        }),
        range: sym_span.as_ref().map(|s| span_to_range(&doc.rope, s)),
    })
}

// -- Markdown rendering -------------------------------------------------------

fn render_manpage_markdown(view: &ManPageView) -> String {
    let mut out = String::new();

    // Heading: name + kind badges.
    out.push_str("### ");
    out.push_str(&view.name);
    out.push('\n');
    if !view.kinds.is_empty() {
        out.push('`');
        for (i, k) in view.kinds.iter().enumerate() {
            if i > 0 { out.push_str(" · "); }
            out.push_str(k.as_str());
        }
        out.push('`');
        out.push('\n');
    }
    out.push('\n');

    // Parents (taxonomic).
    if !view.parents.is_empty() {
        out.push_str("**Parents**\n\n");
        for p in &view.parents {
            out.push_str(&format!("- `{}` → `{}`\n", p.relation, p.parent));
        }
        out.push('\n');
    }

    // Signature: arity / domains / range.
    let sig = &view.signature;
    let has_sig = sig.arity.is_some() || !sig.domains.is_empty() || sig.range.is_some();
    if has_sig {
        out.push_str("**Signature**\n\n");
        if let Some(a) = sig.arity {
            let rendered = if a < 0 { "variable".to_string() } else { a.to_string() };
            out.push_str(&format!("- arity: {}\n", rendered));
        }
        for (pos, s) in &sig.domains {
            let suffix = if s.subclass { " *(subclass-of)*" } else { "" };
            out.push_str(&format!("- arg {}: `{}`{}\n", pos, s.class, suffix));
        }
        if let Some(s) = &sig.range {
            let suffix = if s.subclass { " *(subclass-of)*" } else { "" };
            out.push_str(&format!("- range: `{}`{}\n", s.class, suffix));
        }
        out.push('\n');
    }

    // Documentation (first paragraph per language).  Cross-refs
    // are pre-resolved via the SDK; render Link spans as **Symbol**
    // so hover-pane consumers can visually distinguish them from
    // surrounding prose.
    if !view.documentation.is_empty() {
        out.push_str("**Documentation**\n\n");
        for d in &view.documentation {
            out.push_str(&format!("*{}*\n\n{}\n\n",
                d.language, render_spans(d).trim()));
        }
    }

    // termFormat -- one line per language.
    if !view.term_format.is_empty() {
        out.push_str("**Term format**\n\n");
        for t in &view.term_format {
            out.push_str(&format!("- `{}`: {}\n", t.language, render_spans(t)));
        }
        out.push('\n');
    }

    // format relation strings -- for predicates these template the
    // sentence surface form.
    if !view.format.is_empty() {
        out.push_str("**Format**\n\n");
        for f in &view.format {
            out.push_str(&format!("- `{}`: {}\n", f.language, render_spans(f)));
        }
        out.push('\n');
    }

    out
}

/// Render a [`DocBlock`]'s spans as inline markdown.  Plain text
/// passes through verbatim; cross-ref links render as `**Symbol**`
/// so they stand out in a hover tooltip without depending on a
/// markdown link target the editor may not be able to resolve.
fn render_spans(block: &DocBlock) -> String {
    let mut out = String::new();
    for span in &block.spans {
        match span {
            DocSpan::Text(t)            => out.push_str(t),
            DocSpan::Link { target, .. } => {
                out.push_str("**");
                out.push_str(target);
                out.push_str("**");
            }
        }
    }
    out
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use sumo_kb::{DocEntry, ManKind, ManPage, ParentEdge, SortSig};
    use sumo_sdk::view_from_manpage;

    fn fixture_view() -> ManPageView {
        view_from_manpage(ManPage {
            name:          "Human".to_string(),
            kinds:         vec![ManKind::Instance],
            documentation: vec![DocEntry {
                language: "EnglishLanguage".to_string(),
                text:     "A member of the species &%HomoSapiens.".to_string(),
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
            ref_nested: Vec::new(),
        })
    }

    #[test]
    fn rendered_markdown_contains_all_sections() {
        let md = render_manpage_markdown(&fixture_view());
        assert!(md.contains("### Human"));
        assert!(md.contains("`instance`"));
        assert!(md.contains("Parents"));
        assert!(md.contains("`subclass` → `Hominid`"));
        assert!(md.contains("Documentation"));
        assert!(md.contains("species"));
        assert!(md.contains("Term format"));
        assert!(md.contains("human"));
    }

    #[test]
    fn cross_refs_in_doc_render_as_bold() {
        // The fixture's doc text is "A member of the species &%HomoSapiens."
        // SDK's `manpage_view` resolves that &% marker into a
        // `DocSpan::Link`, and we render it as **HomoSapiens** —
        // raw `&%` markers must NOT appear in the output.
        let md = render_manpage_markdown(&fixture_view());
        assert!(md.contains("**HomoSapiens**"),
            "expected bold cross-ref, markdown was:\n{md}");
        assert!(!md.contains("&%HomoSapiens"),
            "raw cross-ref marker leaked into markdown:\n{md}");
    }

    #[test]
    fn signature_section_renders_arity_and_domains() {
        let view = view_from_manpage(ManPage {
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
            ref_nested: Vec::new(),
        });
        let md = render_manpage_markdown(&view);
        assert!(md.contains("Signature"));
        assert!(md.contains("arity: 2"));
        assert!(md.contains("arg 1"));
        assert!(md.contains("subclass-of"));
        assert!(md.contains("Format"));
        assert!(md.contains("%1 is a subclass of %2"));
    }

    #[test]
    fn empty_manpage_renders_minimally() {
        let view = view_from_manpage(ManPage {
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
            ref_nested: Vec::new(),
        });
        let md = render_manpage_markdown(&view);
        assert!(md.contains("### X"));
        assert!(md.contains("individual"));
        assert!(!md.contains("Parents"));
        assert!(!md.contains("Documentation"));
    }
}
