// crates/sumo-kb/src/kb/export.rs
//
// TPTP export entrypoints on KnowledgeBase: `to_tptp`, `to_tptp_cnf`,
// `format_sentence_tptp`, and their helpers. Split out of kb.rs to keep
// the main module focused on storage / ingestion / promotion.

use std::collections::HashSet;

use super::KnowledgeBase;
use crate::tptp::{TptpLang, TptpOptions};
use crate::types::SentenceId;

#[cfg(feature = "cnf")]
use crate::error::KbError;
#[cfg(feature = "cnf")]
use crate::kif_store::KifStore;

impl KnowledgeBase {
    /// Generate TPTP for the KB.
    ///
    /// - Axioms = all promoted/loaded sentences (fingerprint session=None).
    /// - Assertions = sentences in `session` (if Some) rendered as `hypothesis`.
    /// - Pass `session=None` to omit assertions.
    ///
    /// Routes through the `NativeConverter` + `assemble_tptp` IR pipeline:
    /// SID-based axiom names (`kb_<sid>`), per-axiom KIF comments when
    /// `opts.show_kif_comment` is set, `excluded` predicate filter
    /// applied before conversion.
    pub fn to_tptp(&self, opts: &TptpOptions, session: Option<&str>) -> String {
        use crate::vampire::assemble::{assemble_tptp, AssemblyOpts};
        use crate::vampire::converter::{Mode, NativeConverter};

        let mode = match opts.lang {
            TptpLang::Tff => Mode::Tff,
            TptpLang::Fof => Mode::Fof,
        };

        let mut conv = NativeConverter::new(&self.layer.store, &self.layer, mode)
            .with_hide_numbers(opts.hide_numbers);

        let axiom_ids = self.axiom_ids_set();
        let mut axioms_sorted: Vec<SentenceId> = axiom_ids.into_iter().collect();
        axioms_sorted.sort_unstable();
        for sid in axioms_sorted {
            if self.sentence_excluded(sid, &opts.excluded) { continue; }
            conv.add_axiom(sid);
        }

        if let Some(name) = session {
            if let Some(sids) = self.sessions.get(name) {
                for &sid in sids {
                    if self.sentence_excluded(sid, &opts.excluded) { continue; }
                    conv.add_axiom(sid);
                }
            }
        }

        let (problem, sid_map) = conv.finish();
        assemble_tptp(&problem, &sid_map, &AssemblyOpts {
            show_kif: opts.show_kif_comment,
            layer:    Some(&self.layer),
            ..AssemblyOpts::default()
        })
    }

    /// Return the head predicate name of a sentence, if it has one.
    /// Returns `None` for operator-rooted sentences (e.g. `(and ...)`) or
    /// for sentences whose first element is not a plain symbol.
    fn sentence_head_name(&self, sid: SentenceId) -> Option<String> {
        use crate::types::Element;
        let store = &self.layer.store;
        if !store.has_sentence(sid) { return None; }
        let sentence = &store.sentences[store.sent_idx(sid)];
        match sentence.elements.first()? {
            Element::Symbol(id) => Some(store.sym_name(*id).to_owned()),
            _ => None,
        }
    }

    /// `true` if the sentence's head predicate matches an `excluded` entry.
    fn sentence_excluded(&self, sid: SentenceId, excluded: &HashSet<String>) -> bool {
        if excluded.is_empty() { return false; }
        self.sentence_head_name(sid)
            .map(|n| excluded.contains(&n))
            .unwrap_or(false)
    }

    /// Generate TPTP CNF from pre-computed clauses.
    /// Returns an error if `clausify()` has not been called (or cnf_mode=false).
    #[cfg(feature = "cnf")]
    pub fn to_tptp_cnf(&self, session: Option<&str>) -> Result<String, KbError> {
        use std::fmt::Write as _;

        if self.clauses.is_empty() {
            return Err(KbError::Other(
                "to_tptp_cnf: no clauses available; call clausify() first".into()
            ));
        }

        let sid_set: Option<HashSet<SentenceId>> = session
            .and_then(|s| self.sessions.get(s))
            .map(|v| v.iter().copied().collect());

        let store = &self.layer.store;
        let mut out = String::new();
        let mut idx = 0usize;
        for (&sid, clauses) in &self.clauses {
            if let Some(ref filter) = sid_set {
                if !filter.contains(&sid) { continue; }
            }
            let role = if self.axiom_ids_set().contains(&sid) { "axiom" } else { "hypothesis" };
            for clause in clauses {
                let lit_strs: Vec<String> = clause.literals.iter()
                    .map(|lit| format_cnf_literal(store, lit))
                    .collect();
                let body = if lit_strs.len() == 1 {
                    lit_strs[0].clone()
                } else {
                    format!("({})", lit_strs.join(" | "))
                };
                let _ = writeln!(out, "cnf(c_{}, {}, {}).", idx, role, body);
                idx += 1;
            }
        }
        Ok(out)
    }

    /// Render a single sentence as TPTP.
    ///
    /// Returns the formula body only (no `tff(...)` / `fof(...)` wrapper);
    /// callers add their own `<kw>(name, role, ...)` framing.  Respects
    /// `opts.query` (existential wrap for conjectures vs universal wrap
    /// for axioms), `opts.lang`, and `opts.hide_numbers`.
    pub fn format_sentence_tptp(&self, sid: SentenceId, opts: &TptpOptions) -> String {
        use crate::vampire::converter::{Mode, NativeConverter};

        let mode = match opts.lang {
            TptpLang::Tff => Mode::Tff,
            TptpLang::Fof => Mode::Fof,
        };
        let mut conv = NativeConverter::new(&self.layer.store, &self.layer, mode)
            .with_hide_numbers(opts.hide_numbers);

        if opts.query {
            conv.set_conjecture(sid);
            let (problem, _) = conv.finish();
            return problem
                .conjecture_ref()
                .map(|f| f.to_tptp())
                .unwrap_or_default();
        }
        conv.add_axiom(sid);
        let (problem, _) = conv.finish();
        problem
            .axioms()
            .first()
            .map(|f| f.to_tptp())
            .unwrap_or_default()
    }
}

// -- CNF clause formatting -----------------------------------------------------

#[cfg(feature = "cnf")]
fn format_cnf_literal(store: &KifStore, lit: &crate::types::CnfLiteral) -> String {
    let pred = format_cnf_term(store, &lit.pred);
    let args: Vec<String> = lit.args.iter().map(|t| format_cnf_term(store, t)).collect();
    let atom = if args.is_empty() {
        pred
    } else {
        format!("{}({})", pred, args.join(","))
    };
    if lit.positive { atom } else { format!("~{}", atom) }
}

#[cfg(feature = "cnf")]
fn format_cnf_term(store: &KifStore, term: &crate::types::CnfTerm) -> String {
    use crate::types::CnfTerm;
    match term {
        CnfTerm::Const(id)  => format!("s__{}", store.sym_name(*id)),
        CnfTerm::Var(id)    => format!("V__{}", store.sym_name(*id).replace('@', "_")),
        CnfTerm::Fn { id, args } => {
            let name = format!("s__{}", store.sym_name(*id));
            let arg_strs: Vec<String> = args.iter().map(|a| format_cnf_term(store, a)).collect();
            format!("{}({})", name, arg_strs.join(","))
        }
        CnfTerm::SkolemFn { id, args } => {
            let name = format!("s__{}", store.sym_name(*id));
            let arg_strs: Vec<String> = args.iter().map(|a| format_cnf_term(store, a)).collect();
            format!("{}({})", name, arg_strs.join(","))
        }
        CnfTerm::Num(s) => s.clone(),
        CnfTerm::Str(s) => s.clone(),
    }
}
