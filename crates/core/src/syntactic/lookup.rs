// crates/core/src/syntactic/lookup.rs
//
// Pattern-based sentence lookup for SyntacticLayer.
// Distinct from the top-level `crate::lookup` module which provides
// position-based queries (offset -> element).

use crate::types::{Element, Literal, SentenceId};

use super::SyntacticLayer;

impl SyntacticLayer {
    pub(crate) fn by_head(&self, head: &str) -> &[SentenceId] {
        self.head_index.get(head).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Pattern lookup.  Tokens: literal = exact match, `_` = any one element,
    /// `_N` = consume remaining.
    pub(crate) fn lookup(&self, pattern: &str) -> Vec<SentenceId> {
        let tokens: Vec<&str> = pattern.split_whitespace().collect();
        if tokens.is_empty() { return self.roots.clone(); }
        let candidates: Box<dyn Iterator<Item = &SentenceId>> = if tokens[0] != "_" {
            match self.head_index.get(tokens[0]) { Some(ids) => Box::new(ids.iter()), None => return Vec::new() }
        } else { Box::new(self.roots.iter()) };
        candidates.copied().filter(|&sid| self.matches_pattern(sid, &tokens)).collect()
    }

    fn matches_pattern(&self, sid: SentenceId, tokens: &[&str]) -> bool {
        let elems = &self.sentences[self.sent_idx(sid)].elements;
        let (mut e_idx, mut t_idx) = (0, 0);
        while t_idx < tokens.len() {
            let tok = tokens[t_idx];
            if tok == "_" {
                if e_idx >= elems.len() { return false; }
                e_idx += 1; t_idx += 1;
            } else if tok.starts_with('_') && tok[1..].parse::<usize>().is_ok() {
                return true;
            } else {
                if e_idx >= elems.len() { return false; }
                if !self.elem_matches_name(&elems[e_idx], tok) { return false; }
                e_idx += 1; t_idx += 1;
            }
        }
        e_idx == elems.len()
    }

    fn elem_matches_name(&self, elem: &Element, name: &str) -> bool {
        match elem {
            Element::Symbol { id, .. }                          => self.sym_name(*id) == name,
            Element::Op { op, .. }                              => op.name() == name,
            Element::Variable { name: n, .. }                   => n == name,
            Element::Literal { lit: Literal::Number(n), .. }    => n == name,
            Element::Literal { lit: Literal::Str(s), .. }       => s == name,
            Element::Sub { .. }                                 => false,
        }
    }
}
