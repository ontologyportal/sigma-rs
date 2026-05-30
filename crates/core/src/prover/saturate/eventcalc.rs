// crates/core/src/saturate/eventcalc.rs
//
// Discrete Event Calculus (DEC) forward-simulation for the oracle.
//
// The CSR event-calculus problems load the standard DEC axiomatization
// (CSR001+0.ax: DEC1–DEC12) plus a per-problem narrative that defines
// `happens`/`initiates`/`terminates` by `<=>` enumeration.  Ordinary
// resolution explodes on the frame axioms (the `~∃Event` inertia
// conditions over a discrete-time chain), so instead we read the
// narrative into effect tables and FORWARD-SIMULATE the fluent state
// over the ground time points — a decision procedure that answers
// `holdsAt(F,T)` (and its negation) by lookup.
//
// This module is the simulation ENGINE (pure, over parsed tables).  This
// first phase models the inertial fragment (DEC6/7/10/11): no
// `trajectory` (continuous change) and no `releases` (non-inertial
// fluents) — the spinning / forwards / backwards narrative (CSR001+2).
// The water-tank narrative (trajectory + release + state-dependent
// `happens`) is a later phase.

use std::collections::{HashMap, HashSet};

use crate::syntactic::SyntacticLayer;
use crate::types::{Element, OpKind, Sentence, SentenceId, Symbol, SymbolId};

/// A fluent identity.  The inertial fragment uses bare symbol fluents
/// (`forwards`, `backwards`, `spinning`); functional fluents
/// (`waterLevel(H)`) arrive with the trajectory phase.
pub(crate) type Fluent = SymbolId;

/// One `initiates`/`terminates` effect rule: event `event` affects
/// `fluent`, guarded by same-time concurrent-event conditions
/// (`happens(e,T)` must / must-not also hold at the event's time).
#[derive(Debug, Clone)]
pub(crate) struct Effect {
    pub event:   SymbolId,
    pub fluent:  Fluent,
    /// Events that must ALSO happen at the same time (`happens(e,T)`).
    pub pos_concurrent: Vec<SymbolId>,
    /// Events that must NOT happen at the same time (`~happens(e,T)`).
    pub neg_concurrent: Vec<SymbolId>,
}

impl Effect {
    /// Does this rule's concurrent-event guard hold given the set of
    /// events happening at the rule's time?
    fn guard_holds(&self, events_at_t: &[SymbolId]) -> bool {
        self.pos_concurrent.iter().all(|e| events_at_t.contains(e))
            && self.neg_concurrent.iter().all(|e| !events_at_t.contains(e))
    }
}

/// A parsed DEC narrative over the inertial fragment.
#[derive(Debug, Default, Clone)]
pub(crate) struct Narrative {
    /// Ground time points in order (`n0, n1, …, nK`).
    pub times:      Vec<SymbolId>,
    /// `time → events that happen at it`.
    pub happens:    HashMap<SymbolId, Vec<SymbolId>>,
    pub initiates:  Vec<Effect>,
    pub terminates: Vec<Effect>,
    /// Fluent → whether it holds at `times[0]` (default: false).
    pub initial:    HashMap<Fluent, bool>,
    /// Raw `(fluent, time, holds)` rows from ground `holdsAt` hypotheses,
    /// resolved into `initial` (those at `times[0]`) once the timeline is
    /// ordered.
    pub initial_at: Vec<(Fluent, SymbolId, bool)>,
}

impl Narrative {
    /// All fluents the narrative reasons about (initial state ∪ every
    /// effect rule's fluent).
    fn fluents(&self) -> HashSet<Fluent> {
        let mut fs: HashSet<Fluent> = self.initial.keys().copied().collect();
        for e in self.initiates.iter().chain(self.terminates.iter()) {
            fs.insert(e.fluent);
        }
        fs
    }
}

/// Forward-simulate the narrative, returning the complete fluent state
/// `(fluent, time) → holds`.  Inertial DEC: a fluent holds at `T+1` iff
/// it is initiated at `T`, or it held at `T` and was not terminated at
/// `T` (DEC6/7/10/11).  Complete-state, so both `holdsAt` and `~holdsAt`
/// are decided.
pub(crate) fn simulate(n: &Narrative) -> HashMap<(Fluent, SymbolId), bool> {
    let mut state: HashMap<(Fluent, SymbolId), bool> = HashMap::new();
    let fluents = n.fluents();
    let Some(&t0) = n.times.first() else { return state };

    // Initial state at times[0].
    for &f in &fluents {
        let holds = n.initial.get(&f).copied().unwrap_or(false);
        state.insert((f, t0), holds);
    }

    let empty: Vec<SymbolId> = Vec::new();
    for w in n.times.windows(2) {
        let (t, t1) = (w[0], w[1]);
        let events = n.happens.get(&t).unwrap_or(&empty);
        for &f in &fluents {
            let initiated = n.initiates.iter().any(|e| {
                e.fluent == f && events.contains(&e.event) && e.guard_holds(events)
            });
            let terminated = n.terminates.iter().any(|e| {
                e.fluent == f && events.contains(&e.event) && e.guard_holds(events)
            });
            let held = state.get(&(f, t)).copied().unwrap_or(false);
            let next = initiated || (held && !terminated);
            state.insert((f, t1), next);
        }
    }
    state
}

// ---------------------------------------------------------------------------
// Narrative parsing — recover the DEC tables from the stored (normalized) KB.
// ---------------------------------------------------------------------------
//
// The narrative is defined by three biconditionals (`happens_all_defn`,
// `initiates_all_defn`, `terminates_all_defn`).  At ingest the top-level
// `(forall …)` is stripped and the `<=>` is split into two implications; the
// *only-if* direction survives intact as a single root:
//
//     (=> (initiates E F T) (or d1 … dn))
//     (=> (happens   E T)   (or d1 … dn))
//
// where each disjunct `di` is `(and (= E c) (= F c) [±(happens e T)])`.  We
// read that one root per relation — antecedent gives the variable positions,
// the consequent disjunction gives the effect / timeline rows.

/// Fetch a sub-sentence id from an element.
fn sub_id(e: &Element) -> Option<SentenceId> {
    match e {
        Element::Sub(sid) => Some(*sid),
        _ => None,
    }
}

/// A variable element's id.
fn var_id(e: &Element) -> Option<SymbolId> {
    match e {
        Element::Variable { id, .. } => Some(*id),
        _ => None,
    }
}

/// A ground symbol element's `Symbol`.
fn sym_of(e: &Element) -> Option<Symbol> {
    match e {
        Element::Symbol(s) => Some(s.0.clone()),
        _ => None,
    }
}

/// Flatten a (possibly binary-nested) `(or …)` / `(and …)` into its child
/// sub-sentence ids; a non-`op` sentence yields itself.
fn flatten(syn: &SyntacticLayer, sid: SentenceId, op: &OpKind) -> Vec<SentenceId> {
    let Some(s) = syn.sentence(sid) else { return vec![sid] };
    if s.op() != Some(op) {
        return vec![sid];
    }
    let mut out = Vec::new();
    for e in &s.elements[1..] {
        if let Some(c) = sub_id(e) {
            out.extend(flatten(syn, c, op));
        }
    }
    out
}

/// Parse one effect/timeline disjunct under the LHS atom's variable roles.
/// `vevent`/`vfluent`/`vtime` are the var ids at the LHS argument positions
/// (`vfluent = None` for `happens`).  Records constant bindings into `names`.
struct Disjunct {
    event:  Option<Symbol>,
    fluent: Option<Symbol>,
    time:   Option<Symbol>,
    pos_concurrent: Vec<Symbol>,
    neg_concurrent: Vec<Symbol>,
    /// A feature outside the inertial fragment was seen in this disjunct
    /// (functional fluent, existential, …) — the narrative must NOT be
    /// simulated with this engine (would be unsound).
    unsafe_feature: bool,
}

fn parse_disjunct(
    syn:     &SyntacticLayer,
    dj_sid:  SentenceId,
    vevent:  SymbolId,
    vfluent: Option<SymbolId>,
    vtime:   SymbolId,
) -> Option<Disjunct> {
    let conjuncts = flatten(syn, dj_sid, &OpKind::And);
    let mut d = Disjunct {
        event: None, fluent: None, time: None,
        pos_concurrent: Vec::new(), neg_concurrent: Vec::new(),
        unsafe_feature: false,
    };
    let is_role_var = |v: SymbolId| v == vevent || Some(v) == vfluent || v == vtime;
    for cj in conjuncts {
        let Some(c) = syn.sentence(cj) else { continue };
        match c.op() {
            // (= Var Const) — bind the role to the constant (either arg order).
            Some(&OpKind::Equal) if c.elements.len() == 3 => {
                let (v, k) = match (var_id(&c.elements[1]), sym_of(&c.elements[2])) {
                    (Some(v), Some(k)) => (v, k),
                    _ => match (var_id(&c.elements[2]), sym_of(&c.elements[1])) {
                        (Some(v), Some(k)) => (v, k),
                        _ => {
                            // A role variable equated to a NON-symbol (e.g.
                            // `Fluent = waterLevel(Height)`): a functional
                            // fluent — outside the inertial fragment.
                            if var_id(&c.elements[1]).is_some_and(is_role_var)
                                || var_id(&c.elements[2]).is_some_and(is_role_var)
                            {
                                d.unsafe_feature = true;
                            }
                            continue;
                        }
                    },
                };
                if v == vevent { d.event = Some(k); }
                else if Some(v) == vfluent { d.fluent = Some(k); }
                else if v == vtime { d.time = Some(k); }
            }
            // (not (happens e T)) — a negative concurrent-event guard.
            Some(&OpKind::Not) if c.elements.len() == 2 => {
                if let Some(inner_sid) = sub_id(&c.elements[1]) {
                    if let Some(ev) = happens_event(syn, inner_sid) {
                        d.neg_concurrent.push(ev);
                    }
                }
            }
            // (happens e T) — a positive concurrent-event guard.
            None => {
                if let Some(ev) = happens_event(syn, cj) {
                    d.pos_concurrent.push(ev);
                }
            }
            // A quantifier (`? [Height] : …`) inside a disjunct — a
            // functional / parameterized narrative, not the inertial fragment.
            Some(&OpKind::Exists) | Some(&OpKind::ForAll) => {
                d.unsafe_feature = true;
            }
            _ => {}
        }
    }
    Some(d)
}

/// If `sid` is `(happens e T)`, return the event constant `e`.
fn happens_event(syn: &SyntacticLayer, sid: SentenceId) -> Option<Symbol> {
    let s = syn.sentence(sid)?;
    let head = s.head_symbol_name()?;
    if &*head.name() != "happens" || s.elements.len() < 2 {
        return None;
    }
    sym_of(&s.elements[1])
}

/// Parse the DEC narrative out of the store, returning the simulation tables
/// plus a `SymbolId → Symbol` map for every constant (so the prover can
/// rebuild `holdsAt` terms).  `None` when no DEC narrative is present (so the
/// discharge is a no-op on SUMO and every non-EC corpus).
pub(crate) fn parse_narrative(
    syn: &SyntacticLayer,
) -> Option<(Narrative, HashMap<SymbolId, Symbol>)> {
    let mut nar = Narrative::default();
    let mut names: HashMap<SymbolId, Symbol> = HashMap::new();
    let mut found_happens = false;
    let mut found_effect = false;
    let mut unsafe_narrative = false;
    let mut time_syms: HashSet<Symbol> = HashSet::new();

    let note = |s: &Symbol, names: &mut HashMap<SymbolId, Symbol>| {
        names.insert(s.id(), s.clone());
    };

    for sid in syn.root_sids() {
        let Some(s) = syn.sentence(sid) else { continue };

        // Harvest every time constant (`n0`, `n1`, …) anywhere in the root —
        // including the `plus`/`less` successor axioms — so the simulated
        // timeline extends past the last event (the final transition needs a
        // successor time to land on) and covers every conjecture's time.
        collect_time_syms(syn, &s, &mut time_syms, &mut names);

        // ---- Root A: (=> (HEAD vars…) (or d…)) — a narrative definition. ----
        if s.op() == Some(&OpKind::Implies) && s.elements.len() == 3 {
            let (Some(ant_sid), Some(con_sid)) =
                (sub_id(&s.elements[1]), sub_id(&s.elements[2])) else { continue };
            let (Some(ant), Some(con)) = (syn.sentence(ant_sid), syn.sentence(con_sid))
                else { continue };
            // Non-inertial markers: a `releases` definition (releases makes a
            // fluent non-inertial) or a `trajectory` / `antiTrajectory` rule
            // (continuous functional change).  Either ⇒ outside this engine's
            // fragment; flag and let resolution handle it instead of emitting
            // an unsound partial state.
            if let Some(ah) = ant.head_symbol_name() {
                if &*ah.name() == "releases" { unsafe_narrative = true; }
            }
            if let Some(ch) = con.head_symbol_name() {
                let c = &*ch.name();
                if c == "trajectory" || c == "antiTrajectory" { unsafe_narrative = true; }
            }
            let Some(head) = ant.head_symbol_name() else { continue };
            let hname = &*head.name();
            let is_happens = hname == "happens";
            let is_init    = hname == "initiates";
            let is_term    = hname == "terminates";
            if !(is_happens || is_init || is_term) {
                continue;
            }
            // Variable positions of the LHS atom.
            let (vevent, vfluent, vtime) = if is_happens {
                let v0 = var_id(ant.elements.get(1)?);
                let v1 = var_id(ant.elements.get(2)?);
                match (v0, v1) { (Some(e), Some(t)) => (e, None, t), _ => continue }
            } else {
                let v0 = var_id(ant.elements.get(1)?);
                let v1 = var_id(ant.elements.get(2)?);
                let v2 = var_id(ant.elements.get(3)?);
                match (v0, v1, v2) {
                    (Some(e), Some(f), Some(t)) => (e, Some(f), t),
                    _ => continue,
                }
            };
            if con.op() != Some(&OpKind::Or) {
                continue;
            }
            for dj in flatten(syn, con_sid, &OpKind::Or) {
                let Some(d) = parse_disjunct(syn, dj, vevent, vfluent, vtime) else { continue };
                if d.unsafe_feature { unsafe_narrative = true; }
                for sym in d.pos_concurrent.iter().chain(d.neg_concurrent.iter()) {
                    note(sym, &mut names);
                }
                if is_happens {
                    let (Some(ev), Some(t)) = (d.event, d.time) else { continue };
                    note(&ev, &mut names); note(&t, &mut names);
                    time_syms.insert(t.clone());
                    nar.happens.entry(t.id()).or_default().push(ev.id());
                    found_happens = true;
                } else {
                    let (Some(ev), Some(fl)) = (d.event, d.fluent) else { continue };
                    note(&ev, &mut names); note(&fl, &mut names);
                    let eff = Effect {
                        event:  ev.id(),
                        fluent: fl.id(),
                        pos_concurrent: d.pos_concurrent.iter().map(|s| s.id()).collect(),
                        neg_concurrent: d.neg_concurrent.iter().map(|s| s.id()).collect(),
                    };
                    if is_init { nar.initiates.push(eff); } else { nar.terminates.push(eff); }
                    found_effect = true;
                }
            }
            continue;
        }

        // ---- Initial state: ground `holdsAt(F,T)` / `(not (holdsAt F T))`. ----
        // A positive `holdsAt` root (a hypothesis) sets the fluent true at its
        // time; negatives are the default, recorded for completeness.  Also
        // harvests time constants (`n0`, `n1`, …).
        let (neg, atom) = match s.op() {
            Some(&OpKind::Not) if s.elements.len() == 2 => {
                let Some(a) = sub_id(&s.elements[1]).and_then(|i| syn.sentence(i)) else { continue };
                (true, a)
            }
            None => (false, s.clone()),
            _ => continue,
        };
        let Some(hd) = atom.head_symbol_name() else { continue };
        if &*hd.name() == "holdsAt" && atom.elements.len() == 3 {
            if let (Some(fl), Some(t)) = (sym_of(&atom.elements[1]), sym_of(&atom.elements[2])) {
                note(&fl, &mut names); note(&t, &mut names);
                time_syms.insert(t.clone());
                // Record the initial value keyed by (fluent,time); resolved
                // against `times[0]` once the timeline is known.
                nar.initial_at.push((fl.id(), t.id(), !neg));
            }
        }
    }

    // Require a complete inertial narrative and no non-inertial features.
    if unsafe_narrative || !(found_happens && found_effect) {
        return None;
    }

    // Timeline: every time constant seen, ordered by the numeric suffix of its
    // name (`n0 < n1 < … < nK`).  Falls back to insertion via the names map.
    let mut times: Vec<Symbol> = time_syms.into_iter().collect();
    times.sort_by_key(|s| time_rank(&s.name()));
    nar.times = times.iter().map(|s| s.id()).collect();

    // Resolve the initial state to `times[0]`.
    if let Some(&t0) = nar.times.first() {
        for &(fl, t, val) in &nar.initial_at {
            if t == t0 {
                nar.initial.insert(fl, val);
            }
        }
    }

    Some((nar, names))
}

/// Recursively collect every time-constant symbol (`n` followed by digits)
/// reachable from `s`, noting each in `names`.
fn collect_time_syms(
    syn:   &SyntacticLayer,
    s:     &Sentence,
    times: &mut HashSet<Symbol>,
    names: &mut HashMap<SymbolId, Symbol>,
) {
    for e in &s.elements {
        match e {
            Element::Symbol(sym) => {
                let name = sym.name();
                if is_time_name(&name) {
                    let symbol = sym.0.clone();
                    names.insert(symbol.id(), symbol.clone());
                    times.insert(symbol);
                }
            }
            Element::Sub(sid) => {
                if let Some(child) = syn.sentence(*sid) {
                    collect_time_syms(syn, &child, times, names);
                }
            }
            _ => {}
        }
    }
}

/// `nK` for K ≥ 0 — a discrete-time constant name.
fn is_time_name(name: &str) -> bool {
    matches!(name.strip_prefix('n'), Some(d) if !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}

/// Numeric rank of a time constant name `nK` → `K` (non-`n` names sort last).
fn time_rank(name: &str) -> i64 {
    name.strip_prefix('n')
        .and_then(|d| d.parse::<i64>().ok())
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Symbol;

    fn s(name: &str) -> SymbolId { Symbol::hash_name(name) }

    /// The CSR001+2 spinning/forwards/backwards narrative:
    ///   happens: push@n0, pull@n1, {pull,push}@n2
    ///   push initiates forwards if ¬pull;  pull initiates backwards if ¬push;
    ///   pull initiates spinning if push (concurrent push+pull ⇒ spinning)
    ///   + the matching terminations.
    /// Initial: nothing holds.
    #[test]
    fn spinning_narrative_simulates() {
        let (n0, n1, n2, n3) = (s("n0"), s("n1"), s("n2"), s("n3"));
        let (push, pull) = (s("push"), s("pull"));
        let (fwd, bwd, spin) = (s("forwards"), s("backwards"), s("spinning"));
        let mut happens = HashMap::new();
        happens.insert(n0, vec![push]);
        happens.insert(n1, vec![pull]);
        happens.insert(n2, vec![pull, push]); // concurrent
        let initiates = vec![
            Effect { event: push, fluent: fwd,  pos_concurrent: vec![],     neg_concurrent: vec![pull] },
            Effect { event: pull, fluent: bwd,  pos_concurrent: vec![],     neg_concurrent: vec![push] },
            Effect { event: pull, fluent: spin, pos_concurrent: vec![push], neg_concurrent: vec![] },
        ];
        let terminates = vec![
            Effect { event: push, fluent: bwd,  pos_concurrent: vec![],     neg_concurrent: vec![pull] },
            Effect { event: pull, fluent: fwd,  pos_concurrent: vec![],     neg_concurrent: vec![push] },
            Effect { event: pull, fluent: fwd,  pos_concurrent: vec![push], neg_concurrent: vec![] },
            Effect { event: pull, fluent: bwd,  pos_concurrent: vec![push], neg_concurrent: vec![] },
            Effect { event: push, fluent: spin, pos_concurrent: vec![],     neg_concurrent: vec![pull] },
            Effect { event: pull, fluent: spin, pos_concurrent: vec![],     neg_concurrent: vec![push] },
        ];
        let n = Narrative {
            times: vec![n0, n1, n2, n3],
            happens,
            initiates,
            terminates,
            initial: HashMap::new(), // all false at n0
            initial_at: Vec::new(),
        };
        let st = simulate(&n);
        let h = |f: SymbolId, t: SymbolId| st.get(&(f, t)).copied().unwrap_or(false);

        // n0→n1: push (alone) ⇒ forwards on, backwards/spinning off.
        assert!(h(fwd, n1));
        assert!(!h(bwd, n1));
        assert!(!h(spin, n1)); // the CSR017 conjecture: ¬spinning@n1
        // n1→n2: pull (alone) ⇒ backwards on, forwards/spinning off.
        assert!(h(bwd, n2));
        assert!(!h(fwd, n2));
        assert!(!h(spin, n2)); // CSR020: ¬spinning@n2
        // n2→n3: concurrent push+pull ⇒ spinning on, forwards/backwards off.
        assert!(h(spin, n3));
        assert!(!h(fwd, n3));
        assert!(!h(bwd, n3));
    }
}
