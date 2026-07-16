// crates/core/src/kb/dis.rs
//
// Display focused function implementations

use crate::SentenceId;
use crate::parse::doc::DocItem;
use crate::parse::kif::dis::AstKif; // `.flat()` / `.pretty_print()` / `.format_plain()`

use super::KnowledgeBase;
use crate::Diagnostic;

// -- DiagnosticSource impl ----------------------------------------------------
//
// Lets `Diagnostic::render(Some(&kb))` pull source-line context for
// any sentence id the diagnostic mentions.
impl<L: crate::layer::TopLayer> crate::diagnostic::DiagnosticSource for KnowledgeBase<L> {
    fn render_sentence(
        &self,
        sid:            crate::types::SentenceId,
        _highlight_arg: i32,
    ) -> Option<String> {
        let store = &self.layer.semantic().syntactic;
        if !store.has_sentence(sid) { return None; }
        // Diagnostics want the *source* formula(s) the user wrote — the
        // normalized sentence carries no source syntax (spans were dropped at
        // build).  Falls back to the normalized form for synthetic sentences.
        Some(store.display_source_pretty(sid, crate::syntactic::SourceMode::All))
    }

    fn sentence_location(&self, sid: crate::types::SentenceId) -> Option<crate::parse::Span> {
        self.layer.semantic().syntactic.source_span(sid)
    }

    /// Column span (0-based start, length) of element `arg` within the *flat*
    /// one-line rendering `(e0 e1 e2 …)` of sentence `sid` — used to draw a
    /// caret underline (`^^^`) beneath the offending argument.  `arg` indexes
    /// `elements` directly (matching `highlight_arg`), so `arg == 2` is the
    /// second argument.  Returns `None` for an out-of-range index; alignment is
    /// only valid when the snippet renders on a single line, which the caller
    /// checks.
    fn highlight_span(&self, sid: crate::types::SentenceId, arg: i32) -> Option<(usize, usize)> {
        use crate::types::{Element, Literal};
        if arg < 0 { return None; }
        let store = &self.layer.semantic().syntactic;
        let sentence = store.sentence(sid)?;
        let hi = arg as usize;
        if hi >= sentence.elements.len() { return None; }

        // Char length of one element in flat KIF (matches `sentence_to_plain_kif`).
        let flat_len = |el: &Element| -> usize {
            match el {
                Element::Symbol(s)                  => s.name().chars().count(),
                Element::Variable { name, .. }      => 1 + name.chars().count(), // ? or @
                Element::Literal(Literal::Str(s))   => s.chars().count(),        // quotes included
                Element::Literal(Literal::Number(n))=> n.chars().count(),
                Element::Op(op)                     => op.name().chars().count(),
                Element::Sub(sub)                   => store.display_normalized(*sub).chars().count(),
            }
        };

        // "(e0 e1 e2 …)": '(' at col 0, then each element preceded by one space.
        let mut start = 1; // past '('
        for el in &sentence.elements[..hi] {
            start += flat_len(el) + 1; // element + the following space
        }
        Some((start, flat_len(&sentence.elements[hi])))
    }

    fn arg_count(&self, sid: crate::types::SentenceId) -> Option<usize> {
        let s = self.layer.semantic().syntactic.sentence(sid)?;
        Some(s.elements.len().saturating_sub(1))
    }
}

impl<L: crate::layer::TopLayer> KnowledgeBase<L> {
    /// Render a single sentence as a KIF string (for display).
    pub fn sentence_to_string(&self, sid: SentenceId) -> String {
        use crate::types::Element;
        if !self.layer.semantic().syntactic.has_sentence(sid) {
            return format!("<sid:{}>", sid);
        }
        let sentence = &self.layer.semantic().syntactic.sentence(sid).unwrap();
        let parts: Vec<String> = sentence.elements.iter().map(|e| match e {
            Element::Symbol(sym)                                    => sym.to_string(),
            Element::Variable { name, .. }                       => name.clone(),
            Element::Literal(crate::types::Literal::Str(s))      => s.clone(),
            Element::Literal(crate::types::Literal::Number(n))   => n.clone(),
            Element::Op(op)                                      => op.name().to_owned(),
            Element::Sub(sub_id)                                    => format!("({})", self.sentence_to_string(*sub_id)),
        }).collect();
        format!("({})", parts.join(" "))
    }

    /// Render a single sentence back to KIF notation (plain text, no ANSI).
    pub fn sentence_kif_str(&self, sid: SentenceId) -> String {
        crate::syntactic::sentence_to_plain_kif(sid, &self.layer.semantic().syntactic)
    }

    /// Pretty-print a stored sentence as **ANSI-coloured, indented
    /// KIF**. Sentences that fit within ~72 columns
    /// at `base_indent` are kept on a single line; longer ones break
    /// across lines with each top-level argument indented two columns
    /// further.
    pub fn pretty_print_sentence(&self, sid: SentenceId, base_indent: usize) -> String {
        let kif = self.sentence_kif_str(sid);
        let doc = crate::parse::parse_document("<display>", kif.as_str(), crate::Parser::Kif);
        match doc.ast.into_iter().next() {
            Some(DocItem::Stmt(node)) => node.pretty_print(base_indent),
            _       => kif,
        }
    }

    /// [`Self::pretty_print_sentence`]'s plain-text twin: the same indented,
    /// width-wrapped layout with no ANSI colour codes — safe for contexts
    /// that aren't a terminal (e.g. a browser DOM).
    pub fn pretty_print_sentence_plain(&self, sid: SentenceId, base_indent: usize) -> String {
        let kif = self.sentence_kif_str(sid);
        let doc = crate::parse::parse_document("<display>", kif.as_str(), crate::Parser::Kif);
        match doc.ast.into_iter().next() {
            Some(DocItem::Stmt(node)) => node.format_plain(base_indent),
            _       => kif,
        }
    }

    /// Print a SemanticError with formula context to the log.
    pub fn pretty_print_error(&self, e: &Diagnostic, _level: log::Level) {
        e.emit(Some(self));
    }

    /// Render a diagnostic to its final string (header + source context),
    /// exactly as [`Self::pretty_print_error`] would log it.  Exposed so
    /// callers can deduplicate identical renderings before emitting — e.g.
    /// `validate` collapses the many copies a row-variable-expanded axiom
    /// produces (each concrete arity is its own root sharing one source line).
    pub fn render_diagnostic(&self, e: &Diagnostic) -> String {
        e.render(Some(self))
    }

    /// Produce a short human-readable preview of a sentence.
    pub fn formula_preview(&self, sid: SentenceId) -> String {
        let store = &self.layer.semantic().syntactic;
        if !store.has_sentence(sid) { return format!("<sid:{}>", sid); }
        let sentence = store.sentence(sid).unwrap();
        let display = format!("{:?}", sentence.elements);
        if display.chars().count() > 60 {
            let truncated: String = display.chars().take(60).collect();
            format!("{}...", truncated)
        } else {
            display
        }
    }
}