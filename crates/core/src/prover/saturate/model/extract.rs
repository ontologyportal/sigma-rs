// crates/core/src/saturate/model/extract.rs
//
// Phase 3 — automatic extraction of a Datalog(¬) program from stored axioms.
//
// Replaces the hand-authored programs of Phase 2 with a scan over the
// SyntacticLayer roots that recovers the definite/Horn fragment:
//
//   * `(=> (and B1 … Bn) H)` / `(=> B H)`  →  a rule `H :- B1, …, Bn`
//     (each Bi may be `(not A)` for a negative literal);
//   * `(or l1 … lk)` with EXACTLY ONE positive symbol-headed literal `c` and
//     the rest negated symbol-headed literals `a1, …, an`  →  the same rule
//     `c :- a1, …, an` (Milestone C — a clause `¬a1 ∨ … ∨ ¬an ∨ c` IS that
//     Horn rule).  This is the shape TPTP CNF input and clausified FOF store
//     their axioms in — without this arm the extractor only ever matched
//     KIF-style `(=>)` roots, so the extracted program was structurally
//     EMPTY on the whole TPTP corpus (see `extract_horn_program_stats` /
//     `horn_rule_of_or` below for the full case breakdown: all-negative and
//     ≥2-positive clauses are skipped, not extracted);
//   * a ground symbol-headed atom `(rel c1 … ck)`  →  an EDB fact.
//
// Only atoms whose arguments are variables or symbol constants are taken —
// compound (function) or literal arguments are not first-order Datalog terms,
// so a rule mentioning one is skipped (it falls through to resolution).  The
// extracted program is handed to the kernel (`Program::evaluate`), which
// applies the stratification / safety gates; anything that fails them is
// rejected there, soundly.
//
// NOTE: written under a tooling outage and not yet compiled/tested — the build
// + reproduction tests are the Phase-3 gate and run before this is relied on.

use std::collections::{HashMap, HashSet};

use crate::semantics::roles::TaxonomyRoles;
use crate::syntactic::SyntacticLayer;
use crate::types::{Element, OpKind, Sentence, SentenceId, SymbolId};

use super::{Atom, DTerm, Literal, Program, Rule};

/// A sub-sentence id from an element.
fn sub(e: &Element) -> Option<SentenceId> {
    match e {
        Element::Sub(sid) => Some(*sid),
        _ => None,
    }
}

/// Convert a symbol-headed atom sentence into a model [`Atom`], mapping each
/// distinct logical variable to a rule-local index via `vars`.  Returns the
/// atom and whether it is ground.  `None` if the sentence is not a
/// symbol-headed atom or has a compound / literal / operator argument.
fn atom_of(s: &Sentence, vars: &mut HashMap<SymbolId, u32>) -> Option<(Atom, bool)> {
    let pred = s.head_symbol()?;
    if s.elements.len() < 2 {
        // A nullary/propositional atom — represent as a 0-ary relation.
        return Some((Atom { pred, args: Vec::new() }, true));
    }
    let mut args = Vec::with_capacity(s.elements.len() - 1);
    let mut ground = true;
    for el in &s.elements[1..] {
        match el {
            Element::Symbol(sym) => args.push(DTerm::Const(sym.id())),
            Element::Variable { id, .. } => {
                let next = vars.len() as u32;
                let idx = *vars.entry(*id).or_insert(next);
                args.push(DTerm::Var(idx));
                ground = false;
            }
            _ => return None, // Sub (function term) / Literal / Op: not Datalog
        }
    }
    Some((Atom { pred, args }, ground))
}

/// Parse one body element into a [`Literal`] (handling a leading `(not …)`).
fn literal_of(
    syn:  &SyntacticLayer,
    sid:  SentenceId,
    vars: &mut HashMap<SymbolId, u32>,
) -> Option<Literal> {
    let s = syn.sentence(sid)?;
    if s.op() == Some(&OpKind::Not) && s.elements.len() == 2 {
        let inner = sub(&s.elements[1])?;
        let inner_s = syn.sentence(inner)?;
        let (atom, _) = atom_of(&inner_s, vars)?;
        Some(Literal { atom, negated: true })
    } else {
        let (atom, _) = atom_of(&s, vars)?;
        Some(Literal { atom, negated: false })
    }
}

/// Counters for the clausal (`or`-root) extraction arm — Milestone C.  Purely
/// diagnostic (SIGMA_STATS-style bookkeeping); extraction behavior does not
/// depend on these.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ExtractStats {
    /// `(or …)` roots turned into a Horn rule (exactly one positive literal).
    pub(crate) or_rules: u32,
    /// `(or …)` roots with 0 positive literals (all-negative — a denial /
    /// goal shape, not a definite rule).  Not extracted here; left for
    /// resolution (and, eventually, a dedicated denials/goal channel).
    pub(crate) or_all_negative_skipped: u32,
    /// `(or …)` roots with ≥2 positive literals (non-Horn).  Not extracted.
    pub(crate) or_non_horn_skipped: u32,
    /// `(or …)` roots that had a qualifying single positive literal but some
    /// literal's atom wasn't a plain symbol-headed Datalog atom (compound
    /// term / equality / nested operator) — skipped, falls through to
    /// resolution.
    pub(crate) or_non_datalog_skipped: u32,
    /// A single-literal negative clause `(not (rel c1 … ck))` — ground (a
    /// negative fact, e.g. SUMO's `(not (member Denmark EuropeanMonetaryUnion))`)
    /// or with variables (e.g. an irreflexivity axiom
    /// `(not (colleague ?A ?A ?ORG))`).  Neither is an EDB fact (Datalog(¬)
    /// has no negative facts) nor a rule — both are denial/integrity-constraint
    /// shapes, left for resolution's denials channel.  (Pre-existing
    /// behavior: these fell through the old code's catch-all arm silently;
    /// this only adds the count, not a behavior change.)
    pub(crate) negative_unit_skipped: u32,
}

/// Classify one `(or l1 … lk)` root as a Horn rule, if it has EXACTLY ONE
/// positive symbol-headed literal and every OTHER literal is a negated
/// symbol-headed literal (all subject to the same `atom_of` restrictions as
/// the `(=>)` arm: arguments are variables or symbol constants only).  A
/// clause `¬a1 ∨ … ∨ ¬an ∨ c` is exactly the Horn rule `c :- a1, …, an`.
///
/// Returns `None` (and bumps the matching `stats` counter) for: zero
/// positive literals (all-negative — a denial/goal clause, not extracted
/// here), two-or-more positive literals (non-Horn), or a literal whose atom
/// isn't representable as a Datalog atom.
fn horn_rule_of_or(
    syn:   &SyntacticLayer,
    root:  SentenceId,
    s:     &Sentence,
    stats: &mut ExtractStats,
) -> Option<Rule> {
    let lit_ids: Vec<SentenceId> = s.elements[1..].iter().filter_map(sub).collect();
    if lit_ids.len() != s.elements.len() - 1 {
        // A disjunct that isn't a Sub (shouldn't happen post-CAF, but keep
        // this sound rather than silently dropping a literal).
        stats.or_non_datalog_skipped += 1;
        return None;
    }

    let mut vars: HashMap<SymbolId, u32> = HashMap::new();
    let mut literals: Vec<Literal> = Vec::with_capacity(lit_ids.len());
    for lid in &lit_ids {
        match literal_of(syn, *lid, &mut vars) {
            Some(l) => literals.push(l),
            None => {
                stats.or_non_datalog_skipped += 1;
                return None;
            }
        }
    }

    let positives: Vec<usize> = literals.iter().enumerate()
        .filter(|(_, l)| !l.negated)
        .map(|(i, _)| i)
        .collect();
    match positives.len() {
        0 => {
            // All-negative: a denial/integrity-constraint or a goal clause
            // (e.g. a `negated_conjecture` unit), not a definite Horn rule.
            // Left for resolution / the denials channel.
            stats.or_all_negative_skipped += 1;
            None
        }
        1 => {
            let head_idx = positives[0];
            let head = literals[head_idx].atom.clone();
            // Every other literal was NEGATED in the clause (¬ai) — the
            // clause ¬a1 ∨ … ∨ ¬an ∨ c is (a1 ∧ … ∧ an) → c, so each ai
            // becomes a POSITIVE rule-body premise, not a negated one.
            let body: Vec<Literal> = literals.into_iter().enumerate()
                .filter(|(i, _)| *i != head_idx)
                .map(|(_, mut l)| { l.negated = false; l })
                .collect();
            stats.or_rules += 1;
            Some(Rule { head, body, sid: Some(root) })
        }
        _ => {
            // ≥2 positive literals: non-Horn (a real disjunctive head).
            stats.or_non_horn_skipped += 1;
            None
        }
    }
}

/// Everything one extraction scan recovers: the Horn program, the clausal
/// arm's skip statistics, and the SKIPPED-HEAD bookkeeping the completion
/// certifier (`super::certify`) consumes.  A relation appearing in
/// `skipped_heads` has a potential definition the program did NOT capture,
/// so model-absence says nothing about it — it must never be
/// completion-certified.
#[derive(Debug, Clone, Default)]
pub(crate) struct Extraction {
    pub(crate) program: Program,
    pub(crate) stats:   ExtractStats,
    /// Relations a SKIPPED root might still derive atoms of:
    ///   * skipped `(=> ant con)` → the head positions of `con`'s subtree
    ///     (just `con`'s head when it is a flat atom);
    ///   * skipped `(or …)` → every positive literal's head (a negated
    ///     flat-atom disjunct derives nothing positively);
    ///   * skipped `(not <flat atom>)` → nothing (a denial);
    ///   * every OTHER skipped shape (`<=>`, quantified, `and`-roots,
    ///     non-Datalog facts, malformed) → EVERY symbol in head position
    ///     anywhere in the sentence — erring toward NOT certifying.
    pub(crate) skipped_heads: HashSet<SymbolId>,
    /// A skipped root had a VARIABLE in head position (a predicate-variable
    /// shape like `(?REL a b)`): such a root could derive atoms of ANY
    /// relation, so certification must be refused wholesale.
    pub(crate) wildcard_skip: bool,
}

/// Extract the Horn / definite fragment of the stored axioms as a Datalog(¬)
/// program: implication-shaped roots become rules, ground symbol atoms become
/// EDB facts, and (Milestone C) clausal `(or …)` roots with exactly one
/// positive symbol-headed literal become rules too — a clause
/// `¬a1 ∨ … ∨ ¬an ∨ c` IS the Horn rule `c :- a1, …, an`.  This is what makes
/// extraction dialect-blind: TPTP CNF input and post-clausification FOF both
/// store their clauses as `(or …)` roots (see `extract_horn_program_stats`'s
/// doc for the shapes actually observed), so without this arm the extracted
/// program was structurally empty on the whole TPTP corpus.  Non-Datalog
/// roots (function-term args, non-Horn disjunctive heads, quantifier
/// structure beyond the implicit top-level `forall` already stripped at
/// ingest) are skipped — they remain for resolution.
pub(crate) fn extract_horn_program(syn: &SyntacticLayer) -> Program {
    extract_horn_program_full(syn).program
}

/// As [`extract_horn_program`], but also returns the [`ExtractStats`]
/// breakdown of the clausal arm's skip reasons (SIGMA_STATS-style
/// instrumentation; zero effect on the returned program).
pub(crate) fn extract_horn_program_stats(syn: &SyntacticLayer) -> (Program, ExtractStats) {
    let ex = extract_horn_program_full(syn);
    (ex.program, ex.stats)
}

/// Base-scope visibility for whole-KB extraction: `root_sids` and
/// `by_head_id` span every session's transient sentences, but everything
/// extracted in this module is cached once (`ModelRegistry`) and consulted
/// by every asking scope — only base sentences (no session owners, or
/// promoted axioms) may contribute, or one session's staged facts/rules
/// become model support in another session's proof.
fn base_visible(syn: &SyntacticLayer, sid: SentenceId) -> bool {
    let owners = syn.sessions.sessions_of(sid);
    owners.is_empty() || syn.sessions.is_axiom(sid)
}

/// The full extraction: program + stats + the skipped-head set for the
/// completion certifier.  See [`Extraction`].
pub(crate) fn extract_horn_program_full(syn: &SyntacticLayer) -> Extraction {
    let mut ex = Extraction::default();

    for root in syn.root_sids() {
        // Base-only: `root_sids` spans every session's transient
        // sentences, but the extracted program is cached whole-KB
        // (`ModelRegistry`) and consulted by every asking scope — a
        // session-staged fact or rule must not become model support in
        // another session's proof.  Session facts still reach the prover
        // through its support roots and the scope-filtered `store_facts`.
        if !base_visible(syn, root) {
            continue;
        }
        let Some(s) = syn.sentence(root) else { continue };
        match s.op() {
            // Rule: (=> ant con)
            Some(&OpKind::Implies) if s.elements.len() == 3 => {
                match implies_rule(syn, root, &s) {
                    Some(rule) => ex.program.rules.push(rule),
                    None => {
                        // Certification bookkeeping (a): this skipped root
                        // could derive atoms of its consequent — collect the
                        // consequent subtree's head positions (for a flat
                        // atom that is exactly its head; for a complex
                        // consequent, everything head-positioned inside it).
                        match sub(&s.elements[2]) {
                            Some(cid) => {
                                ex.wildcard_skip |=
                                    collect_head_positions(syn, cid, &mut ex.skipped_heads);
                            }
                            None => {
                                ex.wildcard_skip |=
                                    collect_head_positions(syn, root, &mut ex.skipped_heads);
                            }
                        }
                    }
                }
            }
            // Clause: (or l1 … lk) — CNF input / clausified FOF.  Exactly one
            // positive symbol-headed literal ⇒ a Horn rule (see
            // `horn_rule_of_or`); all-negative or ≥2-positive are skipped.
            Some(&OpKind::Or) if s.elements.len() >= 2 => {
                match horn_rule_of_or(syn, root, &s, &mut ex.stats) {
                    Some(rule) => ex.program.rules.push(rule),
                    None => {
                        // Certification bookkeeping (a): every POSITIVE
                        // disjunct's head could be derived by this clause.
                        collect_or_positive_heads(syn, root, &s, &mut ex);
                    }
                }
            }
            // A single-literal negative clause `(not (rel c1 … ck))` — ground
            // (a negative fact, e.g. a one-literal `negated_conjecture`, or
            // SUMO's own `(not (member Denmark EuropeanMonetaryUnion))`) or
            // with variables (an irreflexivity axiom like
            // `(not (colleague ?A ?A ?ORG))`).  Not an EDB fact (no negative
            // facts in Datalog(¬)) and not a rule — a denial/goal shape.
            // Left for resolution.
            Some(&OpKind::Not) if s.elements.len() == 2 => {
                ex.stats.negative_unit_skipped += 1;
                // A negated FLAT atom derives nothing positively (a pure
                // denial).  Any more complex inner shape could (classically
                // `¬(p ⇒ q) ⊨ p`) — conservative collection.
                match sub(&s.elements[1]).and_then(|i| syn.sentence(i)) {
                    Some(inner) if inner.op().is_none() && inner.head_symbol().is_some() => {}
                    _ => {
                        ex.wildcard_skip |=
                            collect_head_positions(syn, root, &mut ex.skipped_heads);
                    }
                }
            }
            // Fact: a ground symbol-headed atom.
            None => {
                let mut vars = HashMap::new();
                match atom_of(&s, &mut vars) {
                    Some((atom, true)) => {
                        let tuple: Vec<SymbolId> = atom.args.iter().filter_map(|a| match a {
                            DTerm::Const(c) => Some(*c),
                            DTerm::Var(_) => None,
                        }).collect();
                        if tuple.len() == atom.args.len() {
                            ex.program.fact_src(atom.pred, tuple, root);
                        } else {
                            // Unreachable when ground, but stay conservative.
                            if let Some(h) = s.head_symbol() {
                                ex.skipped_heads.insert(h);
                            }
                        }
                    }
                    // A non-ground atom root (`(p ?X)` asserts p of
                    // everything) or one with a compound / literal argument
                    // the model cannot represent: p's definition escapes
                    // the program — certification bookkeeping (a).
                    _ => match s.head_symbol() {
                        Some(h) => { ex.skipped_heads.insert(h); }
                        None => {
                            ex.wildcard_skip |=
                                collect_head_positions(syn, root, &mut ex.skipped_heads);
                        }
                    },
                }
            }
            // Every other root shape (`<=>`, quantified, `and`-roots,
            // malformed operator arities): nothing is extracted, and the
            // sentence could derive atoms of anything head-positioned in it
            // — conservative collection (certification bookkeeping (a)).
            _ => {
                ex.wildcard_skip |= collect_head_positions(syn, root, &mut ex.skipped_heads);
            }
        }
    }

    ex
}

/// The `(=> ant con)` extraction arm of [`extract_horn_program_full`],
/// factored out so a failed extraction (`None`) can funnel into the
/// skipped-head bookkeeping.  Byte-identical logic to the historical
/// inline arm.
fn implies_rule(syn: &SyntacticLayer, root: SentenceId, s: &Sentence) -> Option<Rule> {
    let (ant_id, con_id) = (sub(&s.elements[1])?, sub(&s.elements[2])?);
    let (ant, con) = (syn.sentence(ant_id)?, syn.sentence(con_id)?);

    // Head must be a (positive) symbol-headed atom.
    if con.op().is_some() {
        return None; // disjunctive / negative / equality head: not definite
    }
    let mut vars: HashMap<SymbolId, u32> = HashMap::new();

    // Body literals (process first so positive body vars index low).
    let body_ids: Vec<SentenceId> = if ant.op() == Some(&OpKind::And) {
        ant.elements[1..].iter().filter_map(sub).collect()
    } else {
        vec![ant_id]
    };
    let mut body = Vec::with_capacity(body_ids.len());
    for bid in body_ids {
        body.push(literal_of(syn, bid, &mut vars)?);
    }
    let (head, _) = atom_of(&con, &mut vars)?;
    Some(Rule { head, body, sid: Some(root) })
}

/// Skipped-`(or …)` head bookkeeping: collect the head of every POSITIVE
/// disjunct (a negated flat-atom disjunct derives nothing positively).
/// Falls back to whole-root [`collect_head_positions`] on any disjunct
/// whose shape is not a (possibly negated) flat symbol-headed atom.
fn collect_or_positive_heads(
    syn:  &SyntacticLayer,
    root: SentenceId,
    s:    &Sentence,
    ex:   &mut Extraction,
) {
    for el in &s.elements[1..] {
        match el {
            Element::Sub(lid) => {
                let Some(lit) = syn.sentence(*lid) else {
                    ex.wildcard_skip |=
                        collect_head_positions(syn, root, &mut ex.skipped_heads);
                    return;
                };
                if lit.op() == Some(&OpKind::Not) && lit.elements.len() == 2 {
                    // Negative disjunct: nothing derivable — unless the
                    // negated body is itself complex.
                    match sub(&lit.elements[1]).and_then(|i| syn.sentence(i)) {
                        Some(inner) if inner.op().is_none()
                            && inner.head_symbol().is_some() => {}
                        _ => {
                            ex.wildcard_skip |=
                                collect_head_positions(syn, *lid, &mut ex.skipped_heads);
                        }
                    }
                } else if let Some(h) = lit.head_symbol() {
                    ex.skipped_heads.insert(h); // positive literal's head
                } else {
                    ex.wildcard_skip |=
                        collect_head_positions(syn, *lid, &mut ex.skipped_heads);
                }
            }
            // A bare-symbol disjunct is a positive nullary atom.
            Element::Symbol(sym) => { ex.skipped_heads.insert(sym.id()); }
            // Variable / literal / operator disjuncts: unrecognizable —
            // whole-root conservative collection.
            _ => {
                ex.wildcard_skip |=
                    collect_head_positions(syn, root, &mut ex.skipped_heads);
                return;
            }
        }
    }
}

/// Walk a sentence subtree collecting every symbol in HEAD position (the
/// predicate seat of any sub-sentence) — the conservative potential-head
/// set for a skipped root of unrecognized shape.  Quantifier variable
/// lists (`(forall (?X ?Y) …)`) are NOT descended into — they are binder
/// syntax, not applied atoms.  Returns `true` when a real head position
/// holds a VARIABLE (a predicate-variable application like `(?REL a b)`):
/// such a sentence could derive atoms of ANY relation, and the caller
/// must poison certification wholesale.
fn collect_head_positions(
    syn:  &SyntacticLayer,
    root: SentenceId,
    out:  &mut HashSet<SymbolId>,
) -> bool {
    let mut var_head = false;
    let mut seen: HashSet<SentenceId> = HashSet::new();
    let mut stack: Vec<SentenceId> = vec![root];
    while let Some(sid) = stack.pop() {
        if !seen.insert(sid) {
            continue;
        }
        let Some(s) = syn.sentence(sid) else { continue };
        match s.elements.first() {
            Some(Element::Symbol(sym)) => { out.insert(sym.id()); }
            Some(Element::Variable { .. }) => var_head = true,
            _ => {}
        }
        // Skip a quantifier's variable-list element (elements[1]).
        let skip_from = match s.op() {
            Some(&OpKind::ForAll) | Some(&OpKind::Exists) => 2,
            _ => 0,
        };
        for (i, el) in s.elements.iter().enumerate() {
            if i < skip_from && i == 1 {
                continue;
            }
            if let Element::Sub(sub_id) = el {
                stack.push(*sub_id);
            }
        }
    }
    var_head
}

// ---------------------------------------------------------------------------
// Phase 3.5 — meta-relation schema instantiation (generalized).
// ---------------------------------------------------------------------------
//
// SUMO/Cyc state algebraic properties of a relation by *declaring its role*
// (`(instance R TransitiveRelation)`, `(subrelation R S)`, …) rather than by a
// first-order rule, because the first-order form `(=> (and (?R x y) (?R y z))
// (?R x z))` has a predicate variable and is not first-order.  A naive Horn
// extractor therefore misses transitivity/symmetry entirely (so e.g. the
// extracted `subclass` model has no transitive closure).
//
// This is the generalization of `prover::synthesize_subrelation_rules` (which
// instantiates ONLY `subrelation R S → S(x,y):-R(x,y)`) to the full role set,
// emitting Datalog rules for the model generator:
//
//   subrelation R S            →  S(x,y)  :- R(x,y)
//   instance R TransitiveRel   →  R(x,z)  :- R(x,y), R(y,z)
//   instance R SymmetricRel    →  R(y,x)  :- R(x,y)
//
// Dialect-agnostic: the role symbols come from the recognized [`TaxonomyRoles`]
// (recovered by shape), never from hard-coded names.

/// The relations bearing each algebraic role, gathered from the store's
/// declarations.  Each entry carries the declaring sentence id, so the
/// instantiated schema rule can cite the declaration in a proof.
#[derive(Debug, Default, Clone)]
pub(crate) struct RoleDecls {
    /// `(subrelation R S)` pairs (sub `R`, super `S`) + the declaring root.
    pub subrelation: Vec<(SymbolId, SymbolId, Option<SentenceId>)>,
    /// Relations declared `(instance R TransitiveRelation)` + the declaration.
    pub transitive:  Vec<(SymbolId, Option<SentenceId>)>,
    /// Relations declared `(instance R SymmetricRelation)` + the declaration.
    pub symmetric:   Vec<(SymbolId, Option<SentenceId>)>,
}

/// A binary symbol-headed fact's two symbol arguments.
fn binary_syms(syn: &SyntacticLayer, sid: SentenceId) -> Option<(SymbolId, SymbolId)> {
    let s = syn.sentence(sid)?;
    if s.elements.len() != 3 {
        return None;
    }
    match (&s.elements[1], &s.elements[2]) {
        (Element::Symbol(a), Element::Symbol(b)) => Some((a.id(), b.id())),
        _ => None,
    }
}

/// Scan the store for role declarations under the recognized role symbols.
pub(crate) fn collect_role_decls(syn: &SyntacticLayer, roles: &TaxonomyRoles) -> RoleDecls {
    let mut d = RoleDecls::default();
    for sid in syn.by_head_id(&roles.subrelation) {
        if let Some((r, s)) = binary_syms(syn, sid) {
            if r != s {
                d.subrelation.push((r, s, Some(sid)));
            }
        }
    }
    for sid in syn.by_head_id(&roles.instance) {
        if let Some((r, c)) = binary_syms(syn, sid) {
            if c == roles.transitive {
                d.transitive.push((r, Some(sid)));
            } else if c == roles.symmetric {
                d.symmetric.push((r, Some(sid)));
            }
        }
    }
    d
}

/// Instantiate the role declarations into first-order Datalog rules — the
/// generalized schema expander.  `extra_transitive` lets a caller add
/// relations whose transitivity is known by other means (e.g. the recognized
/// taxonomy roles `subclass`/`subrelation`, transitive by construction, or
/// relations found transitive through the relation-class hierarchy), each
/// with the sentence to cite for that knowledge (or `None`).  Every emitted
/// rule carries its declaring sentence as `sid`, so a proof using the rule
/// can cite the declaration it was instantiated from.
pub(crate) fn schema_rules(
    decls:            &RoleDecls,
    extra_transitive: &[(SymbolId, Option<SentenceId>)],
) -> Vec<Rule> {
    let app = |p: SymbolId, a: u32, b: u32| Atom { pred: p, args: vec![DTerm::Var(a), DTerm::Var(b)] };
    let mut out = Vec::new();

    for &(r, s, sid) in &decls.subrelation {
        // S(x,y) :- R(x,y)
        out.push(Rule {
            head: app(s, 0, 1),
            body: vec![Literal { atom: app(r, 0, 1), negated: false }],
            sid,
        });
    }
    for &(r, sid) in decls.transitive.iter().chain(extra_transitive.iter()) {
        // R(x,z) :- R(x,y), R(y,z)
        out.push(Rule {
            head: Atom { pred: r, args: vec![DTerm::Var(0), DTerm::Var(2)] },
            body: vec![
                Literal { atom: app(r, 0, 1), negated: false },
                Literal { atom: app(r, 1, 2), negated: false },
            ],
            sid,
        });
    }
    for &(r, sid) in &decls.symmetric {
        // R(y,x) :- R(x,y)
        out.push(Rule {
            head: app(r, 1, 0),
            body: vec![Literal { atom: app(r, 0, 1), negated: false }],
            sid,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Denial constraints (negatives package, sub-milestone B).
// ---------------------------------------------------------------------------
//
// SUMO/Cyc declare class DISJOINTNESS — `(disjoint A B)`, and pairwise over
// the tails of `(partition C P1 … Pn)` / `(disjointDecomposition C P1 … Pn)` —
// rather than writing the first-order `∀x.¬(instance x A ∧ instance x B)`.
// Extracted here as ⊥-rules ("denials"): integrity constraints the chase uses
// to REFUTE a ground atom (`ModelProgram::refutes`).  Open-world sound:
// KB ⊨ ¬(instance x C) iff KB ∪ {(instance x C)} is inconsistent, and a
// denial-pair hit inside the model's closure is exactly that inconsistency
// for the Horn+denial fragment — no Clark completion, no CWA.
//
// This mirrors how the taxonomy oracle's `DisjointSets::build` reads the
// declarations (recognized `disjoint`/`partition` role ids, plus the
// hash-named `disjointDecomposition`, whose row-variable defining axiom has
// no recognizer).  `exhaustiveDecomposition` is NOT disjoint — excluded.

/// One extracted denial constraint: the (normalized `min ≤ max`) class pair
/// declared disjoint, and the declaring root sentence — the axiom a
/// refutation cites as its final step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Denial {
    pub classes: (SymbolId, SymbolId),
    pub sid:     SentenceId,
}

/// Scan the store for disjointness declarations under the recognized role
/// symbols and flatten them to pairwise [`Denial`]s.  First declaration of a
/// pair wins (matching the oracle's `src.entry().or_insert` behavior).
pub(crate) fn collect_denials(syn: &SyntacticLayer, roles: &TaxonomyRoles) -> Vec<Denial> {
    let norm = |a: SymbolId, b: SymbolId| if a <= b { (a, b) } else { (b, a) };
    let mut seen: std::collections::HashSet<(SymbolId, SymbolId)> =
        std::collections::HashSet::new();
    let mut out: Vec<Denial> = Vec::new();
    let mut push = |a: SymbolId, b: SymbolId, sid: SentenceId, out: &mut Vec<Denial>| {
        if a != b && seen.insert(norm(a, b)) {
            out.push(Denial { classes: norm(a, b), sid });
        }
    };

    // (disjoint A B)
    for sid in syn.by_head_id(&roles.disjoint) {
        if let Some((a, b)) = binary_syms(syn, sid) {
            push(a, b, sid, &mut out);
        }
    }
    // (partition C P1 … Pn) / (disjointDecomposition C P1 … Pn): the tail
    // members are pairwise disjoint.
    let dd = crate::types::Symbol::hash_name("disjointDecomposition");
    let mut heads = vec![roles.partition];
    if dd != roles.partition {
        heads.push(dd);
    }
    for head in heads {
        for sid in syn.by_head_id(&head) {
            let Some(s) = syn.sentence(sid) else { continue };
            let parts: Vec<SymbolId> = s
                .elements
                .iter()
                .skip(2)
                .filter_map(|e| match e {
                    Element::Symbol(sym) => Some(sym.id()),
                    _ => None,
                })
                .collect();
            for i in 0..parts.len() {
                for j in (i + 1)..parts.len() {
                    push(parts[i], parts[j], sid, &mut out);
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// EGDs — equality-generating dependencies (task #32, seminaive package).
// ---------------------------------------------------------------------------
//
// A binary FD-style uniqueness constraint on a relation: `R`'s `val_pos`
// argument is functionally determined by its `key_pos` argument.  Mined from
// the same two axiom families the prover's FD congruence recognizes
// (`mine_fd_relations` in `prover/mod.rs` — shape logic mirrored here, not
// imported: extraction works on stored ROOTS, the miner on clauses, and the
// layer direction forbids the import):
//
//   1. uniqueness clauses, both stored shapes:
//        `(=> (and (R u v1) (R u v2) [guards…]) (equal v1 v2))`
//        `(or (not (R u v1)) (not (R u v2)) [(not guard)…] (equal v1 v2))`
//      where guards are instance-typing conjuncts `(instance x C)` over the
//      key / equated variables ONLY (anything else skips the sentence);
//   2. `(instance R SingleValuedRelation)` declarations — arg1 determines
//      arg2, unguarded.
//
// The kernel fires an EGD when two stored tuples share the key value with
// distinct value reps: it UNIONS the two values in the evaluation's equality
// classes (`seminaive::EqClasses`), recording the justification edge.

/// One mined EGD.  `key_pos` / `val_pos` are 0-based positions into the
/// model [`Tuple`] (the argument vector — note the spec text's `key_pos=1,
/// val_pos=2` figures are the 1-based FdDecl argument-numbering convention;
/// tuples here carry no relation seat, so the same positions are 0/1).
///
/// DESIGN DELTA (soundness): the spec's `Egd` had no guard fields and said
/// to IGNORE instance-typing guards.  Ignoring a guard applies the FD to
/// keys/values outside the guarded class — merges the KB does not entail
/// (TQG14's `part` axiom guarded by `AtomicNucleus` would merge parts of
/// arbitrary wholes).  Instead the guards are KEPT and enforced at fire
/// time against the store's current `instance` facts — an
/// under-approximation of the instance closure, so the EGD can only
/// under-fire (sound).  Guards over any variable other than key/v1/v2
/// still skip the sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Egd {
    pub rel:        super::Pred,
    pub key_pos:    u8,
    pub val_pos:    u8,
    /// Instance-typing guards on the KEY variable (classes it must belong to).
    pub key_guards: Vec<SymbolId>,
    /// Instance-typing guards on the equated VALUE variables (the axiom
    /// constrains both symmetrically; asymmetric guards skip the sentence).
    pub val_guards: Vec<SymbolId>,
    /// The declaring root — the uniqueness clause / `SingleValuedRelation`
    /// declaration a merge cites.
    pub sid:        Option<SentenceId>,
}

/// One (possibly negated) literal of a candidate uniqueness sentence,
/// classified: a binary same-relation atom over two variables, an
/// instance-typing guard, or the positive `(equal v1 v2)` head.
enum UniqLit {
    /// `(R ?x ?y)` — (rel, arg1 var, arg2 var).
    Rel(SymbolId, SymbolId, SymbolId),
    /// `(instance ?x C)` — (var, class).
    Guard(SymbolId, SymbolId),
    /// `(equal ?a ?b)` — (a, b).
    Equal(SymbolId, SymbolId),
}

/// Classify one flat sentence as a [`UniqLit`] body atom (rel atom or
/// instance guard).  `None` for any other shape.
fn uniq_body_atom(s: &Sentence, instance: SymbolId) -> Option<UniqLit> {
    if s.elements.len() != 3 {
        return None;
    }
    let Some(Element::Symbol(h)) = s.elements.first() else { return None };
    match (&s.elements[1], &s.elements[2]) {
        (Element::Variable { id: x, .. }, Element::Symbol(class)) if h.id() == instance => {
            Some(UniqLit::Guard(*x, class.id()))
        }
        (Element::Variable { id: x, .. }, Element::Variable { id: y, .. }) => {
            Some(UniqLit::Rel(h.id(), *x, *y))
        }
        _ => None,
    }
}

/// Classify a flat sentence as the positive equality head `(equal ?a ?b)`.
fn uniq_equal(s: &Sentence) -> Option<UniqLit> {
    if s.elements.len() != 3 || !matches!(s.elements.first(), Some(Element::Op(OpKind::Equal))) {
        return None;
    }
    let (Element::Variable { id: a, .. }, Element::Variable { id: b, .. }) =
        (&s.elements[1], &s.elements[2]) else { return None };
    (a != b).then_some(UniqLit::Equal(*a, *b))
}

/// Orient a classified uniqueness sentence into an [`Egd`]: exactly two
/// same-relation binary atoms sharing a key variable at one position with
/// the equated pair at the other, guards only over key / equated variables,
/// and symmetric guard sets on the two equated sides.  Mirrors
/// `mine_fd_relations`' orientation logic exactly.
fn orient_egd(lits: Vec<UniqLit>, root: SentenceId) -> Option<Egd> {
    let mut rel_atoms: Vec<(SymbolId, SymbolId, SymbolId)> = Vec::new();
    let mut guards: Vec<(SymbolId, SymbolId)> = Vec::new();
    let mut eq: Option<(SymbolId, SymbolId)> = None;
    for l in lits {
        match l {
            UniqLit::Rel(r, x, y) => rel_atoms.push((r, x, y)),
            UniqLit::Guard(v, c) => guards.push((v, c)),
            UniqLit::Equal(a, b) => {
                if eq.is_some() {
                    return None; // two equality heads: not this shape
                }
                eq = Some((a, b));
            }
        }
    }
    let (va, vb) = eq?;
    if rel_atoms.len() != 2 || rel_atoms[0].0 != rel_atoms[1].0 {
        return None;
    }
    let rel = rel_atoms[0].0;
    let ((_, x1, y1), (_, x2, y2)) = (rel_atoms[0], rel_atoms[1]);
    let (key_pos, val_pos, key_var) = if x1 == x2 && [y1, y2].contains(&va) && [y1, y2].contains(&vb) {
        (0u8, 1u8, x1)
    } else if y1 == y2 && [x1, x2].contains(&va) && [x1, x2].contains(&vb) {
        (1u8, 0u8, y1)
    } else {
        return None;
    };
    let key_guards: Vec<SymbolId> =
        guards.iter().filter(|(v, _)| *v == key_var).map(|(_, c)| *c).collect();
    let val_guards_a: Vec<SymbolId> =
        guards.iter().filter(|(v, _)| *v == va).map(|(_, c)| *c).collect();
    let val_guards_b: Vec<SymbolId> =
        guards.iter().filter(|(v, _)| *v == vb).map(|(_, c)| *c).collect();
    // Sound only when both equated sides carry the SAME guard set.
    let mut ga = val_guards_a.clone();
    ga.sort_unstable();
    let mut gb = val_guards_b;
    gb.sort_unstable();
    if ga != gb {
        return None;
    }
    // Guards on unrelated variables make the axiom more restrictive than
    // this check — skip the sentence.
    if guards.iter().any(|(v, _)| *v != key_var && *v != va && *v != vb) {
        return None;
    }
    Some(Egd { rel, key_pos, val_pos, key_guards, val_guards: val_guards_a, sid: Some(root) })
}

/// Scan the store for EGD-shaped axioms (see the module note above):
/// uniqueness clauses in both stored shapes, plus
/// `(instance R SingleValuedRelation)` declarations.
pub(crate) fn collect_egds(syn: &SyntacticLayer, roles: &TaxonomyRoles) -> Vec<Egd> {
    let mut out: Vec<Egd> = Vec::new();
    let single_valued = crate::types::Symbol::hash_name("SingleValuedRelation");

    // -- Declaration form (keyed like the miner: arg1 determines arg2).
    for sid in syn.by_head_id(&roles.instance) {
        if !base_visible(syn, sid) {
            continue; // a session-staged FD must not merge terms elsewhere
        }
        if let Some((r, c)) = binary_syms(syn, sid) {
            if c == single_valued {
                out.push(Egd {
                    rel: r, key_pos: 0, val_pos: 1,
                    key_guards: Vec::new(), val_guards: Vec::new(),
                    sid: Some(sid),
                });
            }
        }
    }

    // -- Uniqueness-clause forms.
    for root in syn.root_sids() {
        if !base_visible(syn, root) {
            continue; // same base-only rule as the program extraction
        }
        let Some(s) = syn.sentence(root) else { continue };
        let lits: Option<Vec<UniqLit>> = match s.op() {
            // (=> (and B1 … Bn) (equal v1 v2))
            Some(&OpKind::Implies) if s.elements.len() == 3 => (|| {
                let ant = syn.sentence(sub(&s.elements[1])?)?;
                let con = syn.sentence(sub(&s.elements[2])?)?;
                let head = uniq_equal(&con)?;
                let body_ids: Vec<SentenceId> = if ant.op() == Some(&OpKind::And) {
                    ant.elements[1..].iter().filter_map(sub).collect()
                } else {
                    vec![sub(&s.elements[1])?]
                };
                let mut lits = Vec::with_capacity(body_ids.len() + 1);
                for bid in body_ids {
                    let bs = syn.sentence(bid)?;
                    lits.push(uniq_body_atom(&bs, roles.instance)?);
                }
                lits.push(head);
                Some(lits)
            })(),
            // (or (not B1) … (not Bn) (equal v1 v2))
            Some(&OpKind::Or) if s.elements.len() >= 3 => (|| {
                let mut lits = Vec::with_capacity(s.elements.len() - 1);
                for el in &s.elements[1..] {
                    let lid = sub(el)?;
                    let lit = syn.sentence(lid)?;
                    if lit.op() == Some(&OpKind::Not) && lit.elements.len() == 2 {
                        let inner = syn.sentence(sub(&lit.elements[1])?)?;
                        lits.push(uniq_body_atom(&inner, roles.instance)?);
                    } else {
                        lits.push(uniq_equal(&lit)?);
                    }
                }
                Some(lits)
            })(),
            _ => None,
        };
        // Mirror the miner's literal-count window (2 rel atoms + guards + eq).
        if let Some(lits) = lits {
            if (3..=8).contains(&lits.len()) {
                if let Some(egd) = orient_egd(lits, root) {
                    out.push(egd);
                }
            }
        }
    }
    out
}

/// The KIF numeric-literal shape at the symbol level (mirrors
/// `parse::kif::tokenizer::is_numeric`, which is private to the tokenizer):
/// optional leading `-`, ASCII digits, at most one `.`.
pub(crate) fn symbol_is_numeric(name: &str) -> bool {
    let s = name.strip_prefix('-').unwrap_or(name);
    if s.is_empty() {
        return false;
    }
    let mut has_dot = false;
    for ch in s.chars() {
        if ch == '.' {
            if has_dot {
                return false;
            }
            has_dot = true;
        } else if !ch.is_ascii_digit() {
            return false;
        }
    }
    true
}

/// RIGID symbols of a program: constants whose interned name is a numeric
/// literal.  Two distinct rigid symbols denote distinct values, so an EGD
/// union over them is an inconsistency (`ModelError::Inconsistent`), never a
/// merge.  (Numeric tokens normally parse to `Element::Literal` and never
/// enter the model at all — this catches dialects/fixtures that intern
/// numeric-shaped names as symbols.)
pub(crate) fn collect_rigid(
    program: &Program,
    syn:     &SyntacticLayer,
) -> crate::prover::saturate::hash64::Set64<SymbolId> {
    let mut universe: HashSet<SymbolId> = HashSet::new();
    for rows in program.edb.values() {
        for t in rows {
            universe.extend(t.iter().copied());
        }
    }
    for r in &program.rules {
        for a in std::iter::once(&r.head).chain(r.body.iter().map(|l| &l.atom)) {
            for arg in &a.args {
                if let DTerm::Const(c) = arg {
                    universe.insert(*c);
                }
            }
        }
    }
    universe
        .into_iter()
        .filter(|id| syn.sym_name(*id).is_some_and(|s| symbol_is_numeric(&s.name())))
        .collect()
}

/// Derive the relations that bear an algebraic role from an *evaluated* model,
/// rather than reading declarations directly.  A relation `R` is transitive
/// iff `(R, TransitiveRelation)` is in the **instance closure** — which already
/// climbs the relation-class hierarchy via the instance/subclass bridge, so
/// this subsumes (a) direct `(instance R TransitiveRelation)` declarations,
/// (b) inherited ones (`(instance R PartialOrderingRelation)` +
/// `(subclass PartialOrderingRelation TransitiveRelation)`), and (c) the
/// recognized taxonomy roles — with no hard-coded seed.  Feed the result back
/// as `schema_rules`' `extra_transitive` and re-evaluate to a fixpoint (a
/// newly-transitive relation can deepen the subclass closure, revealing more).
fn role_members(model: &super::Model, roles: &TaxonomyRoles, role_class: SymbolId) -> Vec<SymbolId> {
    let Some(inst) = model.get(&roles.instance) else { return Vec::new() };
    inst.iter()
        .filter(|t| t.len() == 2 && t[1] == role_class)
        .map(|t| t[0])
        .collect()
}

/// Relations that are transitive in `model` (membership in `TransitiveRelation`
/// via the instance closure).
pub(crate) fn transitive_members(model: &super::Model, roles: &TaxonomyRoles) -> Vec<SymbolId> {
    role_members(model, roles, roles.transitive)
}

/// Relations that are symmetric in `model` (membership in `SymmetricRelation`).
pub(crate) fn symmetric_members(model: &super::Model, roles: &TaxonomyRoles) -> Vec<SymbolId> {
    role_members(model, roles, roles.symmetric)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantics::caches::test_support::{kif_layer, tptp_layer};
    use crate::types::Symbol;

    fn s(name: &str) -> SymbolId { Symbol::hash_name(name) }

    /// Find the one extracted rule whose head predicate is `head_pred`
    /// (panics if there isn't exactly one — tests target a single clause).
    fn only_rule_for<'a>(p: &'a Program, head_pred: SymbolId) -> &'a Rule {
        let matches: Vec<&Rule> = p.rules.iter().filter(|r| r.head.pred == head_pred).collect();
        assert_eq!(matches.len(), 1, "expected exactly one rule headed by {head_pred:?}, got {matches:?}");
        matches[0]
    }

    // -- (1) or-clause -> Horn rule, with sid -----------------------------
    // `(or (not (killed X Y)) (hates X Y))` is the clause ¬killed(X,Y) ∨
    // hates(X,Y) — exactly the Horn rule `hates(X,Y) :- killed(X,Y)`.
    #[test]
    fn or_clause_with_one_positive_becomes_horn_rule_with_sid() {
        let sem = tptp_layer(
            "cnf(killer_hates_victim, axiom, ( ~ killed(X,Y) | hates(X,Y) ) ).\n",
            "or_rule.p",
        );
        let (p, stats) = extract_horn_program_stats(&sem.syntactic);
        assert_eq!(stats.or_rules, 1, "one or-root extracted as a rule");
        assert_eq!(stats.or_non_horn_skipped, 0);
        assert_eq!(stats.or_all_negative_skipped, 0);

        let rule = only_rule_for(&p, s("hates"));
        assert!(rule.sid.is_some(), "rule cites its declaring root");
        assert_eq!(rule.head.pred, s("hates"));
        assert_eq!(rule.body.len(), 1);
        assert_eq!(rule.body[0].atom.pred, s("killed"));
        assert!(!rule.body[0].negated,
            "the clause's negated literal becomes a POSITIVE rule-body premise \
             (¬killed(X,Y) ∨ hates(X,Y)  ==  hates(X,Y) :- killed(X,Y))");
        // Head and body share the same two rule-local variables (X, Y),
        // just at different argument positions.
        let head_vars: Vec<u32> = rule.head.args.iter().map(|a| match a {
            DTerm::Var(v) => *v,
            DTerm::Const(_) => panic!("expected a variable"),
        }).collect();
        let body_vars: Vec<u32> = rule.body[0].atom.args.iter().map(|a| match a {
            DTerm::Var(v) => *v,
            DTerm::Const(_) => panic!("expected a variable"),
        }).collect();
        assert_eq!(head_vars, body_vars, "X,Y map to the same rule-local indices in head and body");
    }

    // The rule discharges correctly once evaluated: killed(a,b) entails
    // hates(a,b) via the extracted clause, exactly as the source problem
    // intends ("a killer always hates his victim").
    #[test]
    fn or_clause_rule_evaluates_correctly() {
        let sem = tptp_layer(
            "cnf(killer_hates_victim, axiom, ( ~ killed(X,Y) | hates(X,Y) ) ).\n\
             cnf(fact1, axiom, killed(agatha,agatha) ).\n",
            "or_rule_eval.p",
        );
        let p = extract_horn_program(&sem.syntactic);
        let model = p.evaluate().expect("stratifiable");
        let t = vec![s("agatha"), s("agatha")];
        assert!(model.get(&s("hates")).is_some_and(|rows| rows.contains(&t)));
    }

    // -- (2) >= 2 positive literals: skipped (non-Horn) -------------------
    // `(or (not (lives X)) (richer X agatha) (hates butler X))` has TWO
    // positive literals (richer, hates) — not a definite clause.
    #[test]
    fn or_clause_with_two_positive_literals_is_skipped() {
        let sem = tptp_layer(
            "cnf(butler_hates_poor, axiom, \
                ( ~ lives(X) | richer(X,agatha) | hates(butler,X) ) ).\n",
            "non_horn.p",
        );
        let (p, stats) = extract_horn_program_stats(&sem.syntactic);
        assert_eq!(stats.or_non_horn_skipped, 1);
        assert_eq!(stats.or_rules, 0);
        assert!(p.rules.is_empty(), "non-Horn clause contributes no rule");
    }

    // A disjunction with two positives and zero negatives (e.g. a
    // `negated_conjecture` goal clause) is equally non-Horn.
    #[test]
    fn or_clause_all_positive_is_skipped_as_non_horn() {
        let sem = tptp_layer(
            "cnf(goal, negated_conjecture, ( killed(butler,agatha) | killed(charles,agatha) ) ).\n",
            "all_pos.p",
        );
        let (p, stats) = extract_horn_program_stats(&sem.syntactic);
        assert_eq!(stats.or_non_horn_skipped, 1);
        assert!(p.rules.is_empty());
    }

    // -- (3) all-negative: skipped (denial / goal shape) -------------------
    // `(or (not (hates agatha X)) (not (hates charles X)))` has ZERO
    // positive literals — a denial-shaped clause, not a definite rule.
    #[test]
    fn or_clause_all_negative_is_skipped() {
        let sem = tptp_layer(
            "cnf(different_hates, axiom, ( ~ hates(agatha,X) | ~ hates(charles,X) ) ).\n",
            "all_neg.p",
        );
        let (p, stats) = extract_horn_program_stats(&sem.syntactic);
        assert_eq!(stats.or_all_negative_skipped, 1);
        assert_eq!(stats.or_rules, 0);
        assert!(p.rules.is_empty(), "all-negative clause contributes no rule");
    }

    // A ground single-literal negative unit clause `(not (rel c1 … ck))`
    // stores as a bare `Not`-headed root (no `Or` wrapper) — also skipped,
    // not treated as a (nonexistent) "negative EDB fact".
    #[test]
    fn ground_negative_unit_clause_is_skipped() {
        let sem = tptp_layer(
            "cnf(not_butler, axiom, ~ equal(agatha,butler) ).\n",
            "ground_neg_unit.p",
        );
        let (p, stats) = extract_horn_program_stats(&sem.syntactic);
        assert_eq!(stats.negative_unit_skipped, 1);
        assert!(p.rules.is_empty());
        assert!(p.edb.is_empty(), "no negative EDB facts are synthesized");
    }

    // Ground single-literal POSITIVE unit clauses still work as plain EDB
    // facts (pre-existing behavior, unaffected by the or-clause arm).
    #[test]
    fn ground_positive_unit_clause_is_still_a_fact() {
        let sem = tptp_layer("cnf(agatha, axiom, lives(agatha) ).\n", "ground_pos_unit.p");
        let (p, stats) = extract_horn_program_stats(&sem.syntactic);
        assert_eq!(stats.or_rules, 0);
        assert_eq!(stats.negative_unit_skipped, 0);
        assert!(p.edb.get(&s("lives")).is_some_and(|rows| rows.contains(&vec![s("agatha")])));
    }

    // -- (4) TPTP CNF end-to-end: PUZ001-1.p yields a non-empty program ----
    // with correctly-shaped rules (loads the real problem file when $TPTP is
    // set; otherwise uses an inline excerpt covering the same clause shapes
    // so the test still exercises the extractor without a filesystem
    // dependency).
    #[test]
    fn tptp_cnf_end_to_end_yields_nonempty_program_with_horn_rules() {
        let text = match std::env::var("TPTP") {
            Ok(tptp) => {
                let path = format!("{tptp}/Problems/PUZ/PUZ001-1.p");
                std::fs::read_to_string(&path).unwrap_or_else(|e| {
                    panic!("$TPTP set but failed to read {path}: {e}")
                })
            }
            Err(_) => {
                // Inline excerpt of PUZ001-1.p covering unit facts, a
                // single-positive Horn clause, a >=2-positive non-Horn
                // clause, and an all-negative clause.
                "cnf(agatha, hypothesis, lives(agatha) ).\n\
                 cnf(killer_hates_victim, hypothesis, ( ~ killed(X,Y) | hates(X,Y) ) ).\n\
                 cnf(butler_hates_poor, hypothesis, \
                    ( ~ lives(X) | richer(X,agatha) | hates(butler,X) ) ).\n\
                 cnf(different_hates, hypothesis, ( ~ hates(agatha,X) | ~ hates(charles,X) ) ).\n"
                    .to_string()
            }
        };
        let sem = tptp_layer(&text, "PUZ001-1.p");
        let (p, stats) = extract_horn_program_stats(&sem.syntactic);

        assert!(!p.rules.is_empty(), "PUZ001-1.p must yield a non-empty Horn program");
        assert!(stats.or_rules > 0, "at least one or-clause extracted as a rule");
        // Every extracted rule is well-shaped: a symbol-headed atom head, a
        // body of symbol-headed literals, and a citation back to its
        // declaring root.
        for rule in &p.rules {
            assert!(rule.sid.is_some(), "extracted rule cites its source sentence");
            for lit in &rule.body {
                assert!(!lit.atom.args.is_empty() || lit.atom.args.is_empty(), "atom shape sane");
            }
        }
        // The known Horn clause from this problem: hates(X,Y) :- killed(X,Y)
        // ("a killer always hates his victim").  Real PUZ001-1.p has a
        // second `hates`-headed rule too (`same_hates`), so look for the one
        // whose body is `killed` rather than assuming a unique head.
        let killer_rule = p.rules.iter()
            .find(|r| r.head.pred == s("hates") && r.body.len() == 1 && r.body[0].atom.pred == s("killed"))
            .expect("killer_hates_victim clause extracted as a rule");
        assert!(!killer_rule.body[0].negated);

        // At least one ground fact from the unit clauses (`lives(agatha)`).
        assert!(p.edb.get(&s("lives")).is_some_and(|rows| rows.contains(&vec![s("agatha")])));
    }

    // -- Certification bookkeeping: the skipped-head set -------------------

    // A skipped non-Horn or-clause collects EVERY positive literal's head —
    // and only those (a negated flat-atom disjunct derives nothing
    // positively).
    #[test]
    fn skipped_or_root_collects_positive_heads_only() {
        let sem = tptp_layer(
            "cnf(butler_hates_poor, axiom, \
                ( ~ lives(X) | richer(X,agatha) | hates(butler,X) ) ).\n",
            "skip_heads.p",
        );
        let ex = extract_horn_program_full(&sem.syntactic);
        assert!(ex.skipped_heads.contains(&s("richer")));
        assert!(ex.skipped_heads.contains(&s("hates")));
        assert!(
            !ex.skipped_heads.contains(&s("lives")),
            "a negated disjunct derives nothing positively"
        );
        assert!(!ex.wildcard_skip);
    }

    // An all-negative (denial-shaped) clause and a ground negative unit
    // collect NO skipped heads; a fully-extracted KB collects none either.
    #[test]
    fn denial_shapes_and_extracted_roots_collect_no_heads() {
        let sem = tptp_layer(
            "cnf(different_hates, axiom, ( ~ hates(agatha,X) | ~ hates(charles,X) ) ).\n\
             cnf(killer_hates_victim, axiom, ( ~ killed(X,Y) | hates(X,Y) ) ).\n\
             cnf(fact1, axiom, killed(agatha,agatha) ).\n\
             cnf(not_rich, axiom, ~ richer(agatha,butler) ).\n",
            "no_skip_heads.p",
        );
        let ex = extract_horn_program_full(&sem.syntactic);
        assert!(
            ex.skipped_heads.is_empty(),
            "nothing derivable escaped extraction: {:?}",
            ex.skipped_heads
        );
        assert!(!ex.wildcard_skip);
        assert_eq!(ex.stats.or_rules, 1, "the Horn clause still extracts");
    }

    // A skipped `(=> ant con)` collects the consequent's head (a compound
    // argument blocked the head atom), NOT the antecedent relations.
    #[test]
    fn skipped_implies_collects_consequent_head_only() {
        let kif = "(=> (relative ?X ?Y) (grandparent ?X (MotherFn ?Y)))\n";
        let sem = kif_layer(kif);
        let ex = extract_horn_program_full(&sem.syntactic);
        assert!(ex.skipped_heads.contains(&s("grandparent")));
        assert!(
            !ex.skipped_heads.contains(&s("relative")),
            "an antecedent-only relation is not derivable from this root"
        );
        assert!(ex.program.rules.is_empty());
    }

    // A fact root the EDB cannot represent (a literal argument) collects
    // its own head — the relation's extension escapes the program.
    #[test]
    fn non_datalog_fact_root_collects_its_head() {
        let sem = kif_layer("(age Bob 40)\n(parent Alice Bob)\n");
        let ex = extract_horn_program_full(&sem.syntactic);
        assert!(ex.skipped_heads.contains(&s("age")));
        assert!(!ex.skipped_heads.contains(&s("parent")), "the clean fact extracted");
        assert!(ex.program.edb.contains_key(&s("parent")));
        assert!(!ex.program.edb.contains_key(&s("age")));
    }

    // An unrecognized root shape (an `exists` root) conservatively collects
    // every symbol in head position — and the quantifier's variable LIST is
    // binder syntax, not a predicate-variable application, so it must NOT
    // trip the wildcard poison.
    #[test]
    fn quantified_root_collects_heads_without_wildcard_poison() {
        let sem = kif_layer("(exists (?X) (secretFriend ?X Bob))\n");
        let ex = extract_horn_program_full(&sem.syntactic);
        assert!(ex.skipped_heads.contains(&s("secretFriend")));
        assert!(
            !ex.wildcard_skip,
            "a quantifier variable list is not a predicate-variable head"
        );
    }

    // -- KIF suite regression guard: the new or-arm must not fire on the
    // existing KIF (=>) extraction fixtures.  KIF axioms never store a
    // top-level `(or …)` root (SUMO's `.kif` corpus is written in `(=>)` /
    // fact form), so the new arm's hit count on a KIF-loaded KB must be zero.
    #[test]
    fn kif_extraction_unaffected_by_or_clause_arm() {
        let kif = "\
            (subclass Dog Animal)\n\
            (=> (instance ?X Dog) (instance ?X Animal))\n\
            (instance Rex Dog)\n";
        let sem = kif_layer(kif);
        let (p, stats) = extract_horn_program_stats(&sem.syntactic);
        assert_eq!(stats.or_rules, 0, "no or-root exists in this KIF KB");
        assert_eq!(stats.or_all_negative_skipped, 0);
        assert_eq!(stats.or_non_horn_skipped, 0);
        assert_eq!(stats.negative_unit_skipped, 0);
        // The pre-existing (=>) extraction still works unchanged.
        assert_eq!(p.rules.len(), 1);
        assert!(p.edb.get(&s("instance")).is_some_and(|rows| rows.contains(&vec![s("Rex"), s("Dog")])));
    }
}


