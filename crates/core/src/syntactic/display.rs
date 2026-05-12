// crates/core/src/syntactic/display.rs
//
// Rendering for SyntacticLayer-owned sentences.  Two distinct views:
//
//   * **normalized** — the interned `Sentence` structure, rendered as KIF.
//     This is the canonical form the KB actually reasons over.  It carries *no*
//     source information (the build dropped spans and original surface syntax),
//     so it is purely structural.  `SentenceDisplay` (ANSI) and
//     `sentence_to_plain_kif` (plain) produce it.
//
//   * **source** — the original `AstNode`(s) the sentence was built from, as
//     parsed.  A sentence can have several sources (logical equivalence / dedup
//     collapse to one content-addressed id), so [`SyntacticLayer::display_source`]
//     can render all of them grouped, or just the first.

use crate::AstNode;
use crate::parse::Span;
use crate::parse::kif::dis::AstKif; // `.flat()` / `.pretty_print()` / `.format_plain()`
use crate::types::{Element, Literal, SentenceId};

use super::SyntacticLayer;

// -- Source vs. normalized views ----------------------------------------------

/// How [`SyntacticLayer::display_source`] renders a sentence's provenance.
pub(crate) enum SourceMode {
    /// Every source formula that produced the sentence, grouped (the default).
    All,
    /// Only the first source formula (by fingerprint order).
    First,
}

impl SyntacticLayer {
    /// Reconstruct an [`AstNode`] from the interned **normalized** sentence
    /// `sid` — the bridge that lets every Sentence-level renderer reuse the one
    /// KIF emitter in [`crate::parse::kif::dis`] instead of walking `Sentence`
    /// directly.  Sub-formulas (`Element::Sub`) are resolved by content hash and
    /// recursed.  Reconstructed nodes carry synthetic spans (the normalized form
    /// has no source).
    pub(crate) fn sentence_to_ast(&self, sid: SentenceId) -> AstNode {
        let Some(sentence) = self.sentence(sid) else {
            // Unknown sid — preserve the old `sentence_to_plain_kif` placeholder
            // text (`sid: N`) so output stays identical.
            return AstNode::Symbol { name: format!("sid: {}", sid), span: Span::synthetic() };
        };
        let elements = sentence.elements.iter().map(|el| self.element_to_ast(el)).collect();
        AstNode::List { elements, span: Span::synthetic() }
    }

    fn element_to_ast(&self, el: &Element) -> AstNode {
        let span = Span::synthetic();
        match el {
            Element::Symbol(sym) =>
                AstNode::Symbol { name: sym.name().to_string(), span },
            Element::Variable { name, is_row: false, .. } =>
                AstNode::Variable { name: name.clone(), span },
            Element::Variable { name, is_row: true, .. } =>
                AstNode::RowVariable { name: name.clone(), span },
            Element::Literal(Literal::Str(s))    => AstNode::Str { value: s.clone(), span },
            Element::Literal(Literal::Number(n)) => AstNode::Number { value: n.clone(), span },
            Element::Op(op)                      => AstNode::Operator { op: op.clone(), span },
            Element::Sub(sub_sid)                => self.sentence_to_ast(*sub_sid),
        }
    }

    /// Render `sid` as its **normalized** KIF — structural only, no source info.
    pub(crate) fn display_normalized(&self, sid: SentenceId) -> String {
        sentence_to_plain_kif(sid, self)
    }

    /// Render the **source** formula(s) `sid` was built from, as parsed.
    ///
    /// A content-addressed sentence can be produced by several source formulas
    /// (e.g. `(<=> A B)` and a separate `(=> A B)` both yield `(=> A B)`).
    /// [`SourceMode::All`] groups them (each under a `; source i/n` header);
    /// [`SourceMode::First`] shows one.  Falls back to the normalized form when
    /// no source is recorded (a synthetic sentence).
    pub(crate) fn display_source(&self, sid: SentenceId, mode: SourceMode) -> String {
        self.display_source_styled(sid, mode, false)
    }

    /// Like [`Self::display_source`], but rendering each source formula with
    /// ANSI-coloured pretty-printing (`pretty_print`) instead of plain text.
    /// Used by diagnostic output so the offending KIF is syntax-highlighted on
    /// screen, consistent with the rest of the tool.
    pub(crate) fn display_source_pretty(&self, sid: SentenceId, mode: SourceMode) -> String {
        self.display_source_styled(sid, mode, true)
    }

    fn display_source_styled(&self, sid: SentenceId, mode: SourceMode, color: bool) -> String {
        let fps = self.source_fingerprints(sid);
        let nodes: Vec<AstNode> = match mode {
            SourceMode::First => fps.iter().take(1).filter_map(|fp| self.source_ast(*fp)).collect(),
            SourceMode::All   => fps.iter().filter_map(|fp| self.source_ast(*fp)).collect(),
        };
        let render = |n: &AstNode| if color { n.pretty_print(0) } else { n.format_plain(0) };

        match nodes.as_slice() {
            // No source AST (synthetic, source evicted, or a nested sub-sentence
            // that only exists embedded in a root) — render the *normalized*
            // sentence instead, colourised too when requested so these don't
            // appear as the lone un-highlighted snippet in diagnostic output.
            []      => if color {
                // Reconstruct the normalized sentence and render it through the
                // one KIF emitter (canonical width-based layout), colourised.
                self.sentence_to_ast(sid).pretty_print(0)
            } else {
                self.display_normalized(sid)
            },
            [only]  => render(only),
            many    => {
                let n = many.len();
                let mut out = String::new();
                for (i, node) in many.iter().enumerate() {
                    out.push_str(&format!("; source {}/{}\n", i + 1, n));
                    out.push_str(&render(node));
                    out.push('\n');
                }
                out
            }
        }
    }

    /// The source fingerprints that produced `sid` — the inverse of the store's
    /// `forward` (`fingerprint → roots`) map.  A linear scan (display is cold);
    /// sorted for deterministic output.
    fn source_fingerprints(&self, sid: SentenceId) -> Vec<u64> {
        let mut fps = self.fingerprints_producing(sid);
        fps.sort_unstable();
        fps
    }

    /// The source [`Span`](crate::parse::Span) (`file:line`) `sid` was parsed
    /// from — the first non-synthetic source occurrence in deterministic
    /// fingerprint order.  `None` for synthetic / LMDB-rehydrated sentences
    /// that carry no real origin.  Backs the `file:line` header in
    /// [`Diagnostic`](crate::Diagnostic) output.
    pub(crate) fn source_span(&self, sid: SentenceId) -> Option<crate::parse::Span> {
        for fp in self.source_fingerprints(sid) {
            if let Some(node) = self.source_ast(fp) {
                let sp = node.span();
                if !sp.is_synthetic() {
                    return Some(sp.clone());
                }
            }
        }
        None
    }
}

// -- Plain-text KIF formatter (normalized) ------------------------------------

/// Format the normalized sentence `sid` as flat plain KIF — now via the
/// Sentence→AstNode bridge and the single KIF emitter (`kif::dis::flat`).  Kept
/// as a free function (re-exported, used across `kb/`); behaviour is identical
/// to the former hand-rolled `Sentence` walk.
pub(crate) fn sentence_to_plain_kif(sid: SentenceId, store: &SyntacticLayer) -> String {
    crate::parse::kif::dis::flat(&store.sentence_to_ast(sid))
}

#[allow(dead_code)]
fn display_sentence_with_subs(sid: SentenceId, store: &SyntacticLayer, level: usize) -> String {
    let Some(sent) = store.sentence(sid) else { return String::new() };
    let mut root = format!("{:width$}: {}\n", sid, sentence_to_plain_kif(sid, store), width = (level * 2));
    let subs: String = sent.elements.iter().filter_map(|el| match el {
        Element::Sub(sub_sid) => Some(display_sentence_with_subs(*sub_sid, store, level + 1)),
        _ => None,
    }).collect();
    root.push_str(&subs);
    root
}

#[allow(dead_code)]
pub(crate) fn display_syntax(store: &SyntacticLayer) -> String {
    store.root_sids().into_iter()
        .map(|sid| display_sentence_with_subs(sid, store, 0))
        .collect()
}
