// crates/core/src/prover/saturate/prover/schema_apply.rs
//
// Schema-channel pre-pass: recognize theory-rule shapes (symmetry,
// transitivity, antisymmetry, irreflexivity, inverse pairs, equality
// substitution) in input clauses and register them with the oracle's
// matching theory registries -- shared by the pre-pass (`mine_schema`)
// and `make`'s post-canonicalization re-probe (both call
// `apply_schema_hit`).

use crate::types::SentenceId;

use super::super::clause::PClause;
use super::super::schema::{SchemaHit, SchemaKind};
use super::NativeProver;

impl<'a> NativeProver<'a> {
    /// Schema-channel pre-pass: probe every input clause of a root
    /// against the pattern table and register what it states (mined
    /// symmetric / transitive / antisymmetric / irreflexive relations,
    /// inverse pairs).  Registration only — absorption happens when the
    /// same clause flows through `make`, which re-probes.  Running this
    /// over ALL roots before any clause is made means orientation and
    /// the oracle closures are active from the first input clause.
    pub(crate) fn mine_schema(&mut self, clauses: &[PClause], root: SentenceId) {
        if !self.opts.strategy.schema {
            return;
        }
        for pc in clauses {
            if pc.lits.len() > 4 || pc.nvars == 0 {
                continue;
            }
            if let Some(hit) = self.layer.schema.probe(&pc.lits, &self.layer.atoms, self.syn()) {
                self.apply_schema_hit(&hit, Some(root));
            }
        }
    }

    /// Act on a verified schema hit: register with the matching theory
    /// registry, and say whether the clause should be ABSORBED (dropped
    /// — its inferential role fully replaced).  Absorption is earned
    /// per pattern:
    ///
    /// * Symmetry rule + symmetry metaschema: YES.  Ground orientation
    ///   collapses both argument orders to one canonical form; the
    ///   symmetric retrieval retry (`resolve`) and the oracle's
    ///   reversed-edge check cover open literals and stored facts.
    /// * Transitivity (rule AND metaschema): NO.  The oracle closure
    ///   discharges ground transitive queries, but saturation still
    ///   needs the clause to ENUMERATE compositions into open goals
    ///   (`¬R(a,?z)` has no closure analogue) — absorbing it would be
    ///   an enumeration-completeness hole.  Registration alone buys the
    ///   ground discharges.
    /// * Antisymmetry / irreflexivity / inverse: NO — recognized and
    ///   recorded; their consumers land separately.
    pub(super) fn apply_schema_hit(&mut self, hit: &SchemaHit, source: Option<SentenceId>) -> bool {
        let trace = std::env::var_os("SIGMA_ORACLE_TRACE").is_some();
        match hit.kind {
            SchemaKind::Symmetry => {
                let Some(rel) = &hit.rel else { return false };
                if trace {
                    eprintln!("SCHEMA symmetric {}", rel.name());
                }
                self.stats.mined_symmetric += 1;
                self.oracle.register_symmetric(rel.id(), source);
                true
            }
            SchemaKind::SymMetaschema => {
                if trace {
                    eprintln!("SCHEMA symmetry-metaschema absorbed");
                }
                true
            }
            SchemaKind::EqSubstitution => {
                // Substitution of equals is what paramodulation and the
                // ground-equality congruence closure (normalize_eq,
                // compound equality keys, FD pipeline) already do; the
                // axiomatic spelling only multiplies every equality
                // unit by every R-fact.  Absorb.
                if trace {
                    let name = hit.rel.as_ref().map(|r| r.name().to_string());
                    eprintln!("SCHEMA eq-substitution absorbed ({name:?})");
                }
                true
            }
            SchemaKind::Transitivity => {
                let Some(rel) = &hit.rel else { return false };
                if trace {
                    eprintln!("SCHEMA transitive {}", rel.name());
                }
                self.stats.mined_transitive += 1;
                self.oracle.register_transitive(rel.id(), source);
                false
            }
            SchemaKind::TransMetaschema => false,
            SchemaKind::Antisymmetry => {
                if let Some(rel) = &hit.rel {
                    self.stats.mined_other += 1;
                    self.antisym_mined.entry(rel.id()).or_insert(source);
                }
                false
            }
            SchemaKind::Irreflexivity => {
                if let Some(rel) = &hit.rel {
                    self.stats.mined_other += 1;
                    self.irrefl_mined.entry(rel.id()).or_insert(source);
                }
                false
            }
            SchemaKind::Inverse => {
                if let (Some(r1), Some(r2)) = (&hit.rel, &hit.rel2) {
                    self.stats.mined_other += 1;
                    self.inverse_mined.push((r1.id(), r2.id(), source));
                }
                false
            }
        }
    }
}
