use std::collections::{HashMap, HashSet};

use crate::syntactic::SyntacticLayer;
use crate::{Element, Literal, OpKind, SentenceId, SymbolId, TptpLang};

impl SyntacticLayer {
     /// Collect every symbol id mentioned in `sent_id` (recursing into subs).
    pub(crate) fn collect_symbols(&self, sent_id: SentenceId, out: &mut HashSet<SymbolId>) {
        let Some(sentence) = self.sentence(sent_id) else { return };
        for el in &sentence.elements {
            match el {
                Element::Sub(sid) => { self.collect_symbols(*sid, out); },
                Element::Symbol(sym) => { out.insert(sym.id()); },
                _ => continue
            };
        }
    }

    /// `true` if `sent_id` (recursing into subs) contains a numeric literal
    /// anywhere in its formula.
    ///
    /// Drives `TptpLang::Auto`'s Fof-vs-Tff choice: untyped TPTP (FOF/CNF)
    /// has no numeric domain, so a numeral there becomes an opaque `n__N`
    /// constant with no arithmetic distinctness (see `hide_numbers` in
    /// `trans::lower`) — TFF emits it as a real `$int`/`$rat`/`$real`
    /// literal instead. See `sigma-rs` memory `typed-suite-tff-gate` for
    /// the concrete failure mode this exists to route around.
    pub(crate) fn sentence_has_numeral(&self, sent_id: SentenceId) -> bool {
        let Some(sentence) = self.sentence(sent_id) else { return false };
        sentence.elements.iter().any(|el| match el {
            Element::Sub(sid) => self.sentence_has_numeral(*sid),
            Element::Literal(Literal::Number(_)) => true,
            _ => false,
        })
    }

    /// Resolve `TptpLang::Auto` against a sentence set: `Tff` if any
    /// sentence contains a numeric literal (see
    /// [`Self::sentence_has_numeral`]), else the untyped default `Fof`.
    /// A non-`Auto` `mode` passes through unchanged — this never
    /// second-guesses an explicit `--lang` choice.
    ///
    /// `sids` should be the *actually selected* set for this problem (the
    /// whole KB for a bare whole-KB translate, or the post-SInE-selection
    /// axioms — plus the conjecture/query — for a test file or a live
    /// `ask`/`test` run), not the universe of everything loaded.
    pub(crate) fn resolve_tptp_lang<'a>(
        &self,
        mode: TptpLang,
        sids: impl IntoIterator<Item = &'a SentenceId>,
    ) -> TptpLang {
        if mode != TptpLang::Auto {
            return mode;
        }
        if sids.into_iter().any(|&sid| self.sentence_has_numeral(sid)) {
            TptpLang::Tff
        } else {
            TptpLang::Fof
        }
    }

    /// Collect all the variable mentioned in a sentence (recurse if needed)
    pub(crate) fn collect_vars(&self, sent_id: SentenceId, out: &mut HashMap<SymbolId, u32>) {
        let Some(sentence) = self.sentence(sent_id) else { return };
        for el in &sentence.elements {
            match el {
                Element::Sub(sid) => { self.collect_vars(*sid, out); },
                Element::Variable { id, var_index, .. } => { out.insert(*id, *var_index); },
                _ => continue
            };
        }
    }

    /// Collect the variables that are bound by a FOL quantifier in the formula.
    /// Unbound variables are collected into a top level quanitifier
    ///
    /// If `in_formula_pos` if specified, variables bound in a formula which 
    /// appears nested inside a non-logical relation — 
    /// e.g. `(hasPurpose ?X (exists (?Y) ...))` — are returned as first order
    /// translation reifies the existential into a pseudo-relation and therefore 
    /// `?Y` becomes unbound in the process
    pub(crate) fn collect_bound_vars(
        &self,
        sid: SentenceId,
        in_formula_pos: bool,
        out: &mut HashSet<SymbolId>,
    ) {
        let Some(sentence) = self.sentence(sid) else { return };

        if in_formula_pos {
            if let Some(op) = sentence.op() {
                if matches!(op, OpKind::ForAll | OpKind::Exists) {
                    if let Some(Element::Sub(vl_sid)) = sentence.elements.get(1) {
                        if let Some(sub_sent) = self.sentence(*vl_sid) {
                            for e in &sub_sent.elements {
                                if let Element::Variable { id, .. } = e {
                                    out.insert(*id);
                                }
                            }   
                        }
                    }
                }
            }
        }

        // Dispatch sub-sentence positions by the current operator/head. The
        // rules mirror what `sid_to_formula` vs `sid_to_term` actually do
        // when they recurse, so `bound` stays in lockstep with where real
        // FOL binders end up in the IR output.
        let op = sentence.op();
        let sub_in_formula_pos = match op {
            // Logical connectives keep their children in formula position.
            Some(OpKind::And) | Some(OpKind::Or) | Some(OpKind::Not)
            | Some(OpKind::Implies) | Some(OpKind::Iff) => in_formula_pos,

            // Quantifiers are formula-level when we're at a formula site;
            // their body (and the var-list sub, which collect_all_var_ids
            // already indexes) inherit that position.
            Some(OpKind::ForAll) | Some(OpKind::Exists) => in_formula_pos,

            // `Equal` emits `IrF::eq(term, term)` — both sides are terms.
            Some(OpKind::Equal) => false,

            // Non-operator heads / atomic predicate applications:
            // `atomic_sid_to_formula` processes their args as terms
            // (`element_to_term`).  Inside a term context, everything
            // nested stays a term.
            None => false,
        };

        for elem in &sentence.elements {
            if let Element::Sub(sub) = elem {
                self.collect_bound_vars(*sub, sub_in_formula_pos, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(kif: &str) -> SyntacticLayer {
        let mut store = SyntacticLayer::default();
        store.load_kif(kif, "test");
        store
    }

    fn root_sid(store: &SyntacticLayer, nth: usize) -> SentenceId {
        let mut r = store.root_sids();
        r.sort();
        r[nth]
    }

    #[test]
    fn sentence_has_numeral_detects_a_direct_literal() {
        let s = store("(greaterThan ?X 5)");
        assert!(s.sentence_has_numeral(root_sid(&s, 0)));
    }

    #[test]
    fn sentence_has_numeral_recurses_into_subs() {
        // The numeral sits inside the antecedent, a nested sub-sentence of
        // the top-level `=>` — this only passes if the recursion into
        // `Element::Sub` actually happens.
        let s = store("(=> (greaterThan ?X 5) (instance ?X Big))");
        assert!(s.sentence_has_numeral(root_sid(&s, 0)));
    }

    #[test]
    fn sentence_has_numeral_false_with_no_numerals() {
        let s = store("(instance Fido Dog)");
        assert!(!s.sentence_has_numeral(root_sid(&s, 0)));
    }

    #[test]
    fn resolve_tptp_lang_picks_tff_only_when_a_numeral_is_present() {
        // Sentence ids are content hashes, not load-order indices — split
        // the two roots by which one actually has a numeral rather than
        // assuming a load-order/sort-order correspondence.
        let s = store("(instance Fido Dog)\n(greaterThan ?X 5)");
        let roots = s.root_sids();
        assert_eq!(roots.len(), 2);
        let plain = *roots.iter().find(|&&sid| !s.sentence_has_numeral(sid)).unwrap();
        let numeric = *roots.iter().find(|&&sid| s.sentence_has_numeral(sid)).unwrap();

        assert_eq!(s.resolve_tptp_lang(TptpLang::Auto, [&plain]), TptpLang::Fof,
            "no numeral in the selected set -> untyped fallback");
        assert_eq!(s.resolve_tptp_lang(TptpLang::Auto, [&numeric]), TptpLang::Tff,
            "a numeral in the selected set -> typed");
        assert_eq!(s.resolve_tptp_lang(TptpLang::Auto, [&plain, &numeric]), TptpLang::Tff,
            "one numeral anywhere in the set is enough to upgrade the whole problem");
    }

    #[test]
    fn resolve_tptp_lang_never_overrides_an_explicit_choice() {
        let s = store("(greaterThan ?X 5)");
        let numeric = root_sid(&s, 0);
        assert_eq!(s.resolve_tptp_lang(TptpLang::Fof, [&numeric]), TptpLang::Fof,
            "an explicit --lang fof is never second-guessed, even with a numeral present");
        assert_eq!(s.resolve_tptp_lang(TptpLang::Tff, [&numeric]), TptpLang::Tff);
    }
}