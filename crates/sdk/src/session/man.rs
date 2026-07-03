//! Man-page view: a structured projection of [`sigmakee_rs_core::ManPage`]
//! with cross-references already resolved into typed link spans.
//!
//! The KIF documentation convention embeds cross-refs as the byte
//! sequence `&%Symbol` inside otherwise free-form documentation /
//! termFormat / format strings.  Consumers (TUIs, IDE hover panels,
//! the LSP) historically had to know that syntax to render
//! click-able links.  This module is the **single place** that
//! syntax is parsed: callers just consume [`DocSpan::Link`].
//!
//! The session-API entry point is [`Session::manpage`]; when the
//! marker syntax changes (say from `&%X` to `[[X]]`), only
//! [`parse_doc_spans`] needs to change — every consumer continues to
//! receive the same structured `Vec<DocSpan>`.
//!
//! # Example
//!
//! ```no_run
//! use sigmakee_rs_sdk::{Session, DocSpan};
//! # #[cfg(feature = "native-prover")] {
//! use sigmakee_rs_core::ProverLayer;
//!
//! let s = Session::<ProverLayer>::new("demo".into());
//! if let Some(view) = s.manpage("Animal") {
//!     for block in &view.documentation {
//!         for span in &block.spans {
//!             match span {
//!                 DocSpan::Text(s)               => print!("{}", s),
//!                 DocSpan::Link { text, target } => print!("[{} -> {}]", text, target),
//!             }
//!         }
//!         println!();
//!     }
//! }
//! # }
//! ```

use sigmakee_rs_core::{ManKind, ManPage, ParentEdge, SearchHit, SearchOpts, SentenceId, SortSig, TopLayer};

use crate::SdkResult;

use super::Session;
// Note: `SentenceRef` (the (position, sid) pair stored in
// `ManPage::ref_args`) is `pub` but its module is `pub(crate)` in
// sigmakee-rs-core, so we can't name it here.  We never need to — access goes
// through tuple field access (`.0`, `.1`) inside the converter below.

/// One chunk of a documentation / termFormat / format string.  Either
/// literal text or a resolved cross-reference link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocSpan {
    /// Plain text run.  May contain any non-marker characters,
    /// including whitespace and punctuation.
    Text(String),
    /// A cross-reference to another symbol.
    ///
    /// `text` is the visible label (the symbol name with any marker
    /// syntax stripped — `&%Animal` becomes `text: "Animal"`).
    /// `target` is what to look up if the user activates the link;
    /// for `&%X` the two are identical, but the field separation
    /// keeps room for richer link forms (e.g. aliased text) later.
    Link {
        /// Visible label as it should be displayed to the reader.
        text: String,
        /// Symbol name to navigate to when this link is followed.
        target: String,
    },
}

/// One language-tagged documentation entry, with cross-refs resolved.
#[derive(Debug, Clone)]
pub struct DocBlock {
    /// IETF-style language tag (e.g. `"EnglishLanguage"`) from the
    /// underlying KIF `(documentation X <language> "...")` form.
    pub language: String,
    /// Pre-segmented spans.  Concatenating each `Text` and each
    /// `Link.text` reproduces the original text minus the marker
    /// prefix (e.g. `&%`) — never *more* than the original.
    pub spans: Vec<DocSpan>,
}

/// Signature view: arity + per-position domains + range, in the order
/// the underlying `ManPage` exposes them.
#[derive(Debug, Clone, Default)]
pub struct SignatureView {
    /// Declared arity.  `None` if unknown; `Some(-1)` for variable-arity
    /// relations.
    pub arity:   Option<i32>,
    /// Positional domain declarations, indexed by 1-based argument
    /// position.  Arguments without an explicit declaration are elided.
    pub domains: Vec<(usize, SortSig)>,
    /// Declared range (functions and relations that declare one).
    pub range:   Option<SortSig>,
}

/// Sentences where the symbol appears as an axiom argument or head.
#[derive(Debug, Clone, Default)]
pub struct ReferenceSet {
    /// Sentences indexed by the symbol's first root-level position.
    /// Index `0` is "appears as the head"; index `n >= 1` is
    /// "appears as argument number `n`".  Inner vecs are NOT sorted —
    /// preserve the underlying KB order so consumers that want stable
    /// output can sort by their own criterion (e.g. file:line).
    pub by_position: Vec<Vec<SentenceId>>,
    /// Sentences where the symbol only appears nested inside a
    /// sub-sentence.
    pub nested: Vec<SentenceId>,
}

/// Structured man-page view.  See [`Session::manpage`] for construction.
#[derive(Debug, Clone)]
pub struct ManPageView {
    /// Symbol name this view describes.
    pub name:          String,
    /// All categories the symbol belongs to (class, relation, function, …).
    pub kinds:         Vec<ManKind>,
    /// Taxonomic parents (`(subclass X P)` / `(instance X P)` / …).
    pub parents:       Vec<ParentEdge>,
    /// Arity / domain / range declarations.
    pub signature:     SignatureView,
    /// `(documentation X <lang> "...")` blocks, cross-refs resolved.
    pub documentation: Vec<DocBlock>,
    /// `(termFormat <lang> X "...")` blocks, cross-refs resolved.
    pub term_format:   Vec<DocBlock>,
    /// `(format <lang> X "...")` blocks (relations only).
    pub format:        Vec<DocBlock>,
    /// Sentences referencing this symbol, bucketed by where it appears.
    pub references:    ReferenceSet,
}

impl ManPageView {
    /// Iterate every link target the page contains, in the order
    /// they'd appear in a top-to-bottom render: parents first, then
    /// documentation cross-refs, then term-format, then format.  A
    /// TUI that wants to expose Tab-cycling can use this to populate
    /// its link list without parsing anything itself.
    pub fn link_targets(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for p in &self.parents {
            out.push(p.parent.as_str());
        }
        for blocks in [&self.documentation, &self.term_format, &self.format] {
            for block in blocks {
                for span in &block.spans {
                    if let DocSpan::Link { target, .. } = span {
                        out.push(target.as_str());
                    }
                }
            }
        }
        out
    }
}

impl<L: TopLayer> Session<L> {
    /// Build a structured man-page [`ManPageView`] for `symbol`, or `None` if
    /// the symbol isn't interned in this session's KB.  Cross-references in the
    /// documentation strings are resolved into [`DocSpan::Link`] entries —
    /// consumers never see raw `&%Symbol` marker syntax.
    ///
    /// Available on every backend (the man page reads layer-agnostic semantic
    /// data).  This is the session-API replacement for the former free
    /// `manpage_view(&kb, symbol)` helper, and mirrors the core
    /// [`KnowledgeBase::manpage`](sigmakee_rs_core::KnowledgeBase) it wraps.
    pub fn manpage(&self, symbol: &str) -> Option<ManPageView> {
        self.kb.manpage(symbol).map(view_from_manpage)
    }

    /// Search for a symbol by a broad query.
    /// 
    /// This will find symbols whose name or documentation entries somehow
    /// match the query
    pub fn search(&self, query: &str, opts: &SearchOpts) -> SdkResult<Vec<SearchHit>> {
        Ok(self.kb.search(query, opts))
    }
}

/// Convert an already-fetched [`ManPage`] into the structured view.  Useful when
/// the consumer has its own batch-fetch path; otherwise prefer
/// [`Session::manpage`].
pub fn view_from_manpage(raw: ManPage) -> ManPageView {
    let signature = SignatureView {
        arity:   raw.arity,
        domains: raw.domains,
        range:   raw.range,
    };

    let documentation = raw.documentation.into_iter()
        .map(|d| DocBlock { language: d.language, spans: parse_doc_spans(&d.text) })
        .collect();
    let term_format = raw.term_format.into_iter()
        .map(|d| DocBlock { language: d.language, spans: parse_doc_spans(&d.text) })
        .collect();
    let format = raw.format.into_iter()
        .map(|d| DocBlock { language: d.language, spans: parse_doc_spans(&d.text) })
        .collect();

    // Bucket SentenceRefs by their root-level position.  Position 0
    // is "head"; position N >= 1 is "argument N".  The vector is
    // sized from the data so variadic relations don't overflow a
    // fixed-size bucket array.  We access the tuple fields via `.0`
    // / `.1` so we never need to name the (pub-but-pub(crate)-module)
    // `SentenceRef` type.
    let references = if raw.ref_args.is_empty() && raw.ref_nested.is_empty() {
        ReferenceSet::default()
    } else {
        let max_pos = raw.ref_args.iter().map(|r| r.0).max().unwrap_or(0);
        let mut by_position: Vec<Vec<SentenceId>> = vec![Vec::new(); max_pos + 1];
        for r in raw.ref_args {
            by_position[r.0].push(r.1);
        }
        ReferenceSet { by_position, nested: raw.ref_nested }
    };

    ManPageView {
        name: raw.name,
        kinds: raw.kinds,
        parents: raw.parents,
        signature,
        documentation,
        term_format,
        format,
        references,
    }
}

// ---------------------------------------------------------------------------
// Cross-reference parser
// ---------------------------------------------------------------------------

/// Parse a documentation / termFormat / format string into
/// [`DocSpan`]s.  Every `&%Symbol` token becomes a
/// [`DocSpan::Link`]; everything else accumulates into
/// [`DocSpan::Text`] runs.
///
/// **This is the single place the marker syntax is recognised.**  If
/// the convention ever changes (`&%X` → `[[X]]`, say), update here
/// and every consumer continues to receive the same structured spans.
///
/// The visible `text` of each link is the symbol name with the marker
/// stripped — for `&%Animal` that's `"Animal"`.  Identifier characters
/// are ASCII alphanumerics plus `_`; the first non-identifier byte
/// terminates the symbol (so `&%dog's tail` parses as a link to `dog`
/// followed by `'s tail`).
pub fn parse_doc_spans(text: &str) -> Vec<DocSpan> {
    let mut out: Vec<DocSpan> = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0usize;
    let mut i     = 0usize;

    while i < bytes.len() {
        if i + 2 < bytes.len() && bytes[i] == b'&' && bytes[i + 1] == b'%' {
            let sym_start = i + 2;
            let mut sym_end = sym_start;
            while sym_end < bytes.len() {
                let c = bytes[sym_end];
                if c.is_ascii_alphanumeric() || c == b'_' { sym_end += 1; } else { break; }
            }
            if sym_end > sym_start {
                if i > start {
                    out.push(DocSpan::Text(text[start..i].to_string()));
                }
                let label: String = text[sym_start..sym_end].to_string();
                out.push(DocSpan::Link {
                    text:   label.clone(),
                    target: label,
                });
                i = sym_end;
                start = sym_end;
                continue;
            }
        }
        i += 1;
    }
    if start < bytes.len() {
        out.push(DocSpan::Text(text[start..].to_string()));
    }
    out
}

// ---------------------------------------------------------------------------
// Tests for the parser (KB-free)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_extracts_marker() {
        let s = parse_doc_spans("see &%Animal and &%Plant_Tissue.");
        assert_eq!(s.len(), 5);
        assert!(matches!(&s[1], DocSpan::Link { text, target }
            if text == "Animal" && target == "Animal"));
        assert!(matches!(&s[3], DocSpan::Link { text, target }
            if text == "Plant_Tissue" && target == "Plant_Tissue"));
    }

    #[test]
    fn parser_apostrophe_terminates() {
        let s = parse_doc_spans("&%dog's tail");
        assert_eq!(s.len(), 2);
        assert!(matches!(&s[0], DocSpan::Link { target, .. } if target == "dog"));
        assert!(matches!(&s[1], DocSpan::Text(t) if t == "'s tail"));
    }

    #[test]
    fn parser_lone_marker_is_text() {
        let s = parse_doc_spans("&% bare");
        assert_eq!(s.len(), 1);
        assert!(matches!(&s[0], DocSpan::Text(t) if t == "&% bare"));
    }

    #[test]
    fn parser_no_marker_returns_text_run() {
        let s = parse_doc_spans("plain text");
        assert_eq!(s.len(), 1);
        assert!(matches!(&s[0], DocSpan::Text(t) if t == "plain text"));
    }

    #[test]
    fn parser_marker_at_start() {
        let s = parse_doc_spans("&%X is a thing");
        assert_eq!(s.len(), 2);
        assert!(matches!(&s[0], DocSpan::Link { target, .. } if target == "X"));
        assert!(matches!(&s[1], DocSpan::Text(t) if t == " is a thing"));
    }
}

// End-to-end through a *native* (ProverLayer) session — proves the man page is
// genuinely layer-agnostic (the generic `KnowledgeBase::manpage`), not just
// available on the translation layer.
#[cfg(all(test, feature = "native-prover"))]
mod session_tests {
    use super::DocSpan;
    use crate::{Session, Source};
    use sigmakee_rs_core::ProverLayer;

    #[test]
    fn manpage_projects_a_view_and_resolves_cross_refs() {
        let mut s = Session::<ProverLayer>::new("man".into());
        let _ = s.ingest(Source::Reader {
            name: "d.kif".into(),
            reader: Box::new(std::io::Cursor::new(
                br#"(documentation Dog EnglishLanguage "A &%Mammal that barks.")"#.to_vec())),
        }, true);

        let view = s.manpage("Dog").expect("Dog has a man page");
        assert_eq!(view.name, "Dog");
        let linked_mammal = view.documentation.iter()
            .flat_map(|b| &b.spans)
            .any(|sp| matches!(sp, DocSpan::Link { target, .. } if target == "Mammal"));
        assert!(linked_mammal, "the `&%Mammal` cross-ref resolves to a DocSpan::Link");
    }
}
