// crates/core/src/kb/cnf.rs
//
// CNF clausification interfaceß for KB

use crate::{cnf::sentence_to_clauses, types::Clause};
#[cfg(feature = "cnf")]
use crate::SentenceId;

use super::KnowledgeBase;
use super::error::KbError;

impl KnowledgeBase {
    /// Clausify all current axioms and session assertions into the clauses side-car.
    ///
    /// In the Phase-5 pipeline the side-car is populated opportunistically
    /// by `tell` / `load_kif`, so this method is mostly idempotent --
    /// it forces re-clausification of any sentence that isn't already
    /// cached and reports the count.  Skolem symbols discovered by the
    /// Vampire clausifier are interned directly into the `SyntacticLayer` by
    /// `cnf::sentence_to_clauses`, so the method no longer needs an
    /// out-parameter for new symbols.
    #[cfg(feature = "cnf")]
    pub fn clausify(&mut self) -> Result<ClausifyReport, KbError> {

        let mut report = ClausifyReport::default();

        // Collect all SIDs to clausify (axioms + all session assertions).
        let axiom_ids = self.axiom_ids_set();
        let mut all_sids: Vec<SentenceId> = axiom_ids.into_iter().collect();
        for sids in self.sessions.values() { all_sids.extend(sids.iter().copied()); }

        for sid in all_sids {
            if self.clauses.contains_key(&sid) {
                report.clausified += 1;
                continue;
            }
            match crate::cnf::sentence_to_clauses(&mut self.layer, sid) {
                Ok(clauses) => {
                    self.clauses.insert(sid, clauses);
                    report.clausified += 1;
                }
                Err(e) => {
                    self.warn(format!("clausify: sid={} failed: {}", sid, e));
                    report.exceeded_limit.push(sid);
                    report.skipped += 1;
                }
            }
        }

        self.info(format!("clausify: {} clausified, {} skipped", report.clausified, report.skipped));
        Ok(report)
    }

    /// Clausify `sid`, derive the canonical formula hash, and return the
    /// hash alongside the (cached) clause list.  Returns `None` if
    /// clausification failed -- the caller should treat that as "skip
    /// dedup, accept the sentence".
    ///
    /// The clause list is `Vec<Clause>` so callers that accept the
    /// sentence can stash it in `self.clauses` without recomputing it
    /// later at promote time.
    #[allow(dead_code)] // callers are persist-feature gated
    pub(super) fn compute_formula_hash(&mut self, sid: SentenceId) -> Option<(u64, Vec<Clause>)> {
        use crate::canonical::{canonical_clause_hash, formula_hash_from_clauses};

        // `cnf::sentence_to_clauses` borrows the semantic layer mutably to
        // intern new skolem/wrapper symbols into the SyntacticLayer.
        let clauses = match sentence_to_clauses(&mut self.layer, sid) {
            Ok(cs)  => cs,
            Err(e)  => {
                self.warn(format!("compute_formula_hash: sid={} clausify failed: {}", sid, e));
                return None;
            }
        };
        let canonical: Vec<u64> = clauses
            .iter()
            .map(canonical_clause_hash)
            .collect();
        let fh = formula_hash_from_clauses(&canonical);
        Some((fh, clauses))
    }

    /// True when `symbol` is a Skolem function introduced by the
    /// CNF clausifier.  Exposed so workspace-symbol search can
    /// filter these out by default.  O(1) -- name -> id -> Symbol
    /// via the intern table + `sym_idx`.
    pub fn symbol_is_skolem(&self, symbol: &str) -> bool {
        self.symbol_id(symbol)
            .and_then(|id| self.layer.semantic.syntactic.symbol_of(id))
            .map(|s| s.is_skolem)
            .unwrap_or(false)
    }

    // -- Batched clausification with bisection-based recovery -------------------
    //
    // The batched clausify path (`cnf::clausify_sentences_batch`) sends the
    // whole batch through one Vampire call — much cheaper than N per-sentence
    // calls.  The failure mode is also whole-batch: if one sentence triggers
    // a C++ exception in NewCNF, the entire batch returns `Err`.
    //
    // To preserve the per-sentence isolation of the pre-batch code, we wrap
    // the batch call in bisection: on failure, split the sid list in half
    // and recurse.  In the worst case (one bad sid in a batch of N) this
    // does O(log N) batch retries before isolating the bad sentence.  For
    // a 15,000-sentence bootstrap with 3 bad sentences that's ~45 batch
    // retries — still far fewer than 15,000 per-sentence calls in the old
    // code, and only in the (rare) failure path.
    //
    // Sentences that are isolated as individually-failing come back in the
    // `skipped` list; callers in `ingest()` then treat them as "accept
    // without dedup" to match the pre-batch fallback.
    #[cfg(feature = "cnf")]
    pub(super) fn clausify_with_bisection(
        &self,
        sids:  &[SentenceId],
    ) -> crate::cnf::BatchedSentenceClauses {
        use crate::cnf::BatchedSentenceClauses;
        use std::collections::HashMap;

        // Base case: empty slice — nothing to clausify.
        if sids.is_empty() {
            return BatchedSentenceClauses {
                by_sid:  HashMap::new(),
                shared:  Vec::new(),
                skipped: Vec::new(),
            };
        }

        match crate::cnf::clausify_sentences_batch(&self.layer, sids) {
            Ok(batched) => batched,
            Err(e) if sids.len() == 1 => {
                // Base case: single bad sentence.  Record it as skipped
                // so the caller falls back to "accept without dedup".
                self.warn(format!("ingest: clausify failed for sid={}: {}; will accept without dedup", sids[0], e));
                BatchedSentenceClauses {
                    by_sid:  HashMap::new(),
                    shared:  Vec::new(),
                    skipped: vec![sids[0]],
                }
            }
            Err(_) => {
                // Split and recurse.  Log at info level so the
                // bisection walk is visible on bootstrap debugging but
                // doesn't clutter normal output.
                let mid = sids.len() / 2;
                self.info(format!("ingest: batch clausify failed for {} sids; bisecting ({}/{})", sids.len(), mid, sids.len() - mid));
                let left  = self.clausify_with_bisection(&sids[..mid]);
                let right = self.clausify_with_bisection(&sids[mid..]);
                merge_batched(left, right)
            }
        }
    }
}

pub struct ClausifyOptions {
    pub max_clauses_per_formula: usize,
}

impl Default for ClausifyOptions {
    fn default() -> Self { Self { max_clauses_per_formula: 1000 } }
}

#[cfg(feature = "cnf")]
impl KnowledgeBase {
    /// Turn on opportunistic clausification during `tell` / `load_kif`
    /// and store the per-sentence clauses in the side-car.  Call
    /// [`Self::clausify`] afterwards to drain any sentences that were
    /// added before this method ran.
    pub fn enable_cnf(&mut self, opts: ClausifyOptions) {
        self.cnf_mode = true;
        self.cnf_opts = opts;
    }

    /// Turn off opportunistic clausification.  Existing clauses in
    /// the side-car are kept; subsequent `tell` / `load_kif` calls
    /// will not clausify automatically.  [`Self::clausify`] still
    /// works on demand.
    pub fn disable_cnf(&mut self) {
        self.cnf_mode = false;
    }
}

#[derive(Debug, Default)]
pub struct ClausifyReport {
    pub clausified:      usize,
    pub skipped:         usize,
    pub exceeded_limit:  Vec<SentenceId>,
}

#[cfg(feature = "cnf")]
fn merge_batched(
    mut a: crate::cnf::BatchedSentenceClauses,
    b:     crate::cnf::BatchedSentenceClauses,
) -> crate::cnf::BatchedSentenceClauses {
    a.by_sid.extend(b.by_sid);
    a.shared.extend(b.shared);
    a.skipped.extend(b.skipped);
    a
}
