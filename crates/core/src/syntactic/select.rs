// crates/core/src/syntactic/select.rs
//
// Axiom-selection primitives on the `SyntacticLayer` — the SInE-relevance
// reads that the prover backends consume.  These were `KnowledgeBase`
// methods, but proving moved onto the layer (`ProvingLayer::prove_once`
// sees `self = layer`, not the KB), and every one reads only the syntactic
// index + sentence store, so they belong here.  They take a `&ProveCtx`
// for the phase spans / log lines they used to emit through the KB sink.
//
// `KnowledgeBase` keeps thin `&self` wrappers (kb/sine.rs) that forward
// here so the CLI / `serve` / audit callers are unaffected; the layer
// `prove_once` calls them directly off `self.semantic().syntactic`.

#[cfg(feature = "ask")]
use std::collections::HashMap;
use std::collections::HashSet;

use crate::profile_span;
use crate::progress::ProveCtx;
use crate::types::Element;
use crate::{SentenceId, SineParams, SymbolId};

use super::SyntacticLayer;

/// Backend-agnostic knobs for the composed relevance pass
/// ([`SyntacticLayer::select_axioms`]).  The SInE tolerance/budget live in
/// [`SineParams`]; these are the augmentation toggles each prover sets from its
/// own opts (native: `Strategy`; external: defaults — it wants Liu too).
#[cfg(feature = "ask")]
#[derive(Debug, Clone, Copy)]
pub(crate) struct SelectionParams {
    /// Drop bookkeeping-head sentences (`documentation`, `termFormat`, …).
    pub head_filter: bool,
    /// Run the Liu & Xu structural rescue.
    pub liu_rescue:  bool,
    /// Liu rescue rounds.
    pub liu_rounds:  usize,
    /// Liu rescue additions per round.
    pub liu_top_k:   usize,
}

#[cfg(feature = "ask")]
impl Default for SelectionParams {
    fn default() -> Self {
        Self { head_filter: true, liu_rescue: true, liu_rounds: 1, liu_top_k: 32 }
    }
}

impl SyntacticLayer {
    /// The shared relevance pass both provers run: SInE-select the seed
    /// (or the whole axiom base under `select_all`), drop bookkeeping heads,
    /// then rescue goal-near axioms SInE's trigger relation missed (Liu & Xu).
    ///
    /// Returns `(selected, liu_frontier)` — `liu_frontier` is the rescue's
    /// additions (empty when Liu is off), which the native path feeds to
    /// definitional completion.  Each backend then applies its own augmentation
    /// (native: def-completion; external: synthetic-replacement / predvar /
    /// taxonomy-closure injection).
    #[cfg(feature = "ask")]
    pub(crate) fn select_relevant(
        &self,
        seed:   &HashSet<SymbolId>,
        params: SineParams,
        sel:    &SelectionParams,
        ctx:    &ProveCtx,
    ) -> (HashSet<SentenceId>, Vec<SentenceId>) {
        // SInE relevance, or the whole promoted base under `--full-kb`.
        let mut selected: HashSet<SentenceId> = if params.select_all {
            self.sine_current(|idx| idx.axiom_sids()).into_iter().collect()
        } else {
            self.sine_select_with_seed(seed.clone(), params, ctx)
        };
        // Strip SUMO bookkeeping predicates.
        if sel.head_filter {
            let raw: Vec<SentenceId> = selected.iter().copied().collect();
            selected = self.filter_excluded_heads(&raw).into_iter().collect();
        }
        // Liu & Xu structural rescue — goal-near axioms the rare-symbol trigger
        // relation circularly excluded.  (`structural_include` excludes
        // bookkeeping heads itself, so order vs. `head_filter` doesn't matter.)
        let mut liu_frontier = Vec::new();
        if sel.liu_rescue && !params.select_all {
            let extra = self.structural_include(seed, &selected, sel.liu_rounds, sel.liu_top_k);
            if !extra.is_empty() {
                selected.extend(extra.iter().copied());
                liu_frontier = extra;
            }
        }
        (selected, liu_frontier)
    }


    /// SInE-select the relevant axiom subset for a set of sentences already
    /// in the store — symbols read straight off the sids (no parse).
    pub(crate) fn sine_select_for_sids(
        &self,
        sids:   &[SentenceId],
        params: SineParams,
        ctx:    &ProveCtx,
    ) -> HashSet<SentenceId> {
        let seed = {
            profile_span!(ctx, "sine.collect_symbols");
            let mut s: HashSet<SymbolId> = HashSet::new();
            for &sid in sids {
                s.extend(self.sentence_symbols(sid));
            }
            s
        };
        self.sine_select_with_seed(seed, params, ctx)
    }

    /// Canonical SInE-selection entry point: given a pre-computed symbol
    /// seed, return the SentenceIds the index considers relevant at `params`.
    pub(crate) fn sine_select_with_seed(
        &self,
        seed:   HashSet<SymbolId>,
        params: SineParams,
        ctx:    &ProveCtx,
    ) -> HashSet<SentenceId> {
        profile_span!(ctx, "sine.select_axioms");
        // Auto-tolerance: pick the largest tolerance whose selected set stays
        // within the budget.  Otherwise honour the fixed tolerance.
        let (selected, effective_tol, mode) = match params.auto_budget {
            Some(budget) => {
                let (chosen, set) =
                    self.select_axioms_within_budget(&seed, budget, params.depth_limit);
                (set, chosen, format!("auto (budget {})", budget))
            }
            None => {
                let set = self.select_axioms(&seed, params.tolerance, params.depth_limit);
                (set, params.tolerance, "fixed".to_string())
            }
        };
        let axiom_count = self.sine_current(|idx| idx.axiom_count());
        ctx.info(format!(
            "sine_select: {} seed syms -> {} relevant axioms (of {} total) at tolerance {} [{}]",
            seed.len(), selected.len(), axiom_count, effective_tol, mode,
        ));
        selected
    }

    /// Filter a SentenceId list by the canonical default-excluded head
    /// predicates (`documentation`, `termFormat`, `domain`, …).
    pub(crate) fn filter_excluded_heads(&self, sids: &[SentenceId]) -> Vec<SentenceId> {
        let excluded = crate::kb::export::excluded_heads_set();
        sids.iter().copied()
            .filter(|&sid| {
                let Some(sentence) = self.sentence(sid) else { return true };
                match sentence.elements.first() {
                    Some(Element::Symbol(sym)) => !excluded.contains(&*sym.name()),
                    _ => true,
                }
            })
            .collect()
    }

    /// All SentenceIds currently promoted to axioms — roots whose producing
    /// session has been axiomatized (transient assertions and rolled-back
    /// ephemeral query tags excluded).
    pub(crate) fn axiom_ids_set(&self) -> HashSet<SentenceId> {
        self.root_sids().into_iter()
            .filter(|&sid| self.is_axiom(sid))
            .collect()
    }

    /// Structural-relevance inclusion (Liu & Xu, 2022, adapted): rescue
    /// axioms structurally close to the goal but invisible to SInE's trigger
    /// relation.  Ranks axioms by shared predicate-position symbols weighted
    /// by inverse generality; `rounds` iterates the expansion, each adding at
    /// most `top_k`.  Returns the additions; `selected` is not mutated.
    #[cfg(feature = "ask")]
    pub(crate) fn structural_include(
        &self,
        seed:     &HashSet<SymbolId>,
        selected: &HashSet<SentenceId>,
        rounds:   usize,
        top_k:    usize,
    ) -> Vec<SentenceId> {
        /// Symbols more general than this are hubs (`instance`, `subclass`,
        /// …): enumerating their axiom lists is quadratic noise.
        const HUB_CAP: usize = 1500;
        /// How far down the ranking to scan for acceptable candidates.
        const SCAN_CAP: usize = 16;

        let excluded = crate::kb::export::excluded_heads_set();

        // PREDICATE-position symbols of a root: heads of the sentence and of
        // every sub-sentence.
        let head_syms = |sid: SentenceId| -> HashSet<SymbolId> {
            let mut out = HashSet::new();
            let mut stack = vec![sid];
            while let Some(s) = stack.pop() {
                let Some(sent) = self.sentence(s) else { continue };
                if let Some(Element::Symbol(h)) = sent.elements.first() {
                    out.insert(h.id());
                }
                for el in sent.elements.iter() {
                    if let Element::Sub(sub) = el {
                        stack.push(*sub);
                    }
                }
            }
            out
        };

        let mut included: Vec<SentenceId> = Vec::new();
        let mut in_set: HashSet<SentenceId> = HashSet::new();
        let mut work: HashSet<SymbolId> = seed.clone();
        let mut seen_syms: HashSet<SymbolId> = seed.clone();

        let trace = std::env::var_os("SIGMA_LIU_TRACE").is_some();
        self.sine_current(|idx| {
            for round in 0..rounds {
                // Score every axiom sharing a non-hub work symbol, by inverse
                // generality (rare shared content ⇒ close).
                let work_nonhub: HashSet<SymbolId> = work
                    .iter()
                    .copied()
                    .filter(|s| {
                        let occ = idx.generality(*s);
                        occ > 0 && occ <= HUB_CAP
                    })
                    .collect();
                let mut scores: HashMap<SentenceId, f64> = HashMap::new();
                for &s in &work_nonhub {
                    let idf = 1.0 / idx.generality(s) as f64;
                    for &(_, aid) in idx.axioms_of_symbol(s) {
                        if selected.contains(&aid) || in_set.contains(&aid) {
                            continue;
                        }
                        *scores.entry(aid).or_insert(0.0) += idf;
                    }
                }
                let mut ranked: Vec<(SentenceId, f64)> = scores.into_iter().collect();
                ranked.sort_by(|a, b| {
                    b.1.partial_cmp(&a.1)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then(a.0.cmp(&b.0)) // deterministic ties
                });

                // Accept down the ranking: logical axioms only (no bookkeeping
                // heads) that share a non-hub PREDICATE with the goal frontier.
                let mut accepted: Vec<(SentenceId, f64, HashSet<SymbolId>)> = Vec::new();
                for (aid, score) in ranked.into_iter().take(top_k * SCAN_CAP) {
                    if accepted.len() >= top_k {
                        break;
                    }
                    let heads = head_syms(aid);
                    if heads.iter().any(|h| {
                        self.sym_name(*h)
                            .is_some_and(|s| excluded.contains(&*s.name()))
                    }) {
                        continue;
                    }
                    if !heads.iter().any(|h| work_nonhub.contains(h)) {
                        continue;
                    }
                    accepted.push((aid, score, heads));
                }

                if trace {
                    eprintln!(
                        "LIU round {round}: {} work syms ({} non-hub), {} rescued",
                        work.len(), work_nonhub.len(), accepted.len());
                    for (aid, score, _) in accepted.iter().take(8) {
                        eprintln!(
                            "  {score:.4}  {}",
                            crate::syntactic::display::sentence_to_plain_kif(*aid, self));
                    }
                }
                if accepted.is_empty() {
                    break;
                }
                // Next round's frontier: the rescued axioms' NEW predicate-
                // position symbols.
                work = HashSet::new();
                for (aid, _, heads) in accepted {
                    in_set.insert(aid);
                    included.push(aid);
                    for h in heads {
                        if seen_syms.insert(h) {
                            work.insert(h);
                        }
                    }
                }
            }
        });
        included
    }
}
