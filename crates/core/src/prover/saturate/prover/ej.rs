// crates/core/src/prover/saturate/prover/ej.rs
//
// Cross-literal equality joins for forward subsumption (phase 2b of the
// subterm-index milestone) — the hot-path consumer of the phase-2a
// k-channel rows (`rows.rs`).
//
// Forward subsumption asks: does stored clause C match into new clause
// D under ONE consistent substitution?  The funnel in
// `NativeProver::forward_subsumed` ends with the per-literal
// Key-Equation counting filter (`keq_unpartnered`) and then the exact
// backtracking check (`clause_subsumes_in`).  The gap keq cannot see is
// SHARED VARIABLES ACROSS C's LITERALS: literal pairs that are
// keq-partner-feasible individually, but whose implied bindings
// disagree.  This channel closes that gap with the 2a decode algebra:
//
//   * At first use per stored clause (memoized forever — clause terms
//     are immutable), each literal compiles into a 2a-style
//     `PatternPlan` (`rows::compile_into`: v <= 3 open positions,
//     >= 1 surplus row, greedy pivots, same trivial / fallback
//     classification).  Ground literals and unindexable shapes get no
//     plan and contribute NOTHING (keq's residue test is already
//     content-exact for ground literals).
//   * Per candidate (after keq passes, before the exact check), every
//     runnable C-literal decodes against each keq-feasible D-literal
//     partner.  The D side's rows come from a TRANSIENT registration
//     walk over the new clause's literal terms (built at most once per
//     `forward_subsumed` call), byte-identical in key/row derivation
//     to `SubtermPostings::walk` — parity-tested below.  Probing the
//     transient table is sound: if `cσ = d`, every decoded key is a
//     key of an actual subterm of D, which the walk registered.
//   * ZERO SURVIVING PARTNERS for a planned literal ⇒ reject (strictly
//     stronger than keq's count test: keq guaranteed >= 1 residue-
//     compatible partner, the decode refuted them all).
//   * EQUALITY JOIN: per shared variable, the possibility sets of
//     decoded keys UNION over all surviving partners of each literal
//     (a C-literal may partner multiple D-literals — partnering is NOT
//     assumed injective; injectivity is `clause_subsumes_in`'s job),
//     then INTERSECT across the literals containing the variable.
//     Empty intersection ⇒ reject.  No consistent-assignment search is
//     attempted — this is semi-join constraint propagation only.
//
// CONSERVATISM (false rejects are soundness-class bugs here — a
// rejected candidate the exact check would have accepted breaks the
// derivation-identity gate).  A rejection happens ONLY on:
//   (a) 2a-semantics pair rejection (surplus / probe / binding — the
//       twin-verified `PatternPlan::eval` machinery; the pair twin
//       below re-checks every pair rejection against `match_one_way`);
//   (b) zero surviving partners for a literal with a usable plan
//       (root non-fallback; fallback SUBnodes only make `eval` more
//       permissive, never less — they return `Ok` unconditionally);
//   (c) an empty per-variable key intersection where EVERY literal
//       containing the variable has a usable plan and the partner
//       enumeration was complete (it always is — the D-literal scan is
//       never truncated).  Anything unusable, capped, or unbound
//       contributes NO constraint: a variable occurring in a literal
//       without a usable plan is excluded from the join outright
//       (`ClausePlans::joinable`); a surviving partner that leaves the
//       variable undecoded (fallback/opaque site), or a possibility
//       set overflowing `EJ_KEY_CAP`, forces that literal's set to ⊤
//       (top = no constraint), never to a rejection.
//
// Soundness of the join: if C subsumes D via σ, then for every literal
// c_i of C there is a distinct D-literal d_i with c_i·σ = d_i.  Each
// (c_i, d_i) pair is keq-compatible (necessity of the per-pair test)
// and survives `eval` (necessity of the 2a chain), and because the
// row identity holds EXACTLY for a true instance, every decoded
// binding equals the content key of the actual σ-image — so for a
// shared variable X, key(σ(X)) lands in literal i's possibility set
// (or the set is ⊤).  Hence key(σ(X)) survives every intersection: an
// empty intersection proves no σ exists.  Key equality across literals
// of ONE D clause is term identity (keys are pure content functions;
// D-side variables coin as canonical blanks keyed by their clause-wide
// slot, so equal slots mean the SAME variable here — unlike across
// clauses, which this channel never joins).  A hash collision can only
// ADD a key to a set — a false PASS, caught by the exact check.
//
// Cost discipline: no per-candidate allocation.  Scratch (row table,
// binding table + trail, possibility sets) lives on the prover in
// [`EjScratch`]; possibility sets are inline `SmallVec`s capped at
// [`EJ_KEY_CAP`] (overflow ⇒ ⊤, never heap spill); the partner loop
// re-runs the same O(popcount) keq pair test `keq_unpartnered` uses
// (shared predicate — `keq_lit_compatible` — so the two chains cannot
// drift) and pays a few clmuls per decoded pair.

use std::sync::Arc;

use smallvec::SmallVec;

use crate::syntactic::sentence::ElementHasher;

use super::super::canon::canonical_var_cached;
use super::super::clause::{PLit, Term};
use super::super::hash64::Map64;
use super::super::kbo::KboOrdering;
use super::super::terms::TermFactsTable;
use super::super::units::op_tag;
use super::super::AtomInfo;
use super::rows::{self, PatternPlan, Row};
use super::ProverStats;

/// Possibility-set capacity (inline, allocation-free).  Overflow means
/// the variable's constraint from that literal becomes ⊤ (unknown) —
/// NEVER a rejection.
pub(crate) const EJ_KEY_CAP: usize = 8;

/// How one stored-clause literal participates in the channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LitClass {
    /// Ground literal: no variables to decode or join, and keq's
    /// residue test is already content-exact for it.  Never run.
    Ground,
    /// No usable plan (non-`App` atom, shapeless head, or root
    /// fallback — v >= 4 / rank-deficient).  Never run, and every
    /// variable it contains is barred from the join (conservatism
    /// rule (c)).
    Unusable,
    /// Compiled fine but constraint-free (depth-1, all-fresh-distinct
    /// variables — the 2a "trivial plan").  As a REJECTOR it cannot
    /// beat the keq test that already passed, so it runs ONLY when it
    /// contains a joinable variable: its decode then contributes join
    /// keys (the archetypal `(p ?X) ∨ (q ?X)` subsumer is two trivial
    /// plans whose whole power IS the join).
    Trivial,
    /// Compiled with real constraints (ground/bound closures, repeats,
    /// subpattern descent) — always run: its pair rejections and the
    /// zero-survivor rule add power beyond keq even without the join.
    Active,
}

/// One literal's compiled plan + variable footprint.
#[derive(Debug, Clone)]
pub(crate) struct LitPlan {
    pub(crate) plan: PatternPlan,
    pub(crate) class: LitClass,
    /// Bitmask of slot variables `< 64` occurring anywhere in the
    /// literal.  Slots `>= 64` are simply never joinable (they cannot
    /// be tracked in the masks) — a selectivity loss, never a
    /// soundness one.
    pub(crate) vars: u64,
}

/// A stored clause's compiled channel state — built lazily on the
/// clause's FIRST arrival at the ej stage of a `forward_subsumed`
/// probe and memoized on the prover (`NativeProver::ej_plans`),
/// deliberately NOT at accept time: the 2a report flagged the
/// per-accept registration tax, and most accepted clauses never
/// surface as post-keq subsumption candidates at all.
#[derive(Debug, Clone)]
pub(crate) struct ClausePlans {
    pub(crate) lits: Vec<LitPlan>,
    /// Binding-table size: max slot index + 1 over all literals
    /// (INCLUDING slots >= 64 — `eval` binds them even though the
    /// join ignores them).
    pub(crate) nslots: usize,
    /// Slots (< 64) occurring in >= 2 literals where EVERY occurrence
    /// literal has a usable plan (`Trivial` or `Active`) — the only
    /// slots the equality join may constrain.
    pub(crate) joinable: u64,
    /// Precomputed "anything to do at all" test (some literal is
    /// `Active`, or `Trivial` with a joinable variable).
    pub(crate) runnable_any: bool,
}

impl LitPlan {
    /// Whether this literal runs against the candidate's partners
    /// (see [`LitClass`] for the per-class rationale).
    #[inline]
    fn runnable(&self, joinable: u64) -> bool {
        match self.class {
            LitClass::Active => true,
            LitClass::Trivial => self.vars & joinable != 0,
            LitClass::Ground | LitClass::Unusable => false,
        }
    }
}

/// Collect the slot-variable footprint of `t` (mask of slots < 64) and
/// grow `max_end` to cover every slot (the binding-table size).
fn slot_scan(t: &Term, mask: &mut u64, max_end: &mut usize) {
    match t {
        Term::Var(s) => {
            if *s < 64 {
                *mask |= 1u64 << *s;
            }
            let end = *s as usize + 1;
            if end > *max_end {
                *max_end = end;
            }
        }
        Term::App(elems) => {
            for e in elems {
                slot_scan(e, mask, max_end);
            }
        }
        _ => {}
    }
}

/// Compile every literal of a stored clause into its channel plan.
/// Pure function of the literal terms (plus the shared ground-key memo)
/// — compile once, reuse forever.
pub(crate) fn compile_clause_plans(
    terms: &[(bool, Term)],
    facts: &TermFactsTable,
    kbo: &KboOrdering,
) -> ClausePlans {
    let mut lits = Vec::with_capacity(terms.len());
    let mut nslots = 0usize;
    let mut seen = 0u64;
    let mut shared = 0u64;
    for (_, t) in terms {
        let mut vars = 0u64;
        slot_scan(t, &mut vars, &mut nslots);
        let mut plan = PatternPlan::default();
        let class = if t.is_ground() {
            LitClass::Ground
        } else {
            match t {
                Term::App(elems) if rows::head_of(elems).is_some() => {
                    rows::compile_into(&mut plan, t, facts, kbo);
                    if plan.root_fallback() {
                        LitClass::Unusable
                    } else if plan.trivial() {
                        LitClass::Trivial
                    } else {
                        LitClass::Active
                    }
                }
                _ => LitClass::Unusable,
            }
        };
        shared |= seen & vars;
        seen |= vars;
        lits.push(LitPlan { plan, class, vars });
    }
    let mut joinable = shared;
    for lp in &lits {
        if !matches!(lp.class, LitClass::Trivial | LitClass::Active) {
            joinable &= !lp.vars;
        }
    }
    let runnable_any = lits.iter().any(|lp| lp.runnable(joinable));
    ClausePlans { lits, nslots, joinable, runnable_any }
}

/// Prover-owned scratch for the channel — reused across every
/// candidate of every probe, cleared incrementally (no per-candidate
/// allocation once warm).
#[derive(Debug, Default)]
pub(crate) struct EjScratch {
    /// Whether `table`/`lit_rows` describe the CURRENT probe's new
    /// clause.  `forward_subsumed` marks it stale at call entry; the
    /// first candidate reaching the ej stage rebuilds (candidates that
    /// never get here — the overwhelming majority — never pay).
    rows_ready: bool,
    /// Content key → row for every subterm of the new clause's literal
    /// atoms (the transient twin of `SubtermPostings::rows`).
    table: Map64<u64, Row>,
    /// Per-literal root-atom rows, indexed like the literal list.
    lit_rows: Vec<Row>,
    /// Decode binding table (slot → key) + rollback trail, per PAIR —
    /// bindings never survive across pairs, let alone candidates.
    bind: Vec<Option<u64>>,
    trail: Vec<u32>,
    /// Per-slot possibility sets: `gkeys` the running cross-literal
    /// intersection (valid iff the slot's bit is in the caller's
    /// `g_has`), `lkeys` the current literal's union (bit in `l_has`).
    /// Stale contents are overwritten before first use — no
    /// per-candidate clearing sweep.
    gkeys: Vec<SmallVec<[u64; EJ_KEY_CAP]>>,
    lkeys: Vec<SmallVec<[u64; EJ_KEY_CAP]>>,
}

impl EjScratch {
    /// Invalidate the transient row table (a new `forward_subsumed`
    /// probe has a new clause).
    #[inline]
    pub(crate) fn mark_stale(&mut self) {
        self.rows_ready = false;
    }
}

/// Transient registration walk: compute (content key, row) for `t` and
/// every subterm, inserting all of them into `tab`.  Byte-identical
/// key/row derivation to `SubtermPostings::walk` (parity-tested below)
/// — leaves and blanks get presence rows, concrete-headed compounds
/// get Vandermonde rows over their children's keys, shapeless-headed
/// compounds get presence rows keyed by their content id.
fn walk_rows(t: &Term, tab: &mut Map64<u64, Row>) -> u64 {
    match t {
        Term::Var(slot) => {
            let k = rows::blank_key(*slot);
            tab.entry(k).or_insert_with(|| rows::leaf_row(k));
            k
        }
        Term::Sym(s) => {
            let k = s.id();
            tab.entry(k).or_insert_with(|| rows::leaf_row(k));
            k
        }
        Term::Lit(l) => {
            let k = rows::lit_key(l);
            tab.entry(k).or_insert_with(|| rows::leaf_row(k));
            k
        }
        Term::Op(op) => {
            let k = u64::from(op_tag(op));
            tab.entry(k).or_insert_with(|| rows::leaf_row(k));
            k
        }
        Term::App(elems) => {
            let head = rows::head_of(elems);
            let mut h = ElementHasher::new(elems.len());
            let mut row = match head {
                Some(hk) => rows::node_tags(hk, elems.len()),
                None => [0u64; 4],
            };
            for (i, e) in elems.iter().enumerate() {
                let k = walk_rows(e, tab);
                match e {
                    Term::Var(slot) => h.variable(canonical_var_cached(*slot as usize), false),
                    Term::Sym(s) => h.symbol(s.id()),
                    Term::Lit(l) => h.literal(l),
                    Term::Op(op) => h.op(op),
                    Term::App(_) => h.sub(k),
                }
                if i >= 1 {
                    if let Some(hk) = head {
                        rows::accum_child(&mut row, rows::seat_elem(hk, i), k);
                    }
                }
            }
            let key = h.finish();
            if head.is_none() {
                row = rows::leaf_row(key);
            }
            tab.entry(key).or_insert(row);
            key
        }
    }
}

/// Build the new clause's transient row table + per-literal root rows.
fn build_rows(scratch: &mut EjScratch, d_terms: &[(bool, Term)]) {
    scratch.table.clear();
    scratch.lit_rows.clear();
    for (_, t) in d_terms {
        let k = walk_rows(t, &mut scratch.table);
        let r = *scratch.table.get(&k).expect("walk registered the root");
        scratch.lit_rows.push(r);
    }
    scratch.rows_ready = true;
}

/// The channel filter for one candidate subsumer: `true` = REJECT (the
/// exact check would fail — every rejection is twin-verified against
/// the reference matcher in debug/test builds).  `false` says nothing;
/// `clause_subsumes_in` still decides.
#[allow(clippy::too_many_arguments)]
#[cfg_attr(not(any(test, debug_assertions)), allow(unused_variables))]
pub(crate) fn filter(
    plans: &ClausePlans,
    c_lits: &[PLit],
    c_infos: &[Arc<AtomInfo>],
    c_terms: &[(bool, Term)],
    d_lits: &[PLit],
    d_infos: &[AtomInfo],
    d_terms: &[(bool, Term)],
    scratch: &mut EjScratch,
    stats: &mut ProverStats,
) -> bool {
    debug_assert_eq!(plans.lits.len(), c_lits.len(), "plan/literal lockstep");
    debug_assert_eq!(c_lits.len(), c_infos.len());
    debug_assert_eq!(d_lits.len(), d_infos.len());
    debug_assert_eq!(d_lits.len(), d_terms.len());
    if !plans.runnable_any {
        stats.ej_skipped_unusable += 1;
        return false;
    }
    stats.ej_candidates += 1;
    if !scratch.rows_ready {
        build_rows(scratch, d_terms);
    }
    debug_assert_eq!(scratch.lit_rows.len(), d_terms.len());
    if scratch.bind.len() < plans.nslots {
        scratch.bind.resize(plans.nslots, None);
    }
    if scratch.gkeys.len() < 64 {
        scratch.gkeys.resize(64, SmallVec::new());
        scratch.lkeys.resize(64, SmallVec::new());
    }
    debug_assert!(scratch.bind.iter().all(Option::is_none), "bind table clean on entry");
    debug_assert!(scratch.trail.is_empty());
    // Which slots have a live global (cross-literal) set in `gkeys`.
    let mut g_has = 0u64;
    for (ci, lp) in plans.lits.iter().enumerate() {
        if !lp.runnable(plans.joinable) {
            continue;
        }
        let join_mask = lp.vars & plans.joinable;
        let (cl, cinfo) = (&c_lits[ci], &*c_infos[ci]);
        // This literal's per-slot union state: a live set (`l_has`) or
        // forced-⊤ (`l_top` — undecoded occurrence or cap overflow).
        let mut l_has = 0u64;
        let mut l_top = 0u64;
        let mut survivors = 0usize;
        for (dj, dl) in d_lits.iter().enumerate() {
            // Same per-pair necessary condition keq counted partners
            // with — pairs it refutes cannot be σ-images.
            if !super::keq_lit_compatible(cl, cinfo, dl, &d_infos[dj]) {
                continue;
            }
            stats.ej_pairs_decoded += 1;
            let verdict = lp.plan.eval(
                &scratch.lit_rows[dj],
                &scratch.table,
                &mut scratch.bind,
                &mut scratch.trail,
            );
            if verdict.is_ok() {
                survivors += 1;
                let mut m = join_mask;
                while m != 0 {
                    let s = m.trailing_zeros() as usize;
                    m &= m - 1;
                    let bit = 1u64 << s;
                    if l_top & bit != 0 {
                        continue; // already ⊤ — absorbing
                    }
                    match scratch.bind[s] {
                        // Undecoded at a surviving partner (fallback /
                        // opaque site): the whole literal's constraint
                        // on this slot is ⊤.
                        None => {
                            l_top |= bit;
                            l_has &= !bit;
                        }
                        Some(k) => {
                            let set = &mut scratch.lkeys[s];
                            if l_has & bit == 0 {
                                set.clear();
                                set.push(k);
                                l_has |= bit;
                            } else if !set.contains(&k) {
                                if set.len() >= EJ_KEY_CAP {
                                    // Cap overflow ⇒ unknown, never
                                    // a constraint.
                                    l_top |= bit;
                                    l_has &= !bit;
                                } else {
                                    set.push(k);
                                }
                            }
                        }
                    }
                }
            } else {
                // MANDATORY pair twin (debug/test builds): a 2a-
                // semantics pair rejection must agree with the one-way
                // matcher — `eval` is a necessary condition for
                // `c_lit·σ = d_lit`.
                #[cfg(any(test, debug_assertions))]
                {
                    let mut s: super::Subst =
                        vec![None; super::term_slots_end(&c_terms[ci].1)];
                    debug_assert!(
                        !super::match_one_way(&c_terms[ci].1, &d_terms[dj].1, &mut s),
                        "ej pair decode rejected {:?} -> {:?} but match_one_way accepts",
                        c_terms[ci].1,
                        d_terms[dj].1,
                    );
                }
            }
            // Per-pair rollback: bindings never cross pairs.
            for &s in &scratch.trail {
                scratch.bind[s as usize] = None;
            }
            scratch.trail.clear();
        }
        if survivors == 0 {
            // keq guaranteed >= 1 residue-compatible partner for every
            // C literal; the decode refuted them all — no D literal
            // can host this literal's σ-image.
            stats.ej_rej_no_partner += 1;
            stats.ej_full_checks_saved += 1;
            // MANDATORY rejection twin (debug/test builds).
            #[cfg(any(test, debug_assertions))]
            debug_assert!(
                !super::clause_subsumes(c_terms, d_terms),
                "ej zero-partner rejected a pair clause_subsumes accepts: {:?} vs {:?}",
                c_terms,
                d_terms,
            );
            return true;
        }
        // Fold this literal's sets into the global intersection (⊤ and
        // never-constrained slots are identity).  Rejecting on a
        // partial intersection is sound: adding more literals can only
        // shrink it further, and the true σ's key would have to be in
        // every processed set already.
        let mut m = join_mask & l_has;
        while m != 0 {
            let s = m.trailing_zeros() as usize;
            m &= m - 1;
            let bit = 1u64 << s;
            if g_has & bit == 0 {
                let l = &scratch.lkeys[s];
                scratch.gkeys[s].clone_from(l);
                g_has |= bit;
            } else {
                let l = &scratch.lkeys[s];
                scratch.gkeys[s].retain(|k| l.contains(k));
                if scratch.gkeys[s].is_empty() {
                    stats.ej_rej_join += 1;
                    stats.ej_full_checks_saved += 1;
                    // MANDATORY rejection twin (debug/test builds).
                    #[cfg(any(test, debug_assertions))]
                    debug_assert!(
                        !super::clause_subsumes(c_terms, d_terms),
                        "ej equality-join rejected a pair clause_subsumes accepts: \
                         {:?} vs {:?}",
                        c_terms,
                        d_terms,
                    );
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::super::postings::SubtermPostings;
    use super::*;
    use crate::types::{Literal, Symbol};

    fn sym(n: &str) -> Term {
        Term::Sym(Symbol::from(n))
    }
    fn app(v: Vec<Term>) -> Term {
        Term::App(v)
    }
    fn var(s: u64) -> Term {
        Term::Var(s)
    }

    // The transient D-side walk must agree entry-for-entry with the
    // registration walk (`SubtermPostings::walk`) — same keys, same
    // rows, same domain — over a fixture set covering ground/open
    // compounds, blanks, literal and operator children, and nesting.
    // This is the parity that licenses evaluating stored-side plans
    // against transient-side rows at all.
    #[test]
    fn transient_walk_matches_registration_walk() {
        let facts = TermFactsTable::default();
        let kbo = KboOrdering::new();
        let fixtures = vec![
            (true, app(vec![
                sym("p"),
                app(vec![sym("f"), sym("a"), var(0)]),
                app(vec![
                    sym("g"),
                    app(vec![sym("f"), sym("a"), var(0)]),
                    Term::Lit(Literal::Str("s".into())),
                ]),
            ])),
            (false, app(vec![
                sym("q"),
                app(vec![sym("h"), app(vec![sym("f"), app(vec![sym("f"), sym("a")]), sym("b")])]),
                Term::Lit(Literal::Number("3".into())),
                var(1),
            ])),
            (true, app(vec![
                Term::Op(crate::parse::OpKind::Equal),
                app(vec![sym("mult"), var(0), app(vec![sym("inv"), var(0)])]),
                sym("e"),
            ])),
        ];
        let mut po = SubtermPostings::default();
        // Parity test compares transient rows against the STORED registration
        // rows, so registration must store them (store_rows = true).
        po.register_clause(0, &fixtures, &facts, &kbo, true);
        let mut scratch = EjScratch::default();
        build_rows(&mut scratch, &fixtures);
        // Same domain, same rows, both directions.
        assert_eq!(
            scratch.table.len(),
            po.row_table().len(),
            "transient and registration walks cover the same node set",
        );
        for (k, r) in &scratch.table {
            assert_eq!(
                po.row_table().get(k),
                Some(r),
                "row parity for key {k:#x}",
            );
        }
        // And every literal root row is the registered row of its atom.
        for ((_, t), r) in fixtures.iter().zip(&scratch.lit_rows) {
            let k = walk_rows(t, &mut Map64::default());
            assert_eq!(po.row_table().get(&k), Some(r), "root row parity for {t:?}");
        }
    }

    // Literal classification: ground ⇒ Ground; fresh-distinct depth-1 ⇒
    // Trivial; ground-anchored ⇒ Active; v >= 4 ⇒ Unusable — and the
    // joinable mask excludes any variable that occurs in a non-usable
    // literal (conservatism rule (c)).
    #[test]
    fn classification_and_joinable_mask() {
        let facts = TermFactsTable::default();
        let kbo = KboOrdering::new();
        let terms = vec![
            (true, app(vec![sym("g"), sym("a")])),                       // Ground
            (true, app(vec![sym("p"), var(0)])),                         // Trivial
            (true, app(vec![sym("q"), var(0), sym("k")])),               // Active
            (true, app(vec![sym("f"), var(1), var(2), var(3), var(4)])), // Unusable (v=4)
            (true, app(vec![sym("r"), var(1), sym("m")])),               // Active
        ];
        let cp = compile_clause_plans(&terms, &facts, &kbo);
        assert_eq!(cp.lits[0].class, LitClass::Ground);
        assert_eq!(cp.lits[1].class, LitClass::Trivial);
        assert_eq!(cp.lits[2].class, LitClass::Active);
        assert_eq!(cp.lits[3].class, LitClass::Unusable);
        assert_eq!(cp.lits[4].class, LitClass::Active);
        // ?0 is shared by two usable literals ⇒ joinable; ?1 is shared
        // but one occurrence sits in the Unusable literal ⇒ barred.
        assert_eq!(cp.joinable, 1u64 << 0, "only ?0 joins");
        assert!(cp.runnable_any);
        assert_eq!(cp.nslots, 5);
    }

    // A clause whose only literals are ground/unusable has nothing to
    // run — the filter must skip (counter), never reject.
    #[test]
    fn unusable_only_clause_is_never_runnable() {
        let facts = TermFactsTable::default();
        let kbo = KboOrdering::new();
        let terms = vec![
            (true, app(vec![sym("f"), var(0), var(1), var(2), var(3)])),
            (true, app(vec![sym("g"), sym("a")])),
        ];
        let cp = compile_clause_plans(&terms, &facts, &kbo);
        assert!(!cp.runnable_any);
        assert_eq!(cp.joinable, 0);
    }

    // A lone Trivial literal with no shared variable is skipped (the
    // inherited 2a trivial-plan discipline: as a rejector it cannot
    // beat the keq test that already passed).
    #[test]
    fn trivial_without_join_variable_is_not_runnable() {
        let facts = TermFactsTable::default();
        let kbo = KboOrdering::new();
        let terms = vec![(true, app(vec![sym("p"), var(0), var(1)]))];
        let cp = compile_clause_plans(&terms, &facts, &kbo);
        assert_eq!(cp.lits[0].class, LitClass::Trivial);
        assert_eq!(cp.joinable, 0, "no second literal — nothing shared");
        assert!(!cp.runnable_any);
    }
}
