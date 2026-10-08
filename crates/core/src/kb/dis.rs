// crates/core/src/kb/dis.rs
//
// Display focused function implementations

use crate::parse::kif::dis::AstKif;
use crate::SentenceId; // `.flat()` / `.pretty_print()` / `.format_plain()`

use super::KnowledgeBase;
use crate::Diagnostic;

/// Which form of a stored sentence [`KnowledgeBase::render_sentence`] shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SentenceForm {
    /// The normalized (canonicalized) structure the KB stores.
    Normalized,
    /// Every source formula that produced the sentence, in the syntax it was
    /// parsed from (case, macro sugar, etc. preserved). A content-addressed
    /// sentence can have several, each shown under a `; source i/n` header.
    /// Falls back to the normalized form when no source is recorded (a
    /// synthetic sentence, or one restored from persistence without it).
    Source,
}

// -- DiagnosticSource impl ----------------------------------------------------
//
// Lets `Diagnostic::render(Some(&kb))` pull source-line context for
// any sentence id the diagnostic mentions.
impl<L: crate::layer::TopLayer> crate::diagnostic::DiagnosticSource for KnowledgeBase<L> {
    fn render_sentence(
        &self,
        sid: crate::types::SentenceId,
        _highlight_arg: i32,
    ) -> Option<String> {
        let store = &self.layer.semantic().syntactic;
        if !store.has_sentence(sid) {
            return None;
        }
        // Diagnostics want the *source* formula(s) the user wrote — the
        // normalized sentence carries no source syntax (spans were dropped at
        // build).  Falls back to the normalized form for synthetic sentences.
        Some(store.display_source_pretty(sid, crate::syntactic::SourceMode::All, 0))
    }

    fn sentence_location(&self, sid: crate::types::SentenceId) -> Option<crate::parse::Span> {
        self.layer.semantic().syntactic.source_span(sid)
    }

    fn sentence_locations(
        &self,
    ) -> std::collections::HashMap<crate::types::SentenceId, crate::parse::Span> {
        self.layer.semantic().syntactic.source_span_index()
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
        if arg < 0 {
            return None;
        }
        let store = &self.layer.semantic().syntactic;
        let sentence = store.sentence(sid)?;
        let hi = arg as usize;
        if hi >= sentence.elements.len() {
            return None;
        }

        // Char length of one element in flat KIF (matches `sentence_to_plain_kif`).
        let flat_len = |el: &Element| -> usize {
            match el {
                Element::Symbol(s) => s.name().chars().count(),
                Element::Variable { name, .. } => 1 + name.chars().count(), // ? or @
                Element::Literal(Literal::Str(s)) => s.chars().count(),     // quotes included
                Element::Literal(Literal::Number(n)) => n.chars().count(),
                Element::Op(op) => op.name().chars().count(),
                Element::Sub(sub) => store.display_normalized(*sub).chars().count(),
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
    /// Render a single sentence back to KIF notation (plain text, no ANSI).
    pub fn sentence_kif_str(&self, sid: SentenceId) -> String {
        crate::syntactic::sentence_to_plain_kif(sid, &self.layer.semantic().syntactic)
    }

    /// Pretty-print a stored sentence as indented KIF. Sentences that fit
    /// within ~72 columns at `base_indent` stay on one line; longer ones break
    /// with each top-level argument indented two further columns.
    ///
    /// `form` picks what is shown (see [`SentenceForm`]); `color` adds ANSI
    /// colour codes for terminals and must be `false` for any other sink
    /// (e.g. a browser DOM).
    pub fn render_sentence(
        &self,
        sid: SentenceId,
        form: SentenceForm,
        color: bool,
        base_indent: usize,
    ) -> String {
        let syn = &self.layer.semantic().syntactic;
        match (form, color) {
            (SentenceForm::Normalized, true) => syn.sentence_to_ast(sid).pretty_print(base_indent),
            (SentenceForm::Normalized, false) => syn.sentence_to_ast(sid).format_plain(base_indent),
            (SentenceForm::Source, true) => {
                syn.display_source_pretty(sid, crate::syntactic::SourceMode::All, base_indent)
            }
            (SentenceForm::Source, false) => {
                syn.display_source_plain(sid, crate::syntactic::SourceMode::All, base_indent)
            }
        }
    }

    /// Log a diagnostic with its formula context, at the diagnostic's own
    /// severity.
    pub fn pretty_print_error(&self, e: &Diagnostic) {
        e.emit(Some(self));
    }

    /// Render a diagnostic to its final string (header + source context),
    /// exactly as [`Self::pretty_print_error`] would log it.  Exposed so
    /// callers can deduplicate identical renderings before emitting -- e.g.
    /// `validate` collapses the many copies a row-variable-expanded axiom
    /// produces (each concrete arity is its own root sharing one source line).
    pub fn render_diagnostic(&self, e: &Diagnostic) -> String {
        e.render(Some(self))
    }
}
