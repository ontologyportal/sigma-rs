// crates/core/src/saturate/eventcalc.rs
//
// Discrete Event Calculus (DEC) narrative RECOGNIZER for the oracle.
//
// The CSR event-calculus problems load the standard DEC axiomatization
// (CSR001+0.ax: DEC1–DEC12) plus a per-problem narrative that defines
// `happens`/`initiates`/`terminates` by `<=>` enumeration.  Ordinary
// resolution explodes on the frame axioms (the `~∃Event` inertia
// conditions over a discrete-time chain), so instead we read the
// narrative into effect tables (`parse_narrative`, below) which
// `discharge_event_calculus` (in `prover/discharge.rs`) feeds to the
// GENERIC Datalog(¬) model kernel (`model::narrative_to_program` →
// `Program::evaluate`) — a decision procedure that answers `holdsAt(F,T)`
// (and its negation) by lookup over the kernel's perfect model.
//
// This module is the PARSER + soundness-gate ONLY: it recovers the
// narrative's tables from the stored KB and bails (`None`) on any feature
// outside the inertial fragment it models (DEC6/7/10/11): no `trajectory`
// (continuous change) and no `releases` (non-inertial fluents) — the
// spinning / forwards / backwards narrative (CSR001+2) is in-fragment; the
// water-tank narrative (trajectory + release + state-dependent `happens`)
// bails to ordinary resolution.  The bespoke forward-simulation engine this
// module once also contained (`simulate`) has been retired — the kernel is
// the sole evaluator now.

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
    /// Raw `(fluent, time, holds, sid)` rows from ground `holdsAt`
    /// hypotheses, resolved into `initial` (those at `times[0]`) once the
    /// timeline is ordered.  `sid` is the hypothesis sentence itself — the
    /// fact_parent for an emitted `holdsAt` unit whose initial value it set.
    pub initial_at: Vec<(Fluent, SymbolId, bool, SentenceId)>,
    /// `(fluent, time) → hypothesis sid`, resolved from `initial_at`
    /// (populated alongside `initial`) — provenance for the initial-state
    /// cells the grid reconstruction emits.
    pub initial_sid: HashMap<(Fluent, SymbolId), SentenceId>,
    /// The KB sentence defining `happens` (the only-if root
    /// `(=> (happens E T) (or …))`) — cited on every emitted `holdsAt`
    /// unit whose derivation passed through a `happens` fact/absence.
    pub happens_sid:    Option<SentenceId>,
    /// The KB sentence defining `initiates` (ditto, for `initiates`).
    pub initiates_sid:  Option<SentenceId>,
    /// The KB sentence defining `terminates` (ditto, for `terminates`).
    pub terminates_sid: Option<SentenceId>,
    /// `time → immediate successor`, derived from the KB's own `plus`/`less`
    /// order axioms (see [`order_succ_edge`] / [`order_chain`]) rather than
    /// assumed from the lexical `nK` suffix.  `None` when no complete order
    /// chain was found in the KB — the caller falls back to the lexical rank
    /// in that case.
    pub succ: Option<HashMap<SymbolId, SymbolId>>,
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
///
/// `scope` restricts the harvest to the asking scope (base + that
/// session's overlay): `root_sids` spans EVERY session's transients, and
/// merging another session's events would let the closed-world grid
/// assert `holdsAt`/`~holdsAt` units from a narrative the asker never
/// stated (same visibility rule as `store_facts`).
pub(crate) fn parse_narrative(
    syn:   &SyntacticLayer,
    scope: crate::semantics::types::Scope,
) -> Option<(Narrative, HashMap<SymbolId, Symbol>)> {
    let mut nar = Narrative::default();
    let mut names: HashMap<SymbolId, Symbol> = HashMap::new();
    let mut found_happens = false;
    let mut found_effect = false;
    let mut unsafe_narrative = false;
    let mut time_syms: HashSet<Symbol> = HashSet::new();
    // time → (immediate successor, defining sid), harvested from the KB's
    // own order axioms (see `order_succ_edge`) — the timeline-honesty
    // signal that lets `succ` be derived instead of assumed.  A `HashMap`
    // keeps only the first edge found per predecessor (the CSR chain
    // declares each exactly once).
    let mut succ_edges: HashMap<SymbolId, (SymbolId, SentenceId)> = HashMap::new();

    let note = |s: &Symbol, names: &mut HashMap<SymbolId, Symbol>| {
        names.insert(s.id(), s.clone());
    };

    for sid in syn.root_sids() {
        // Scope filter (see the fn doc): only base sentences and the
        // asking session's own overlay feed the narrative.
        let owners = syn.sessions.sessions_of(sid);
        let visible = owners.is_empty()
            || syn.sessions.is_axiom(sid)
            || matches!(scope,
                crate::semantics::types::Scope::Session(id) if owners.contains(&id));
        if !visible {
            continue;
        }
        let Some(s) = syn.sentence(sid) else { continue };

        // Harvest every time constant (`n0`, `n1`, …) anywhere in the root —
        // including the `plus`/`less` successor axioms — so the simulated
        // timeline extends past the last event (the final transition needs a
        // successor time to land on) and covers every conjecture's time.
        collect_time_syms(syn, &s, &mut time_syms, &mut names);

        // ---- Order axioms: `succ` EDB honesty from the KB's own order axioms. ----
        // `(=> (less_or_equal ?X nK) (less ?X nJ))` / `(=> (less ?X nJ)
        // (less_or_equal ?X nK))` — the `<=>` between "at most nK" and
        // "before nJ" the CSR `less1..less9` chain encodes — pins nJ as
        // nK's immediate successor directly from the two ground constants
        // in the axiom, no inference needed.  A ground `(equal (plus T n1)
        // T1)` successor fact is the other shape the KB carries (the
        // `plus0_1`/`plus1_1`/… table) and is read the same way.  Checked
        // BEFORE the narrative-definition branch below (which `continue`s
        // unconditionally for every `Implies`-headed root) since a `less`
        // chain root is itself an `Implies` root that would otherwise never
        // reach this check.
        if let Some(edge) = order_succ_edge(syn, &s) {
            succ_edges.entry(edge.0).or_insert((edge.1, sid));
        }

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
                    nar.happens_sid.get_or_insert(sid);
                } else {
                    let (Some(ev), Some(fl)) = (d.event, d.fluent) else { continue };
                    note(&ev, &mut names); note(&fl, &mut names);
                    let eff = Effect {
                        event:  ev.id(),
                        fluent: fl.id(),
                        pos_concurrent: d.pos_concurrent.iter().map(|s| s.id()).collect(),
                        neg_concurrent: d.neg_concurrent.iter().map(|s| s.id()).collect(),
                    };
                    if is_init {
                        nar.initiates.push(eff);
                        nar.initiates_sid.get_or_insert(sid);
                    } else {
                        nar.terminates.push(eff);
                        nar.terminates_sid.get_or_insert(sid);
                    }
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
                nar.initial_at.push((fl.id(), t.id(), !neg, sid));
            }
        }
    }

    // Require a complete inertial narrative and no non-inertial features.
    if unsafe_narrative || !(found_happens && found_effect) {
        return None;
    }

    // Timeline: every time constant seen.  Ordered by the KB's OWN order
    // axioms when they cover every harvested time constant in one chain
    // (timeline honesty, derived from the KB's own order axioms); falls back to the lexical `nK` suffix
    // rank only when no such chain is derivable (traced below).
    let mut times: Vec<Symbol> = time_syms.into_iter().collect();
    let time_ids: HashSet<SymbolId> = times.iter().map(Symbol::id).collect();
    let chain = order_chain(&succ_edges, &time_ids);
    let used_order_axioms = chain.is_some();
    if let Some(ordered) = &chain {
        let pos: HashMap<SymbolId, usize> =
            ordered.iter().enumerate().map(|(i, &t)| (t, i)).collect();
        times.sort_by_key(|s| pos.get(&s.id()).copied().unwrap_or(usize::MAX));
        nar.succ = Some(
            succ_edges
                .iter()
                .filter(|(from, (to, _))| time_ids.contains(from) && time_ids.contains(to))
                .map(|(&from, &(to, _))| (from, to))
                .collect(),
        );
    } else {
        times.sort_by_key(|s| time_rank(&s.name()));
    }
    if std::env::var_os("SIGMA_ORACLE_TRACE").is_some() {
        eprintln!(
            "EC[parse]: timeline order {} ({} order-axiom edge(s) covering {} time points)",
            if used_order_axioms { "AXIOM-DERIVED" } else { "LEXICAL FALLBACK" },
            succ_edges.len(),
            time_ids.len(),
        );
    }
    nar.times = times.iter().map(|s| s.id()).collect();

    // Resolve the initial state to `times[0]`.
    if let Some(&t0) = nar.times.first() {
        for &(fl, t, val, isid) in &nar.initial_at {
            if t == t0 {
                nar.initial.insert(fl, val);
                nar.initial_sid.insert((fl, t), isid);
            }
        }
        // A `holdsAt` hypothesis at any LATER time is a constraint the
        // DEC model never consumes — the closed-world grid fills that
        // cell purely from event effects and can then assert the
        // NEGATION of an explicit KB fact as a support unit (a wrong
        // Proved for `(not (holdsAt F t))`).  Such narratives are not
        // pure inertial narratives; bail and let the generic saturation
        // path handle them soundly.
        if nar.initial_at.iter().any(|&(_, t, _, _)| t != t0) {
            return None;
        }
    }

    Some((nar, names))
}

/// Does `edges` (predecessor → (successor, sid)) form ONE unbroken chain
/// covering exactly `wanted`?  Walks from the unique node with no incoming
/// edge; `None` if the coverage is incomplete, branches, or cycles — the
/// caller falls back to the lexical rank in that case (never a partial /
/// unsound order).
fn order_chain(
    edges:  &HashMap<SymbolId, (SymbolId, SentenceId)>,
    wanted: &HashSet<SymbolId>,
) -> Option<Vec<SymbolId>> {
    if wanted.is_empty() {
        return None;
    }
    let relevant: HashMap<SymbolId, SymbolId> = edges
        .iter()
        .filter(|(from, (to, _))| wanted.contains(from) && wanted.contains(to))
        .map(|(&from, &(to, _))| (from, to))
        .collect();
    if relevant.len() + 1 != wanted.len() {
        return None; // not a single chain spanning every time point
    }
    let succs: HashSet<SymbolId> = relevant.values().copied().collect();
    let mut roots = wanted.iter().copied().filter(|t| !succs.contains(t));
    let (Some(root), None) = (roots.next(), roots.next()) else { return None }; // need exactly one root
    let mut ordered = vec![root];
    let mut seen: HashSet<SymbolId> = [root].into_iter().collect();
    let mut cur = root;
    while let Some(&next) = relevant.get(&cur) {
        if !seen.insert(next) {
            return None; // cycle
        }
        ordered.push(next);
        cur = next;
    }
    (ordered.len() == wanted.len()).then_some(ordered)
}

/// Recognize one order-axiom root as a `(predecessor, successor)` `succ`
/// edge, when it directly names two ground time constants:
///
/// * `(=> (less_or_equal ?X A) (less ?X B))` or its converse
///   `(=> (less ?X B) (less_or_equal ?X A))` — the CSR `less1..less9` chain
///   (`A` is `X`'s bound, `B` is `A`'s immediate successor).
/// * `(equal (plus A n1) B)` or `(equal (plus n1 A) B)` — a ground
///   unit-successor fact from the `plus` table (symmetric, so either
///   argument order is read).
///
/// Anything else (a non-unit `plus` fact, a variable-only `less` root, …)
/// yields `None` — this is a targeted recognizer for the two literal shapes
/// the task's order axioms carry, not a general arithmetic evaluator.
fn order_succ_edge(syn: &SyntacticLayer, s: &Sentence) -> Option<(SymbolId, SymbolId)> {
    // `(equal (plus A n1) B)` / `(equal (plus n1 A) B)` — ground unit step.
    if s.op() == Some(&OpKind::Equal) && s.elements.len() == 3 {
        if let Some(rhs) = sym_of(&s.elements[2]) {
            if let Some(lhs_sid) = sub_id(&s.elements[1]) {
                if let Some(lhs) = syn.sentence(lhs_sid) {
                    if lhs.head_symbol_name().is_some_and(|h| &*h.name() == "plus")
                        && lhs.elements.len() == 3
                    {
                        let (a, b) = (sym_of(&lhs.elements[1]), sym_of(&lhs.elements[2]));
                        let is_one = |s: &Option<Symbol>| s.as_ref().is_some_and(|s| &*s.name() == "n1");
                        if is_one(&b) { if let Some(a) = a { return Some((a.id(), rhs.id())); } }
                        if is_one(&a) { if let Some(b) = b { return Some((b.id(), rhs.id())); } }
                    }
                }
            }
        }
        return None;
    }
    // `(=> (less_or_equal ?X A) (less ?X B))` / `(=> (less ?X B) (less_or_equal ?X A))`.
    if s.op() == Some(&OpKind::Implies) && s.elements.len() == 3 {
        let (Some(ant_sid), Some(con_sid)) = (sub_id(&s.elements[1]), sub_id(&s.elements[2]))
            else { return None };
        let (Some(ant), Some(con)) = (syn.sentence(ant_sid), syn.sentence(con_sid)) else { return None };
        let names = |a: &Sentence| a.head_symbol_name().map(|h| h.name().to_string());
        let (an, cn) = (names(&ant), names(&con));
        let le_lt = an.as_deref() == Some("less_or_equal") && cn.as_deref() == Some("less");
        let lt_le = an.as_deref() == Some("less") && cn.as_deref() == Some("less_or_equal");
        if !(le_lt || lt_le) {
            return None;
        }
        let (le, lt) = if le_lt { (&ant, &con) } else { (&con, &ant) };
        if le.elements.len() != 3 || lt.elements.len() != 3 {
            return None;
        }
        // Both atoms must share the SAME variable in the first argument
        // (the `?X` bound by the enclosing `<=>`) — otherwise this isn't
        // the chain shape.
        let (Some(v1), Some(v2)) = (var_id(&le.elements[1]), var_id(&lt.elements[1])) else { return None };
        if v1 != v2 {
            return None;
        }
        let (Some(a), Some(b)) = (sym_of(&le.elements[2]), sym_of(&lt.elements[2])) else { return None };
        return Some((a.id(), b.id()));
    }
    None
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

    // -- order_chain: the succ-EDB honesty fallback logic --------

    #[test]
    fn order_chain_complete_sequence_is_derived() {
        let (n0, n1, n2, n3) = (s("n0"), s("n1"), s("n2"), s("n3"));
        let mut edges = HashMap::new();
        edges.insert(n0, (n1, 1));
        edges.insert(n1, (n2, 2));
        edges.insert(n2, (n3, 3));
        let wanted: HashSet<SymbolId> = [n0, n1, n2, n3].into_iter().collect();
        assert_eq!(order_chain(&edges, &wanted), Some(vec![n0, n1, n2, n3]));
    }

    #[test]
    fn order_chain_missing_edge_falls_back() {
        let (n0, n1, n2) = (s("n0"), s("n1"), s("n2"));
        let mut edges = HashMap::new();
        edges.insert(n0, (n1, 1)); // n1 -> n2 missing
        let wanted: HashSet<SymbolId> = [n0, n1, n2].into_iter().collect();
        assert_eq!(order_chain(&edges, &wanted), None);
    }

    #[test]
    fn order_chain_cycle_falls_back() {
        let (n0, n1) = (s("n0"), s("n1"));
        let mut edges = HashMap::new();
        edges.insert(n0, (n1, 1));
        edges.insert(n1, (n0, 2)); // cycle: 2 edges, 2 nodes -> len+1 check fails
        let wanted: HashSet<SymbolId> = [n0, n1].into_iter().collect();
        assert_eq!(order_chain(&edges, &wanted), None);
    }

    #[test]
    fn order_chain_no_edges_falls_back() {
        let wanted: HashSet<SymbolId> = [s("n0"), s("n1")].into_iter().collect();
        assert_eq!(order_chain(&HashMap::new(), &wanted), None);
    }

    // -- order_succ_edge: recognizing the two literal order-axiom shapes ---

    #[test]
    fn order_succ_edge_less_or_equal_then_less() {
        let mut store = SyntacticLayer::default();
        // (=> (less_or_equal ?X n0) (less ?X n1))
        store.load_kif("(=> (less_or_equal ?X n0) (less ?X n1))", "t");
        let sid = *store.root_sids().first().unwrap();
        let sent = store.sentence(sid).unwrap();
        assert_eq!(order_succ_edge(&store, &sent), Some((s("n0"), s("n1"))));
    }

    #[test]
    fn order_succ_edge_less_then_less_or_equal_converse() {
        let mut store = SyntacticLayer::default();
        // (=> (less ?X n1) (less_or_equal ?X n0)) — the other <=> direction.
        store.load_kif("(=> (less ?X n1) (less_or_equal ?X n0))", "t");
        let sid = *store.root_sids().first().unwrap();
        let sent = store.sentence(sid).unwrap();
        assert_eq!(order_succ_edge(&store, &sent), Some((s("n0"), s("n1"))));
    }

    #[test]
    fn order_succ_edge_ground_plus_unit_step() {
        let mut store = SyntacticLayer::default();
        store.load_kif("(equal (plus n0 n1) n1)", "t");
        let sid = *store.root_sids().first().unwrap();
        let sent = store.sentence(sid).unwrap();
        assert_eq!(order_succ_edge(&store, &sent), Some((s("n0"), s("n1"))));
    }

    #[test]
    fn order_succ_edge_ground_plus_symmetric_arg_order() {
        let mut store = SyntacticLayer::default();
        store.load_kif("(equal (plus n1 n2) n3)", "t");
        let sid = *store.root_sids().first().unwrap();
        let sent = store.sentence(sid).unwrap();
        assert_eq!(order_succ_edge(&store, &sent), Some((s("n2"), s("n3"))));
    }

    #[test]
    fn order_succ_edge_non_unit_plus_is_not_a_succ_edge() {
        let mut store = SyntacticLayer::default();
        // plus(n1,n1)=n2 — a real fact but neither argument is n1's *unit*
        // step target in the sense the recognizer needs disambiguated;
        // still: one argument IS n1, so this DOES read as n1 -> n2. Use a
        // genuinely non-unit pair instead: plus(n2,n3)=n5 has no n1 operand.
        store.load_kif("(equal (plus n2 n3) n5)", "t");
        let sid = *store.root_sids().first().unwrap();
        let sent = store.sentence(sid).unwrap();
        assert_eq!(order_succ_edge(&store, &sent), None);
    }

    #[test]
    fn order_succ_edge_unrelated_root_is_none() {
        let mut store = SyntacticLayer::default();
        store.load_kif("(happens push n0)", "t");
        let sid = *store.root_sids().first().unwrap();
        let sent = store.sentence(sid).unwrap();
        assert_eq!(order_succ_edge(&store, &sent), None);
    }
}
