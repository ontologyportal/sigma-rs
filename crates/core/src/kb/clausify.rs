//! `KnowledgeBase<ProverLayer>::clausify_*` — CNF form of the KB (or a
//! single ad hoc formula), rendered back to flat SUO-KIF, via the native
//! prover's own clausifier (`prover::saturate::clausify`), and
//! `matching_axioms` -- the axioms whose clauses unify with / subsume / are
//! subsumed by an ad hoc formula's clauses, via the prover's unifier.
//! Native-only: the clause representation (`PClause`/`AtomTable`) only
//! exists under `native-prover`.

#![cfg(feature = "native-prover")]

use crate::layer::TopLayer;
use std::cmp::Reverse;
use std::collections::HashSet;

use crate::prover::saturate::clause::{PClause, Term};
use crate::prover::saturate::kbo::KboOrdering;
use crate::prover::saturate::prover::{clause_subsumes_in, SubsScratch};
use crate::prover::saturate::render::{clause_to_kif, SkolemNames};
use crate::prover::saturate::unify::{slot_atom, unify_off};
use crate::prover::saturate::ProverLayer;
use crate::types::{SentenceId, SymbolId};

use super::KnowledgeBase;

/// Clausify every axiom currently loaded into `layer` and render each
/// resulting clause as flat SUO-KIF (`(or lit1 lit2 ...)`, bare literal for
/// unit clauses). One entry per clause; axioms are visited in `SentenceId`
/// order for determinism. Skolem symbols introduced during clausification
/// are renamed `SkFnN`/`SkCN`, consistently across the whole call.
fn clausify_all_in<S: TopLayer + 'static>(
    layer: &ProverLayer<S>,
    mut roots: Vec<SentenceId>,
) -> Vec<String> {
    roots.sort_unstable();
    let syn = &layer.semantic().syntactic;
    let mut sk = SkolemNames::default();
    roots
        .iter()
        .flat_map(|&root| {
            let clauses = layer.clauses_for(root);
            clauses
                .iter()
                .map(|c| clause_to_kif(c, &layer.atoms, syn, &mut sk))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Clausify a single ad hoc KIF formula (not pulled from the KB store) with
/// `layer`'s clausifier and render its clauses as flat SUO-KIF. Empty for a
/// formula that clausifies to nothing; an error when `kif` does not parse.
fn clausify_formula_in<S: TopLayer + 'static>(
    layer: &ProverLayer<S>,
    kif: &str,
) -> crate::DiagResult<Vec<String>> {
    let asts = crate::prover::Conjecture::parse(kif, crate::Parser::Kif { options: None })
        .map_err(|r| crate::Diagnostic::new_error("parse", "clausify-input", r.raw_output))?;
    let clauses = layer.clausify_asts(asts);
    let syn = &layer.semantic().syntactic;
    let mut sk = SkolemNames::default();
    Ok(clauses
        .iter()
        .map(|c| clause_to_kif(c, &layer.atoms, syn, &mut sk))
        .collect())
}

/// Which clause relation [`KnowledgeBase::matching_axioms`] tests between a
/// query clause and an axiom clause.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MatchMode {
    /// Some query literal unifies with some axiom literal (variables of
    /// both sides are bindable, renamed apart).
    #[default]
    Unify,
    /// The query clause subsumes the axiom clause: the axiom clause is an
    /// instance of the query (query variables bind, axiom variables stay
    /// rigid).
    Instances,
    /// The axiom clause subsumes the query clause: the axiom is at least as
    /// general as the query.
    Generalizations,
}

/// How literal signs are compared by [`KnowledgeBase::matching_axioms`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MatchPolarity {
    /// Compare atoms only; `(not P)` and `P` are interchangeable.
    #[default]
    Ignore,
    /// Paired literals must have the same sign.
    Same,
    /// Paired literals must have opposite signs (resolution partners).
    Opposite,
}

/// Options for [`KnowledgeBase::matching_axioms`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MatchOpts {
    pub mode: MatchMode,
    pub polarity: MatchPolarity,
}

/// A clause as signed slot-variable terms (`Vk` -> slot `k`); `None` when an
/// atom fails to resolve.
fn clause_terms<S: TopLayer + 'static>(
    layer: &ProverLayer<S>,
    clause: &PClause,
    flip: bool,
    ignore_sign: bool,
) -> Option<Vec<(bool, Term)>> {
    let syn = &layer.semantic().syntactic;
    clause
        .lits
        .iter()
        .map(|l| {
            let pos = ignore_sign || (l.pos != flip);
            Some((pos, slot_atom(&layer.atoms, syn, l.atom, 0)?))
        })
        .collect()
}

/// `true` when some literal of `q` (slots `0..q_nvars`) unifies with a
/// same-signed literal of `a` (renamed apart above `q_nvars`).
fn some_literal_unifies(
    q: &[(bool, Term)],
    q_nvars: u32,
    a: &[(bool, Term)],
    a_nvars: u32,
) -> bool {
    let mut subst = vec![None; (q_nvars + a_nvars) as usize];
    q.iter().any(|(qp, qt)| {
        a.iter()
            .any(|(ap, at)| qp == ap && unify_off(qt, 0, at, u64::from(q_nvars), &mut subst))
    })
}

/// Insert every ground symbol of `t` into `out`.
fn collect_symbols(t: &Term, out: &mut HashSet<SymbolId>) {
    match t {
        Term::Sym(s) => {
            out.insert(s.id());
        }
        Term::App(elems) => elems.iter().for_each(|e| collect_symbols(e, out)),
        Term::Var(_) | Term::Lit(_) | Term::Op(_) => {}
    }
}

/// Clausify `asts` with `layer`'s clausifier and return every promoted axiom
/// with at least one clause standing in `opts.mode`'s relation to at least
/// one query clause, under `opts.polarity`. Free variables of the query are
/// pattern variables; an explicit `exists` becomes a Skolem constant and only
/// matches itself. Empty when the query clausifies to nothing.
///
/// Ranked by the number of distinct query symbols the axiom contains (more
/// first), then by its total KBO clause weight (lighter first), then by
/// `SentenceId`.
fn matching_axioms_in<S: TopLayer + 'static>(
    layer: &ProverLayer<S>,
    roots: Vec<SentenceId>,
    asts: Vec<crate::AstNode>,
    opts: &MatchOpts,
) -> Vec<SentenceId> {
    let ignore_sign = opts.polarity == MatchPolarity::Ignore;
    let flip = opts.polarity == MatchPolarity::Opposite;
    let queries: Vec<_> = layer
        .clausify_asts(asts)
        .iter()
        .filter_map(|c| Some((clause_terms(layer, c, flip, ignore_sign)?, c.nvars)))
        .collect();
    if queries.is_empty() {
        return Vec::new();
    }
    let mut query_syms = HashSet::new();
    for (q, _) in &queries {
        q.iter()
            .for_each(|(_, t)| collect_symbols(t, &mut query_syms));
    }
    let kbo = KboOrdering::new();
    let mut scratch = SubsScratch::default();
    let mut ranked: Vec<_> = roots
        .into_iter()
        .filter_map(|root| {
            let clauses: Vec<_> = layer
                .clauses_for(root)
                .iter()
                .filter_map(|c| Some((clause_terms(layer, c, false, ignore_sign)?, c.nvars)))
                .collect();
            let matched = clauses.iter().any(|(a, a_nvars)| {
                queries.iter().any(|(q, q_nvars)| match opts.mode {
                    MatchMode::Unify => some_literal_unifies(q, *q_nvars, a, *a_nvars),
                    MatchMode::Instances => clause_subsumes_in(q, a, &mut scratch),
                    MatchMode::Generalizations => clause_subsumes_in(a, q, &mut scratch),
                })
            });
            if !matched {
                return None;
            }
            let mut syms = HashSet::new();
            let mut weight = 0u64;
            for (_, t) in clauses.iter().flat_map(|(a, _)| a) {
                collect_symbols(t, &mut syms);
                weight = weight.saturating_add(kbo.term_weight(t));
            }
            let hits = query_syms.intersection(&syms).count();
            Some((Reverse(hits), weight, root))
        })
        .collect();
    ranked.sort_unstable();
    ranked.into_iter().map(|(_, _, root)| root).collect()
}

impl<S: TopLayer + 'static> KnowledgeBase<ProverLayer<S>> {
    /// See [`clausify_all_in`].
    pub fn clausify_all(&self) -> Vec<String> {
        clausify_all_in(&self.layer, self.axiom_ids_set().into_iter().collect())
    }

    /// See [`clausify_formula_in`].
    pub fn clausify_formula(&self, kif: &str) -> crate::DiagResult<Vec<String>> {
        clausify_formula_in(&self.layer, kif)
    }

    /// See [`matching_axioms_in`].
    pub fn matching_axioms(&self, asts: Vec<crate::AstNode>, opts: &MatchOpts) -> Vec<SentenceId> {
        matching_axioms_in(
            &self.layer,
            self.axiom_ids_set().into_iter().collect(),
            asts,
            opts,
        )
    }
}

#[cfg(feature = "external-prover")]
impl<S: crate::trans::HasTranslation + 'static>
    KnowledgeBase<crate::prover::ExternalProverLayer<ProverLayer<S>>>
{
    /// See [`clausify_all_in`] -- the native prover nested under the
    /// external layer does the clausifying.
    pub fn clausify_all(&self) -> Vec<String> {
        clausify_all_in(
            self.layer.inner_layer(),
            self.axiom_ids_set().into_iter().collect(),
        )
    }

    /// See [`clausify_formula_in`].
    pub fn clausify_formula(&self, kif: &str) -> crate::DiagResult<Vec<String>> {
        clausify_formula_in(self.layer.inner_layer(), kif)
    }

    /// See [`matching_axioms_in`].
    pub fn matching_axioms(&self, asts: Vec<crate::AstNode>, opts: &MatchOpts) -> Vec<SentenceId> {
        matching_axioms_in(
            self.layer.inner_layer(),
            self.axiom_ids_set().into_iter().collect(),
            asts,
            opts,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kb_from(kif: &str) -> KnowledgeBase<ProverLayer> {
        let mut kb = KnowledgeBase::new_native();
        let r = kb.reload_kif(kif, &std::path::PathBuf::from("base.kif"), "load");
        assert!(r.ok, "fixture ingest failed: {:?}", r.diagnostics);
        kb.make_session_axiomatic("load").expect("promote");
        kb
    }

    #[test]
    fn ground_fact_round_trips() {
        let kb = kb_from("(instance Rex Dog)");
        let clauses = kb.clausify_all();
        assert_eq!(clauses, vec!["(instance Rex Dog)".to_string()]);
    }

    #[test]
    fn disjunction_becomes_or_clause() {
        let kb = kb_from("(=> (instance ?X Dog) (instance ?X Animal))");
        let clauses = kb.clausify_all();
        assert_eq!(clauses.len(), 1);
        assert!(clauses[0].starts_with("(or "), "got: {}", clauses[0]);
        assert!(clauses[0].contains("(not (instance"));
        assert!(clauses[0].contains("(instance"));
    }

    #[test]
    fn existential_gets_readable_skolem_name() {
        let kb = kb_from("(=> (instance ?X Dog) (exists (?Y) (owner ?Y ?X)))");
        let clauses = kb.clausify_all();
        assert_eq!(clauses.len(), 1);
        assert!(clauses[0].contains("SkFn1"), "got: {}", clauses[0]);
        assert!(!clauses[0].contains("sk_"), "got: {}", clauses[0]);
    }

    #[test]
    fn single_formula_scratch_clausify() {
        let kb = kb_from("(instance Rex Dog)");
        let clauses = kb
            .clausify_formula("(=> (instance ?X Cat) (instance ?X Animal))")
            .expect("well-formed KIF");
        assert_eq!(clauses.len(), 1);
        assert!(clauses[0].starts_with("(or "), "got: {}", clauses[0]);
        // The base KB's own axioms are untouched by a scratch clausify.
        assert_eq!(kb.clausify_all(), vec!["(instance Rex Dog)".to_string()]);
    }

    const MATCH_FIXTURE: &str = "(instance Rex Dog)
        (=> (instance ?X Dog) (instance ?X Animal))
        (=> (instance ?X Cat) (attribute ?X Furry))
        (subclass Dog Mammal)";

    fn ranked_matches(
        kb: &KnowledgeBase<ProverLayer>,
        query: &str,
        opts: MatchOpts,
    ) -> Vec<String> {
        let asts = crate::prover::Conjecture::parse(query, crate::Parser::Kif { options: None })
            .expect("well-formed query");
        kb.matching_axioms(asts, &opts)
            .into_iter()
            .map(|sid| kb.sentence_kif_str(sid))
            .collect()
    }

    fn matches(kb: &KnowledgeBase<ProverLayer>, query: &str, opts: MatchOpts) -> Vec<String> {
        let mut out = ranked_matches(kb, query, opts);
        out.sort();
        out
    }

    fn opts(mode: MatchMode, polarity: MatchPolarity) -> MatchOpts {
        MatchOpts { mode, polarity }
    }

    const REX_DOG: &str = "(instance Rex Dog)";
    const DOG_ANIMAL: &str = "(=> (instance ?X Dog) (instance ?X Animal))";
    const CAT_FURRY: &str = "(=> (instance ?X Cat) (attribute ?X Furry))";

    #[test]
    fn unify_ignoring_polarity_finds_every_mention() {
        let kb = kb_from(MATCH_FIXTURE);
        assert_eq!(
            matches(&kb, "(instance ?Z Dog)", MatchOpts::default()),
            vec![DOG_ANIMAL, REX_DOG]
        );
    }

    #[test]
    fn unify_respects_requested_polarity() {
        let kb = kb_from(MATCH_FIXTURE);
        let q = "(instance ?Z Dog)";
        assert_eq!(
            matches(&kb, q, opts(MatchMode::Unify, MatchPolarity::Same)),
            vec![REX_DOG]
        );
        assert_eq!(
            matches(&kb, q, opts(MatchMode::Unify, MatchPolarity::Opposite)),
            vec![DOG_ANIMAL]
        );
    }

    #[test]
    fn unify_binds_axiom_variables_but_instances_keeps_them_rigid() {
        let kb = kb_from(MATCH_FIXTURE);
        let q = "(instance Rex ?C)";
        assert_eq!(
            matches(&kb, q, MatchOpts::default()),
            vec![CAT_FURRY, DOG_ANIMAL, REX_DOG]
        );
        assert_eq!(
            matches(&kb, q, opts(MatchMode::Instances, MatchPolarity::Ignore)),
            vec![REX_DOG]
        );
    }

    #[test]
    fn instances_with_same_polarity() {
        let kb = kb_from(MATCH_FIXTURE);
        let q = "(instance ?Z Dog)";
        assert_eq!(
            matches(&kb, q, opts(MatchMode::Instances, MatchPolarity::Ignore)),
            vec![DOG_ANIMAL, REX_DOG]
        );
        assert_eq!(
            matches(&kb, q, opts(MatchMode::Instances, MatchPolarity::Same)),
            vec![REX_DOG]
        );
    }

    #[test]
    fn generalizations_find_more_general_axioms() {
        let kb = kb_from(MATCH_FIXTURE);
        let q = "(=> (instance Rex Dog) (instance Rex Animal))";
        assert_eq!(
            matches(
                &kb,
                q,
                opts(MatchMode::Generalizations, MatchPolarity::Same)
            ),
            vec![DOG_ANIMAL]
        );
        assert_eq!(
            matches(
                &kb,
                q,
                opts(MatchMode::Generalizations, MatchPolarity::Ignore)
            ),
            vec![DOG_ANIMAL, REX_DOG]
        );
    }

    #[test]
    fn matching_axioms_ignores_unpromoted_and_unrelated() {
        let mut kb = kb_from(MATCH_FIXTURE);
        let r = kb.reload_kif(
            "(instance Tom Dog)",
            &std::path::PathBuf::from("s.kif"),
            "s",
        );
        assert!(r.ok, "{:?}", r.diagnostics);
        assert_eq!(
            matches(
                &kb,
                "(instance ?Z Dog)",
                opts(MatchMode::Unify, MatchPolarity::Same)
            ),
            vec![REX_DOG]
        );
        assert!(matches(&kb, "(part Rex Earth)", MatchOpts::default()).is_empty());
    }

    #[test]
    fn query_is_clausified_as_an_axiom_not_a_goal() {
        let kb = kb_from(MATCH_FIXTURE);
        let same = opts(MatchMode::Instances, MatchPolarity::Same);
        // Free and explicitly universal variables are both pattern variables.
        assert_eq!(matches(&kb, "(instance ?Z Dog)", same), vec![REX_DOG]);
        assert_eq!(
            matches(&kb, "(forall (?Z) (instance ?Z Dog))", same),
            vec![REX_DOG]
        );
        // `?Z` becomes a Skolem constant, which no axiom mentions.
        assert!(matches(&kb, "(exists (?Z) (instance ?Z Dog))", same).is_empty());
        // A negated query keeps its sign.
        assert_eq!(
            matches(&kb, "(not (instance Rex Dog))", same),
            Vec::<String>::new()
        );
        assert_eq!(
            matches(&kb, "(not (instance ?Z Dog))", same),
            vec![DOG_ANIMAL]
        );
    }

    #[test]
    fn matching_axioms_on_empty_query_is_empty() {
        let kb = kb_from(MATCH_FIXTURE);
        assert!(kb
            .matching_axioms(Vec::new(), &MatchOpts::default())
            .is_empty());
    }

    #[test]
    fn matches_rank_by_query_symbols_then_kbo_weight() {
        const REX_ANIMAL: &str = "(instance Rex Animal)";
        const DOG_PET_ANIMAL: &str =
            "(=> (and (instance ?X Dog) (instance ?X Pet)) (instance ?X Animal))";
        const INHERIT: &str = "(=> (and (subclass ?S ?C) (instance ?X ?S)) (instance ?X ?C))";
        let kb = kb_from(&[REX_DOG, REX_ANIMAL, DOG_ANIMAL, DOG_PET_ANIMAL, INHERIT].join("\n"));
        // `INHERIT` contains neither `Rex` nor `Animal`.
        assert_eq!(
            ranked_matches(&kb, "(instance Rex Animal)", MatchOpts::default()),
            vec![REX_ANIMAL, DOG_ANIMAL, DOG_PET_ANIMAL, INHERIT]
        );
    }

    #[test]
    fn clausify_formula_reports_malformed_input_as_an_error() {
        let kb = kb_from("(subclass Cat Animal)");
        let err = kb
            .clausify_formula("(=> (instance ?X")
            .expect_err("unbalanced");
        assert_eq!(err.code, "clausify-input");
    }
}
