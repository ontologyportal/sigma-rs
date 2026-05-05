// crates/core/src/syntactic/display.rs
//
// ANSI / plain-text rendering for SyntacticLayer-owned sentences.

use std::fmt;

use inline_colorization::*;

use crate::types::{Element, Literal, SentenceId};

use super::SyntacticLayer;

// -- Display wrappers ----------------------------------------------------------

pub(crate) struct ElementDisplay<'a> {
    pub element:   &'a Element,
    pub store:     &'a SyntacticLayer,
    pub indent:    usize,
    pub highlight: bool,
}

impl<'a> fmt::Display for ElementDisplay<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.highlight { write!(f, "{style_bold}{style_underline}")?; }
        let res = match self.element {
            Element::Symbol { id, .. } => {
                let name = self.store.sym_name(*id);
                if name.chars().next().map_or(false, |c| c.is_uppercase()) {
                    write!(f, "{color_yellow}{}{color_reset}", name)
                } else {
                    write!(f, "{color_blue}{}{color_reset}", name)
                }
            }
            Element::Variable { name, is_row: false, .. }    => write!(f, "{color_magenta}?{}{color_reset}", name),
            Element::Variable { name, is_row: true,  .. }    => write!(f, "{color_magenta}@{}{color_reset}", name),
            Element::Literal { lit: Literal::Str(s), .. }    => write!(f, "{}", s),
            Element::Literal { lit: Literal::Number(n), .. } => write!(f, "{color_green}{}{color_reset}", n),
            Element::Op { op, .. }                           => write!(f, "{color_cyan}{}{color_reset}", op),
            Element::Sub { sid, .. }                         => SentenceDisplay::raw(*sid, self.store, self.indent, -1).fmt(f),
        };
        if self.highlight { write!(f, "{style_reset}") } else { res }
    }
}

pub(crate) struct SentenceDisplay<'a> {
    pub sid:           SentenceId,
    pub store:         &'a SyntacticLayer,
    pub indent:        usize,
    pub show_gutter:   bool,
    pub highlight_arg: i32,
}

impl<'a> SentenceDisplay<'a> {
    pub(crate) fn raw(sid: SentenceId, store: &'a SyntacticLayer, indent: usize, arg: i32) -> Self {
        Self { sid, store, indent, show_gutter: false, highlight_arg: arg }
    }
    fn to_raw_string(&self, arg: i32) -> String {
        SentenceDisplay::raw(self.sid, self.store, 0, arg).to_string()
    }
}

impl<'a> fmt::Display for SentenceDisplay<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.show_gutter {
            let content  = self.to_raw_string(self.highlight_arg);
            let line_no  = self.store.sentences[self.store.sent_idx(self.sid)].span.line;
            let num_str  = line_no.to_string();
            let width    = num_str.len().min(6);
            let blank    = " ".repeat(width);
            for (i, line) in content.lines().enumerate() {
                if i > 0 { writeln!(f)?; }
                if i == 0 { write!(f, "{:>width$} | {}", line_no, line, width = width)?; }
                else       { write!(f, "{} | {}", blank, line)?; }
            }
            return Ok(());
        }
        let sentence     = &self.store.sentences[self.store.sent_idx(self.sid)];
        let child_indent = self.indent + 1;
        write!(f, "(")?;
        for (i, el) in sentence.elements.iter().enumerate() {
            let highlight = self.highlight_arg == i as i32;
            if i == 0 {
                ElementDisplay { element: el, store: self.store, indent: child_indent, highlight }.fmt(f)?;
            } else if matches!(el, Element::Sub { .. }) {
                write!(f, "\n{}", "  ".repeat(child_indent))?;
                ElementDisplay { element: el, store: self.store, indent: child_indent, highlight }.fmt(f)?;
            } else {
                write!(f, " ")?;
                ElementDisplay { element: el, store: self.store, indent: child_indent, highlight }.fmt(f)?;
            }
        }
        write!(f, ")")
    }
}

// -- Plain-text KIF formatter -------------------------------------------------

/// Recursively format a sentence as plain KIF text (no ANSI escapes).
pub(crate) fn sentence_to_plain_kif(sid: SentenceId, store: &SyntacticLayer) -> String {
    let sentence = &store.sentences[store.sent_idx(sid)];
    let mut out = String::from("(");
    for (i, elem) in sentence.elements.iter().enumerate() {
        if i > 0 { out.push(' '); }
        match elem {
            Element::Symbol { id, .. } => out.push_str(store.sym_name(*id)),
            Element::Variable { name, is_row: false, .. } => {
                out.push('?');
                out.push_str(name);
            }
            Element::Variable { name, is_row: true, .. } => {
                out.push('@');
                out.push_str(name);
            }
            Element::Literal { lit: Literal::Str(s), .. }    => out.push_str(s),
            Element::Literal { lit: Literal::Number(n), .. } => out.push_str(n),
            Element::Op { op, .. }                           => out.push_str(op.name()),
            Element::Sub { sid: sub_sid, .. }                => out.push_str(&sentence_to_plain_kif(*sub_sid, store)),
        }
    }
    out.push(')');
    out
}
