//! `KnowledgeBase<ProverLayer>::clausify_*` — CNF form of the KB (or a
//! single ad hoc formula), rendered back to flat SUO-KIF, via the native
//! prover's own clausifier (`prover::saturate::clausify`). Native-only: the
//! clause representation (`PClause`/`AtomTable`) only exists under
//! `native-prover`.

#![cfg(feature = "native-prover")]

use crate::layer::TopLayer;
use crate::prover::saturate::render::{clause_to_kif, SkolemNames};
use crate::prover::saturate::ProverLayer;
use crate::types::SentenceId;

use super::KnowledgeBase;

impl<S: TopLayer + 'static> KnowledgeBase<ProverLayer<S>> {
    /// Clausify every axiom currently loaded and render each resulting
    /// clause as flat SUO-KIF (`(or lit1 lit2 ...)`, bare literal for unit
    /// clauses). One entry per clause; axioms are visited in `SentenceId`
    /// order for determinism. Skolem symbols introduced during
    /// clausification are renamed `SkFnN`/`SkCN`, consistently across the
    /// whole call.
    pub fn clausify_all(&self) -> Vec<String> {
        let mut roots: Vec<SentenceId> = self.axiom_ids_set().into_iter().collect();
        roots.sort_unstable();
        let syn = &self.layer.semantic().syntactic;
        let mut sk = SkolemNames::default();
        roots
            .iter()
            .flat_map(|&root| {
                let clauses = self.layer.clauses_for(root);
                clauses
                    .iter()
                    .map(|c| clause_to_kif(c, &self.layer.atoms, syn, &mut sk))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Clausify a single ad hoc KIF formula (not pulled from the KB store)
    /// and render its clauses as flat SUO-KIF. Returns an empty vec on a
    /// parse error or a formula that clausifies to nothing.
    pub fn clausify_formula(&self, kif: &str) -> Vec<String> {
        let doc = crate::parse_document("clausify", kif.to_string(), crate::Parser::Kif);
        if doc.has_errors() {
            return Vec::new();
        }
        let asts: Vec<crate::AstNode> = doc
            .ast
            .into_iter()
            .filter_map(|d| d.as_stmt().cloned())
            .collect();
        let clauses = self.layer.clausify_asts(asts);
        let syn = &self.layer.semantic().syntactic;
        let mut sk = SkolemNames::default();
        clauses
            .iter()
            .map(|c| clause_to_kif(c, &self.layer.atoms, syn, &mut sk))
            .collect()
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
        let clauses = kb.clausify_formula("(=> (instance ?X Cat) (instance ?X Animal))");
        assert_eq!(clauses.len(), 1);
        assert!(clauses[0].starts_with("(or "), "got: {}", clauses[0]);
        // The base KB's own axioms are untouched by a scratch clausify.
        assert_eq!(kb.clausify_all(), vec!["(instance Rex Dog)".to_string()]);
    }
}
