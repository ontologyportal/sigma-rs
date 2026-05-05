// crates/core/src/kb/store.rs
//
// Display focused function implementations

use crate::SentenceId;

use super::KnowledgeBase;
use super::KbError;

impl KnowledgeBase {
    /// Render a single sentence as a KIF string (for display).
    pub fn sentence_to_string(&self, sid: SentenceId) -> String {
        use crate::types::Element;
        if !self.layer.semantic.syntactic.has_sentence(sid) { return format!("<sid:{}>", sid); }
        let sentence = &self.layer.semantic.syntactic.sentences[self.layer.semantic.syntactic.sent_idx(sid)];
        let parts: Vec<String> = sentence.elements.iter().map(|e| match e {
            Element::Symbol { id, .. }                                       => self.layer.semantic.syntactic.sym_name(*id).to_owned(),
            Element::Variable { name, .. }                                   => name.clone(),
            Element::Literal { lit: crate::types::Literal::Str(s), .. }      => s.clone(),
            Element::Literal { lit: crate::types::Literal::Number(n), .. }   => n.clone(),
            Element::Op { op, .. }                                           => op.name().to_owned(),
            Element::Sub { sid: sub_id, .. }                                 => format!("({})", self.sentence_to_string(*sub_id)),
        }).collect();
        format!("({})", parts.join(" "))
    }

    /// Render a single sentence back to KIF notation (plain text, no ANSI).
    pub fn sentence_kif_str(&self, sid: SentenceId) -> String {
        crate::syntactic::sentence_to_plain_kif(sid, &self.layer.semantic.syntactic)
    }

    /// Pretty-print a stored sentence as **ANSI-coloured, indented
    /// KIF**. Sentences that fit within ~72 columns
    /// at `base_indent` are kept on a single line; longer ones break
    /// across lines with each top-level argument indented two columns
    /// further.
    pub fn pretty_print_sentence(&self, sid: SentenceId, base_indent: usize) -> String {
        let kif = self.sentence_kif_str(sid);
        let doc = crate::parse::parse_document("<display>", kif.as_str());
        match doc.ast.into_iter().next() {
            Some(node) => node.pretty_print(base_indent),
            None       => kif,
        }
    }

    /// Print a SemanticError with formula context to the log.
    pub fn pretty_print_error(&self, e: &KbError, level: log::Level) {
        e.pretty_print(self, level);
    }

    /// Produce a short human-readable preview of a sentence.
    pub fn formula_preview(&self, sid: SentenceId) -> String {
        let store = &self.layer.semantic.syntactic;
        if !store.has_sentence(sid) { return format!("<sid:{}>", sid); }
        let sentence = &store.sentences[store.sent_idx(sid)];
        let display = format!("{:?}", sentence.elements);
        if display.chars().count() > 60 {
            let truncated: String = display.chars().take(60).collect();
            format!("{}...", truncated)
        } else {
            display
        }
    }
}