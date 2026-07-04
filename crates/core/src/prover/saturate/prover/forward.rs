// crates/core/src/prover/saturate/prover/forward.rs
//
// Bounded hyperresolution forward closure (support units x background
// clauses) and Phase-6 background completion (Knuth-Bendix-style unit-
// equation saturation) -- both run ONCE before the main given-clause
// loop, over the pre-activated background.

use std::time::Instant;

use crate::types::SentenceId;

use super::super::clause::{AtomId, Term};
use super::super::oracle::Witness;
use super::super::theory::TheoryOracle;
use super::super::unify::{apply, shift_slots, slot_atom, unify, Subst};
use super::{positions, term_binary_ids, term_depth, term_kif, witnesses_kif, NativeProver, JOIN_UNIT_OFF, SUPPORT};

impl<'a> NativeProver<'a> {
    /// Is clause `id` an activated, KBO-orientable positive unit equality
    /// — i.e. a demodulator that completion can superpose with?
    fn is_unit_equation(&self, id: u32) -> bool {
        let c = &self.clauses[id as usize];
        c.activated
            && !c.retired
            && c.terms.len() == 1
            && c.terms[0].0
            && self.equality_oriented(&c.terms[0].1).is_some()
    }

    /// The activated unit-equation clause ids, in arena (deterministic)
    /// order — completion's working set.
    fn unit_equation_ids(&self) -> Vec<u32> {
        (0..self.clauses.len() as u32)
            .filter(|&id| self.is_unit_equation(id))
            .collect()
    }

    /// Phase 6 — bounded background completion (Knuth–Bendix-style).  Run
    /// ONCE before the main loop: superpose the active unit equations
    /// against each other, keeping every *new* oriented unit equation as a
    /// demodulator, to a hard budget (completion can diverge; the budget
    /// is the terminator).  The payoff is that proof-time equational
    /// rewriting becomes cheap one-way demodulation against this richer,
    /// closer-to-confluent rule set instead of repeated live superposition.
    /// Sound: every product is `superpose`'d from two equational parents,
    /// so it is an equational consequence of the background.  Gated by
    /// `Strategy.bg_completion`; deterministic for a fixed input.
    pub(crate) fn complete_background(&mut self) {
        if !self.opts.strategy.bg_completion {
            return;
        }
        let budget = self.opts.strategy.bg_completion_budget.max(1);
        let mut produced = 0usize;
        let mut attempts = 0usize;
        let hard = budget.saturating_mul(16); // attempt backstop
        // LIFO frontier of equation ids still to superpose against the set;
        // newly derived equations join it (the closure's fixpoint engine).
        let mut frontier: Vec<u32> = self.unit_equation_ids();
        while let Some(eid) = frontier.pop() {
            if produced >= budget || attempts >= hard {
                break;
            }
            if !self.is_unit_equation(eid) {
                continue;
            }
            // `eid`'s oriented larger side rewrites the partners' subterms.
            let partners = self.unit_equation_ids();
            'partners: for p in partners {
                if p == eid {
                    continue;
                }
                let Some(p_atom) =
                    slot_atom(&self.layer.atoms, self.syn(), self.clauses[p as usize].lits[0].atom, 0)
                else { continue };
                for (path, _sub) in positions(&p_atom) {
                    attempts += 1;
                    if attempts >= hard {
                        break 'partners;
                    }
                    let Some(nid) = self.superpose(eid, 0, p, 0, &path) else { continue };
                    // Keep only genuinely new oriented unit equations.
                    let key = self.clauses[nid as usize].key;
                    if self.is_unit_equation_unactivated(nid) && self.seen.insert(key) {
                        self.activate(nid);
                        frontier.push(nid);
                        produced += 1;
                        if produced >= budget {
                            break 'partners;
                        }
                    }
                }
            }
        }
        self.stats.bg_completed = produced as u64;
    }

    /// Like `is_unit_equation` but for a freshly `make`'d (not-yet
    /// activated) clause — completion's acceptance test for a product.
    fn is_unit_equation_unactivated(&self, id: u32) -> bool {
        let c = &self.clauses[id as usize];
        c.terms.len() == 1
            && c.terms[0].0
            && self.equality_oriented(&c.terms[0].1).is_some()
    }

    /// Bounded hyperresolution: support units × background clauses,
    /// joining all remaining negative literals against active positive
    /// units (or the oracle).  Only FLAT ground unit conclusions are
    /// kept — the problem-specific forward closure, without flooding.
    pub(crate) fn forward_close(&mut self) -> usize {
        let fc_start = Instant::now();
        // Copied out: the loop below borrows `self` mutably.
        let st = &self.opts.strategy;
        let (fc_rounds, fc_max_premise_lits, fc_flat_depth, fc_fanout, fc_cap, fc_branch, fc_max_pos) = (
            st.fc_rounds, st.fc_max_premise_lits, st.fc_flat_depth,
            st.fc_fanout, st.fc_cap, st.fc_branch, st.fc_max_pos.max(1),
        );
        let instance = self.oracle.roles().instance;
        let mut units: Vec<(AtomId, u32)> = self
            .support_seeds
            .clone()
            .into_iter()
            .filter(|(a, _)| {
                self.layer.atoms.resolve(*a, self.syn())
                    .and_then(|s| s.head_symbol()) != Some(instance)
            })
            .collect();
        let mut total = 0usize;
        for _ in 0..fc_rounds {
            let mut nxt: Vec<(AtomId, u32)> = Vec::new();
            'units: for (u_atom, u_cid) in &units {
                let u_info = self.layer.atom_info(*u_atom);
                let layer = self.layer;
                let src = move |a| layer.atom_info(a);
                let candidates = self.idx.complementary(true, &u_info, &src);
                let Some(u_term) = slot_atom(&self.layer.atoms, self.syn(), *u_atom, 0)
                else { continue };
                for at in candidates {
                    let (c_id, c_i) = (at.clause, at.lit as usize);
                    let (c_terms, c_nvars, c_npos) = {
                        let c = &self.clauses[c_id as usize];
                        if c.retired { continue; } // superseded by bwd-demod replacement
                        if c.lits.len() > fc_max_premise_lits || c.lits[c_i].pos { continue; }
                        (c.terms.clone(), c.nvars,
                         c.terms.iter().filter(|(p, _)| *p).count())
                    };
                    if c_npos < 1 || c_npos > fc_max_pos { continue; }
                    let off = 1u64; // unit is ground: no slots of its own
                    let mut s: Subst = vec![None; (off + u64::from(c_nvars) + 1) as usize];
                    let p_lit = shift_slots(&c_terms[c_i].1, off);
                    self.stats.fc_unify_attempts += 1;
                    self.stats.fc_ground_candidate += 1; // seed unit is ground by construction
                    if !unify(&p_lit, &u_term, &mut s) { continue; }
                    self.stats.fc_unify_hits += 1;
                    let negs: Vec<Term> = c_terms.iter().enumerate()
                        .filter(|(k, (p, _))| *k != c_i && !*p)
                        .map(|(_, (_, t))| shift_slots(t, off))
                        .collect();
                    // ALL positive heads (a unit for a Horn rule; a short
                    // disjunction for a multi-conclusion rule) — the
                    // conclusion is their σ-applied disjunction.
                    let pos_terms: Vec<Term> = c_terms.iter()
                        .filter(|(p, _)| *p)
                        .map(|(_, t)| shift_slots(t, off))
                        .collect();

                    let mut got = 0usize;
                    let mut stack: Vec<(usize, Subst, Vec<SentenceId>, Vec<u32>, Vec<String>)> =
                        vec![(0, s, Vec::new(), Vec::new(), Vec::new())];
                    while let Some((k, s2, facts, used, jnotes)) = stack.pop() {
                        if k == negs.len() {
                            // The conclusion is the disjunction of all
                            // positive heads, σ-applied — every head must be
                            // a flat ground atom (the anti-flooding contract).
                            let atoms: Vec<Term> =
                                pos_terms.iter().map(|t| apply(t, &s2)).collect();
                            if atoms.iter().any(|a| {
                                !a.is_ground() || term_depth(a) > fc_flat_depth
                            }) {
                                continue;
                            }
                            let mut parents = vec![c_id, *u_cid];
                            parents.extend(used.iter().copied());
                            let lits: Vec<(bool, Term)> =
                                atoms.into_iter().map(|a| (true, a)).collect();
                            let made =
                                self.make(lits, parents, "hyper", SUPPORT, None, true);
                            let Some(cid) = made else { continue };
                            self.clauses[cid as usize].fact_parents.extend(facts.iter().copied());
                            self.clauses[cid as usize].notes.extend(jnotes.iter().cloned());
                            if self.clauses[cid as usize].lits.is_empty() {
                                // The joined conclusion was refuted
                                // outright (arithmetic / oracle
                                // discharge).  Queue it only if it is
                                // a reportable refutation.
                                if let Some(e) = self.reportable_refutation(cid) {
                                    self.push(Some(e));
                                }
                                continue;
                            }
                            let key = self.clauses[cid as usize].key;
                            if !self.seen.insert(key) { continue; }
                            self.activate(cid);
                            // Only UNIT conclusions re-seed the unit-driven
                            // next round; a derived disjunction can't.
                            if self.clauses[cid as usize].lits.len() == 1 {
                                let new_atom = self.clauses[cid as usize].lits[0].atom;
                                nxt.push((new_atom, cid));
                            }
                            total += 1;
                            got += 1;
                            if got >= fc_fanout || total >= fc_cap { break; }
                            continue;
                        }
                        let a = apply(&negs[k], &s2);
                        // Oracle discharge of a ground joined literal.
                        if a.is_ground() {
                            if let Some((rel, x, y)) = term_binary_ids(&a) {
                                if self.oracle.holds(rel, x, y, None) {
                                    let mut why: Vec<Witness> = Vec::new();
                                    let _ = self.oracle.holds(rel, x, y, Some(&mut why));
                                    let mut facts2 = facts.clone();
                                    let mut used2  = used.clone();
                                    for w in &why {
                                        if let Some(sid) = w.sid {
                                            facts2.push(sid);
                                        } else if let Some(cid) =
                                            self.oracle.learned_src(w.rel, w.x, w.y)
                                        {
                                            used2.push(cid);
                                        }
                                    }
                                    let mut jn = jnotes.clone();
                                    jn.push(format!(
                                        "(not {}) -- oracle: {}",
                                        term_kif(&a, self.syn()),
                                        witnesses_kif(&why, self.syn())));
                                    stack.push((k + 1, s2.clone(), facts2, used2, jn));
                                    continue;
                                }
                            }
                        }
                        // Join against active positive units via the index.
                        let qa = self.layer.atoms.intern_atom(&a);
                        let q_info = self.layer.atom_info(qa);
                        let cands = self.idx.probe(true, &q_info, &src);
                        let mut branch = 0usize;
                        for cand in cands {
                            let uc = &self.clauses[cand.clause as usize];
                            if uc.lits.len() != 1 || uc.retired { continue; }
                            // Two-way unification binds the unit's vars
                            // too, so (unlike the one-way matches) the
                            // substitution must cover its slot range.
                            let Some(u2) = slot_atom(
                                &self.layer.atoms, self.syn(), uc.lits[0].atom,
                                JOIN_UNIT_OFF as u32)
                            else { continue };
                            let mut s3 = s2.clone();
                            s3.resize((JOIN_UNIT_OFF + 257) as usize, None);
                            self.stats.fc_unify_attempts += 1;
                            if uc.nvars == 0 { self.stats.fc_ground_candidate += 1; }
                            if unify(&a, &u2, &mut s3) {
                                self.stats.fc_unify_hits += 1;
                                let mut used2 = used.clone();
                                used2.push(cand.clause);
                                stack.push((k + 1, s3, facts.clone(), used2, jnotes.clone()));
                                branch += 1;
                                if branch >= fc_branch { break; }
                            }
                        }
                    }
                    if total >= fc_cap { break 'units; }
                }
            }
            self.stats.forward_closed = total as u64;
            units = nxt;
            if units.is_empty() || total >= fc_cap { break; }
            // Wall-clock insurance: fc has its own caps, but theory
            // feedback (lists, FD, exhaustiveness) can make rounds
            // expensive at full-KB scale.
            if self.opts.cancelled()
                || (!self.opts.step
                    && self.opts.time_limit_secs > 0
                    && fc_start.elapsed().as_secs() >= self.opts.time_limit_secs.div_ceil(4))
            {
                break;
            }
        }
        total
    }
}
