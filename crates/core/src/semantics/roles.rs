//! Shape-recognized taxonomy roles — dialect-agnostic discovery of the
//! membership / subsumption / sub-relation operators and the
//! transitive / symmetric meta-classes, by matching their defining
//! axioms structurally rather than keying on hard-coded names.
//!
//! Both the semantic layer's taxonomy classification and the prover's
//! oracle route taxonomy decisions through these ids. This module
//! re-derives the ids from the axioms that define the roles, so a KB
//! that names `instance` differently still builds its taxonomy and
//! engages the oracle.
//!
//! Opt-in (`SIGMA_RECOGNIZE_ROLES` / `Strategy.recognize_roles`). When
//! off, and as the fallback for every role a scan fails to pin down, the
//! ids are `hash_name("instance")` &c.
//!
//! Recognition works on the parsed Sentence (KIF) tree, not the
//! clausified form: the keystone axioms for the meta-classes are
//! higher-order (`(?REL ?X ?Y)` — a variable applied as a relation) and
//! do not survive clausification.

use std::collections::HashMap;

use crate::semantics::taxonomy::TaxRelation;

thread_local! {
    /// Per-prove override for the disjointness-decomposition opt-in.
    /// `None` falls back to the `SIGMA_DISJOINT_DECOMP` env flag;
    /// `Some(b)` is the caller's decision.
    static DISJOINT_DECOMP_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// Sets (or clears with `None`) the per-prove disjointness-decomposition
/// decision. Pair with a reset guard so it never leaks across proves.
pub(crate) fn set_disjoint_decomp_override(v: Option<bool>) {
    DISJOINT_DECOMP_OVERRIDE.with(|c| c.set(v));
}

/// Returns whether the disjointness-decomposition capability is active
/// for the current prove: the override when set, else the env flag.
pub(crate) fn disjoint_decomp_active() -> bool {
    if let Some(b) = DISJOINT_DECOMP_OVERRIDE.with(|c| c.get()) {
        return b;
    }
    std::env::var_os("SIGMA_DISJOINT_DECOMP").is_some()
}
use crate::types::{Element, OpKind, Sentence, SentenceId, Symbol, SymbolId};

/// The five operator/class ids the oracle keys taxonomy reasoning on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TaxonomyRoles {
    pub instance:    SymbolId,
    pub subclass:    SymbolId,
    pub subrelation: SymbolId,
    pub transitive:  SymbolId,
    pub symmetric:   SymbolId,
    /// Argument-typing relations: `domain` is ternary `(domain R N C)`,
    /// `range` binary `(range R C)`.
    pub domain:      SymbolId,
    pub range:       SymbolId,
    /// Class-disjointness / exhaustive-decomposition operators. Only the
    /// binary `disjoint` and the exhaustive `partition` are recognized;
    /// row-variable `disjointDecomposition`/`exhaustiveDecomposition`
    /// stay on their global names.
    pub disjoint:    SymbolId,
    pub partition:   SymbolId,
}

impl Default for TaxonomyRoles {
    /// The hard-coded names used as the fallback.
    fn default() -> Self {
        Self {
            instance:    Symbol::hash_name("instance"),
            subclass:    Symbol::hash_name("subclass"),
            subrelation: Symbol::hash_name("subrelation"),
            transitive:  Symbol::hash_name("TransitiveRelation"),
            symmetric:   Symbol::hash_name("SymmetricRelation"),
            domain:      Symbol::hash_name("domain"),
            range:       Symbol::hash_name("range"),
            disjoint:    Symbol::hash_name("disjoint"),
            partition:   Symbol::hash_name("partition"),
        }
    }
}

impl TaxonomyRoles {
    /// Classifies a head symbol as a taxonomy edge relation against the
    /// recognized ids. Returns `None` for non-taxonomy heads (including
    /// `subAttribute`, which has no recognizer).
    pub(crate) fn classify(&self, head_id: SymbolId) -> Option<TaxRelation> {
        if head_id == self.instance {
            Some(TaxRelation::Instance)
        } else if head_id == self.subclass {
            Some(TaxRelation::Subclass)
        } else if head_id == self.subrelation {
            Some(TaxRelation::Subrelation)
        } else {
            None
        }
    }
}

/// A binary atom `(R a b)` reduced to its head id, head-is-variable
/// flag, and the two argument elements.
struct Binary<'a> {
    head:   SymbolId,
    is_var: bool,
    a:      &'a Element,
    b:      &'a Element,
}

/// Resolve a sentence by id through the syntactic store.
fn sent(syn: &crate::syntactic::SyntacticLayer, sid: SentenceId) -> Option<std::sync::Arc<Sentence>> {
    syn.sentence(sid)
}

/// Head id (+ whether it is a *variable* head, the higher-order case)
/// of an atom sentence, or `None` for an operator/quantifier sentence.
fn atom_head(s: &Sentence) -> Option<(SymbolId, bool)> {
    match s.elements.first()? {
        Element::Symbol(sym)      => Some((sym.id(), false)),
        Element::Variable { id, .. } => Some((*id, true)),
        _ => None,
    }
}

/// View a sentence as a binary atom `(R a b)`.
fn as_binary(s: &Sentence) -> Option<Binary<'_>> {
    if s.elements.len() != 3 {
        return None;
    }
    let (head, is_var) = atom_head(s)?;
    Some(Binary { head, is_var, a: &s.elements[1], b: &s.elements[2] })
}

/// Variable id of an element, if it is a variable.
fn var_id(e: &Element) -> Option<SymbolId> {
    match e {
        Element::Variable { id, .. } => Some(*id),
        _ => None,
    }
}

/// Constant-symbol id of an element, if it is a ground symbol.
fn sym_id(e: &Element) -> Option<SymbolId> {
    match e {
        Element::Symbol(s) => Some(s.id()),
        _ => None,
    }
}

/// The sub-sentence id an operator argument points at (`(and …)`,
/// `(R …)` children are stored as `Sub`).
fn sub_id(e: &Element) -> Option<SentenceId> {
    match e {
        Element::Sub(sid) => Some(*sid),
        _ => None,
    }
}

/// If `s` is `(=> ANT CON)`, return `(ant_sid, con_sid)`.
fn as_implies(s: &Sentence) -> Option<(SentenceId, SentenceId)> {
    if s.op() != Some(&OpKind::Implies) || s.elements.len() != 3 {
        return None;
    }
    Some((sub_id(&s.elements[1])?, sub_id(&s.elements[2])?))
}

/// If `s` is `(and A B)` with exactly two conjuncts, return `(a_sid, b_sid)`.
fn as_and2(s: &Sentence) -> Option<(SentenceId, SentenceId)> {
    if s.op() != Some(&OpKind::And) || s.elements.len() != 3 {
        return None;
    }
    Some((sub_id(&s.elements[1])?, sub_id(&s.elements[2])?))
}

/// If `s` is `(or A B)`, return `(a_sid, b_sid)`.
fn as_or2(s: &Sentence) -> Option<(SentenceId, SentenceId)> {
    if s.op() != Some(&OpKind::Or) || s.elements.len() != 3 {
        return None;
    }
    Some((sub_id(&s.elements[1])?, sub_id(&s.elements[2])?))
}

/// If `s` is `(and A B …)` (any arity ≥ 1), return the conjunct sids.
fn as_and_n(s: &Sentence) -> Option<Vec<SentenceId>> {
    if s.op() != Some(&OpKind::And) {
        return None;
    }
    s.elements[1..].iter().map(sub_id).collect()
}

/// If `s` is `(not A)`, return `a_sid`.
fn as_not(s: &Sentence) -> Option<SentenceId> {
    if s.op() != Some(&OpKind::Not) || s.elements.len() != 2 {
        return None;
    }
    sub_id(&s.elements[1])
}

/// If `s` is `(exists (vars) BODY)`, return `body_sid`.
fn as_exists(s: &Sentence) -> Option<SentenceId> {
    if s.op() != Some(&OpKind::Exists) || s.elements.len() != 3 {
        return None;
    }
    sub_id(&s.elements[2])
}

/// An n-ary atom `(H a1 … an)`: head id, head-is-variable, and the
/// argument variable ids (None if any argument is not a variable).
fn atom_var_args(s: &Sentence) -> Option<(SymbolId, bool, Vec<SymbolId>)> {
    let (head, is_var) = atom_head(s)?;
    let args: Option<Vec<SymbolId>> = s.elements[1..].iter().map(var_id).collect();
    Some((head, is_var, args?))
}

/// Strip a leading `(forall (vars) BODY)`, returning the body sid;
/// passes a non-quantified sentence through unchanged.
fn under_forall(s: &Sentence) -> Option<SentenceId> {
    if s.op() == Some(&OpKind::ForAll) && s.elements.len() == 3 {
        sub_id(&s.elements[2])
    } else {
        None
    }
}

impl TaxonomyRoles {
    /// Scans the background root sentences for the defining axioms of
    /// each role, overriding the default ids where a shape is matched.
    ///
    /// Order matters: the meta-class scans reuse the `instance` id the
    /// bridge scan recovers.
    pub(crate) fn recognize<I>(syn: &crate::syntactic::SyntacticLayer, roots: I) -> Self
    where
        I: IntoIterator<Item = SentenceId>,
    {
        let mut roles = TaxonomyRoles::default();
        let sids: Vec<SentenceId> = roots.into_iter().collect();

        // Head frequency over all roots: where a shape matches several
        // axioms, the most frequent head is the real role. Also makes
        // selection deterministic when `root_sids()` order is unspecified.
        let mut freq: HashMap<SymbolId, usize> = HashMap::new();
        for &sid in &sids {
            if let Some(s) = sent(syn, sid) {
                if let Some((h, false)) = atom_head(&s) {
                    *freq.entry(h).or_insert(0) += 1;
                }
            }
        }
        let score = |a: SymbolId, b: SymbolId| {
            freq.get(&a).copied().unwrap_or(0) + freq.get(&b).copied().unwrap_or(0)
        };

        // Pass 1 — the instance/subclass bridge pins both at once.
        let mut best_bridge: Option<(SymbolId, SymbolId, usize)> = None;
        for &sid in &sids {
            if let Some(s) = sent(syn, sid) {
                if let Some((inst, sub)) = match_bridge(syn, &s) {
                    let sc = score(inst, sub);
                    let take = match best_bridge {
                        None => true,
                        // Higher combined frequency wins; ties break on the
                        // (instance, subclass) id pair for determinism.
                        Some((i0, s0, b)) => sc > b || (sc == b && (inst, sub) < (i0, s0)),
                    };
                    if take {
                        best_bridge = Some((inst, sub, sc));
                    }
                }
            }
        }
        if let Some((inst, sub, _)) = best_bridge {
            roles.instance = inst;
            roles.subclass = sub;
        }

        // Pass 2 — meta-classes (need the recovered `instance`) and the
        // sub-relation operator. Higher frequency wins; ties break on
        // smaller id for determinism when frequency is uninformative.
        let better = |cand: SymbolId, cur: &Option<(SymbolId, usize)>, f: &HashMap<SymbolId, usize>| {
            let sc = f.get(&cand).copied().unwrap_or(0);
            match cur {
                None => true,
                Some((c0, b)) => sc > *b || (sc == *b && cand < *c0),
            }
        };
        let mut best_trans: Option<(SymbolId, usize)> = None;
        let mut best_sym: Option<(SymbolId, usize)> = None;
        let mut best_subrel: Option<(SymbolId, usize)> = None;
        let mut best_domain: Option<(SymbolId, usize)> = None;
        let mut best_range: Option<(SymbolId, usize)> = None;
        let mut best_disjoint: Option<(SymbolId, usize)> = None;
        let mut best_partition: Option<(SymbolId, usize)> = None;
        // The biconditional/forall `disjoint` form is gated so plain
        // recognition is unchanged.
        let decomp_disjoint = disjoint_decomp_active();
        for &sid in &sids {
            let Some(s) = sent(syn, sid) else { continue };
            if let Some(c) = match_meta_class(syn, &s, roles.instance, MetaShape::Transitive) {
                if better(c, &best_trans, &freq) { best_trans = Some((c, freq.get(&c).copied().unwrap_or(0))); }
            }
            if let Some(c) = match_meta_class(syn, &s, roles.instance, MetaShape::Symmetric) {
                if better(c, &best_sym, &freq) { best_sym = Some((c, freq.get(&c).copied().unwrap_or(0))); }
            }
            if let Some(sr) = match_subrelation(syn, &s) {
                if better(sr, &best_subrel, &freq) { best_subrel = Some((sr, freq.get(&sr).copied().unwrap_or(0))); }
            }
            if let Some(d) = match_typing_consistency(syn, &s, roles.subclass, 3) {
                if better(d, &best_domain, &freq) { best_domain = Some((d, freq.get(&d).copied().unwrap_or(0))); }
            }
            if let Some(r) = match_typing_consistency(syn, &s, roles.subclass, 2) {
                if better(r, &best_range, &freq) { best_range = Some((r, freq.get(&r).copied().unwrap_or(0))); }
            }
            if let Some(d) = match_disjoint(syn, &s, roles.instance)
                .or_else(|| match_disjoint_flat(syn, &s, roles.instance))
                .or_else(|| decomp_disjoint.then(|| match_disjoint_forall(syn, &s, roles.instance)).flatten())
            {
                if better(d, &best_disjoint, &freq) { best_disjoint = Some((d, freq.get(&d).copied().unwrap_or(0))); }
            }
            if let Some(p) = match_partition(syn, &s, roles.instance) {
                if better(p, &best_partition, &freq) { best_partition = Some((p, freq.get(&p).copied().unwrap_or(0))); }
            }
        }
        if let Some((c, _)) = best_trans     { roles.transitive  = c; }
        if let Some((c, _)) = best_sym       { roles.symmetric   = c; }
        if let Some((c, _)) = best_subrel    { roles.subrelation = c; }
        if let Some((c, _)) = best_domain    { roles.domain      = c; }
        if let Some((c, _)) = best_range     { roles.range       = c; }
        if let Some((c, _)) = best_disjoint  { roles.disjoint    = c; }
        if let Some((c, _)) = best_partition { roles.partition   = c; }
        roles
    }
}

/// `(=> (and (S ?x ?y) (I ?z ?x)) (I ?z ?y))` → `(instance = I, subclass = S)`.
///
/// The fingerprint: three first-order binary atoms; one relation `I`
/// occurs twice (sharing its first argument), the other `S` once, with
/// `S`'s arguments chaining the two `I` atoms' second arguments.  The
/// conjunct order is not assumed.
fn match_bridge(syn: &crate::syntactic::SyntacticLayer, s: &Sentence) -> Option<(SymbolId, SymbolId)> {
    let (ant, con) = as_implies(s)?;
    let ant_s = sent(syn, ant)?;
    let (l1, l2) = as_and2(&ant_s)?;
    let con_s = sent(syn, con)?;
    let con = as_binary(&con_s)?;
    let l1 = sent(syn, l1)?;
    let l2 = sent(syn, l2)?;
    let l1 = as_binary(&l1)?;
    let l2 = as_binary(&l2)?;
    if con.is_var {
        return None;
    }

    // Try each conjunct as the second `instance` atom.
    for (inst_ante, sub_ante) in [(&l1, &l2), (&l2, &l1)] {
        if inst_ante.is_var || sub_ante.is_var {
            continue;
        }
        if inst_ante.head != con.head {
            continue;
        }
        // The instance and subclass operators must be distinct relations,
        // else a transitivity axiom `(=> (and (R a b) (R c a)) (R c b))`
        // matches the bridge shape with `instance == subclass == R`.
        if con.head == sub_ante.head {
            continue;
        }
        let (z_con, x) = (var_id(con.a)?, var_id(con.b)?);
        let (z_ante, mid) = (var_id(inst_ante.a)?, var_id(inst_ante.b)?);
        if z_con != z_ante {
            continue;
        }
        // subclass atom (S mid x) chains the instance-antecedent's
        // second arg to the conclusion's second arg.
        let (s_a, s_b) = (var_id(sub_ante.a)?, var_id(sub_ante.b)?);
        if s_a == mid && s_b == x && mid != x {
            return Some((con.head, sub_ante.head));
        }
    }
    None
}

#[derive(Clone, Copy)]
enum MetaShape {
    Transitive,
    Symmetric,
}

/// `(=> (I ?rel ?class) [forall …] (=> BODY))` where BODY is the
/// transitivity / symmetry rule over the *variable-headed* `(?rel …)`
/// atoms → returns `?class`.  `inst` is the recovered `instance` id.
fn match_meta_class(
    syn:   &crate::syntactic::SyntacticLayer,
    s:     &Sentence,
    inst:  SymbolId,
    shape: MetaShape,
) -> Option<SymbolId> {
    let (ant, con) = as_implies(s)?;
    // Antecedent: (instance ?rel ?class), both arguments variables.
    let ant_s = sent(syn, ant)?;
    let ant = as_binary(&ant_s)?;
    if ant.is_var || ant.head != inst {
        return None;
    }
    let rel = var_id(ant.a)?;
    // The class endpoint is a ground constant (the meta-class itself).
    let class = sym_id(ant.b)?;

    // Consequent: optionally wrapped in (forall (…) …).
    let con_s = sent(syn, con)?;
    let body_sid = under_forall(&con_s).unwrap_or(con);
    let body = sent(syn, body_sid)?;
    let (rule_ant, rule_con) = as_implies(&body)?;

    match shape {
        MetaShape::Transitive => {
            // (=> (and (?rel a b) (?rel b c)) (?rel a c))
            let rule_ant_s = sent(syn, rule_ant)?;
            let (a1, a2) = as_and2(&rule_ant_s)?;
            let a1 = bind_var_atom(syn, a1, rel)?;
            let a2 = bind_var_atom(syn, a2, rel)?;
            let cc = bind_var_atom(syn, rule_con, rel)?;
            // a1=(a,b) a2=(b,c) con=(a,c): chain through b.
            if a1.1 == a2.0 && cc.0 == a1.0 && cc.1 == a2.1 && a1.0 != a1.1 {
                return Some(class);
            }
            None
        }
        MetaShape::Symmetric => {
            // (=> (?rel a b) (?rel b a))
            let a1 = bind_var_atom(syn, rule_ant, rel)?;
            let cc = bind_var_atom(syn, rule_con, rel)?;
            if a1.0 == cc.1 && a1.1 == cc.0 && a1.0 != a1.1 {
                return Some(class);
            }
            None
        }
    }
}

/// A variable-headed binary atom `(?rel a b)` whose head is exactly
/// `rel`, returned as its two argument variable ids.
fn bind_var_atom(
    syn: &crate::syntactic::SyntacticLayer,
    sid: SentenceId,
    rel: SymbolId,
) -> Option<(SymbolId, SymbolId)> {
    let s = sent(syn, sid)?;
    let b = as_binary(&s)?;
    if !b.is_var || b.head != rel {
        return None;
    }
    Some((var_id(b.a)?, var_id(b.b)?))
}

/// `(=> (and (SR ?r1 ?r2) … (?r1 @row)) (?r2 @row))` → `SR`.
///
/// The defining shape of the sub-relation operator: a first-order
/// binary atom `(SR ?r1 ?r2)` in the antecedent, a variable-headed
/// `(?r1 …)` conjunct, and a variable-headed conclusion `(?r2 …)` with
/// the same argument list.  Argument count is left open (row variable),
/// so we match on the head variables and identical argument vectors.
fn match_subrelation(syn: &crate::syntactic::SyntacticLayer, s: &Sentence) -> Option<SymbolId> {
    let (ant, con) = as_implies(s)?;
    let con = sent(syn, con)?;
    let (con_head, con_is_var) = atom_head(&con)?;
    if !con_is_var {
        return None;
    }
    let ant = sent(syn, ant)?;
    if ant.op() != Some(&OpKind::And) {
        return None;
    }
    let conjuncts: Vec<SentenceId> = ant.elements[1..].iter().filter_map(sub_id).collect();

    // The conclusion is (?r2 args). Find an antecedent var-headed
    // conjunct (?r1 args) with the same args, then find a first-order
    // binary (SR ?r1 ?r2) linking the two heads.
    for &cj in &conjuncts {
        let Some(cjs) = sent(syn, cj) else { continue };
        let Some((h, is_var)) = atom_head(&cjs) else { continue };
        if !is_var || h == con_head {
            continue;
        }
        if cjs.elements.len() != con.elements.len() {
            continue;
        }
        if cjs.elements[1..]
            .iter()
            .zip(con.elements[1..].iter())
            .any(|(p, q)| var_id(p) != var_id(q) || var_id(p).is_none())
        {
            continue;
        }
        let r1 = h;
        let r2 = con_head;
        for &dj in &conjuncts {
            let Some(djs) = sent(syn, dj) else { continue };
            let Some(b) = as_binary(&djs) else { continue };
            if b.is_var {
                continue;
            }
            if var_id(b.a) == Some(r1) && var_id(b.b) == Some(r2) {
                return Some(b.head);
            }
        }
    }
    None
}

/// The argument-typing "uniqueness up to subclass" axiom that defines
/// `domain` (arity 3) and `range` (arity 2):
///
/// ```text
/// (=> (and (D ?r ?n ?c1) (D ?r ?n ?c2)) (or (subclass ?c1 ?c2) (subclass ?c2 ?c1)))   ; domain
/// (=> (and (R ?r ?c1)    (R ?r ?c2))    (or (subclass ?c1 ?c2) (subclass ?c2 ?c1)))   ; range
/// ```
///
/// Matches two same-headed atoms of arity `arity`, agreeing on every
/// argument but the last (the class), whose consequent is the
/// `subclass` disjunction over the two differing classes; returns the
/// head. `subclass_id` is the recovered subclass relation.
fn match_typing_consistency(
    syn:         &crate::syntactic::SyntacticLayer,
    s:           &Sentence,
    subclass_id: SymbolId,
    arity:       usize,
) -> Option<SymbolId> {
    let (ant, con) = as_implies(s)?;
    let ant_s = sent(syn, ant)?;
    let (l1, l2) = as_and2(&ant_s)?;
    let l1_s = sent(syn, l1)?;
    let l2_s = sent(syn, l2)?;
    let (h1, v1, a1) = atom_var_args(&l1_s)?;
    let (h2, v2, a2) = atom_var_args(&l2_s)?;
    // Same first-order head, the target arity, all-variable arguments.
    if v1 || v2 || h1 != h2 || a1.len() != arity || a2.len() != arity {
        return None;
    }
    // Agree on the first arity-1 args, differ on the last (the class).
    if a1[..arity - 1] != a2[..arity - 1] {
        return None;
    }
    let (c1, c2) = (a1[arity - 1], a2[arity - 1]);
    if c1 == c2 {
        return None;
    }
    // Consequent: (or (subclass c1 c2) (subclass c2 c1)) in either order.
    let con_s = sent(syn, con)?;
    let (o1, o2) = as_or2(&con_s)?;
    let o1_s = sent(syn, o1)?;
    let o2_s = sent(syn, o2)?;
    let b1 = as_binary(&o1_s)?;
    let b2 = as_binary(&o2_s)?;
    if b1.is_var || b2.is_var || b1.head != subclass_id || b2.head != subclass_id {
        return None;
    }
    let (b1a, b1b) = (var_id(b1.a)?, var_id(b1.b)?);
    let (b2a, b2b) = (var_id(b2.a)?, var_id(b2.b)?);
    let forward  = b1a == c1 && b1b == c2 && b2a == c2 && b2b == c1;
    let backward = b1a == c2 && b1b == c1 && b2a == c1 && b2b == c2;
    (forward || backward).then_some(h1)
}

/// The class-disjointness operator, from its meaning axiom:
/// ```text
/// (=> (D ?c1 ?c2) (not (exists (?i) (and (instance ?i ?c1) (instance ?i ?c2)))))
/// ```
/// A binary `D` whose holding forbids a shared `instance` of the two
/// classes → returns `D`.  `inst` is the recovered `instance` id.
fn match_disjoint(
    syn:  &crate::syntactic::SyntacticLayer,
    s:    &Sentence,
    inst: SymbolId,
) -> Option<SymbolId> {
    let (ant, con) = as_implies(s)?;
    let ant_s = sent(syn, ant)?;
    let d = as_binary(&ant_s)?;
    if d.is_var {
        return None;
    }
    let (c1, c2) = (var_id(d.a)?, var_id(d.b)?);
    // (not (exists (?i) (and (instance ?i c1) (instance ?i c2))))
    let con_s = sent(syn, con)?;
    let exists_s = sent(syn, as_not(&con_s)?)?;
    let body_s = sent(syn, as_exists(&exists_s)?)?;
    let (a1, a2) = as_and2(&body_s)?;
    let a1_s = sent(syn, a1)?;
    let a2_s = sent(syn, a2)?;
    let a1 = as_binary(&a1_s)?;
    let a2 = as_binary(&a2_s)?;
    if a1.is_var || a2.is_var || a1.head != inst || a2.head != inst {
        return None;
    }
    let (i1, k1) = (var_id(a1.a)?, var_id(a1.b)?);
    let (i2, k2) = (var_id(a2.a)?, var_id(a2.b)?);
    let classes_ok = (k1 == c1 && k2 == c2) || (k1 == c2 && k2 == c1);
    (i1 == i2 && classes_ok && c1 != c2).then_some(d.head)
}

/// The class-disjointness operator in the flat universally-closed
/// De Morgan form (nothing is an instance of two disjoint classes):
/// ```text
/// (or (not (I ?o ?c1)) (not (I ?o ?c2)) (not (D ?c1 ?c2)))
/// ```
/// Two `instance` atoms share a subject and carry the two classes; a
/// first-order binary `D` relates those classes; returns `D`. `inst` is
/// the recovered `instance` id.
fn match_disjoint_flat(
    syn:  &crate::syntactic::SyntacticLayer,
    s:    &Sentence,
    inst: SymbolId,
) -> Option<SymbolId> {
    // Optional leading (forall (vars) …).
    let s = match under_forall(s) {
        Some(sid) => sent(syn, sid)?,
        None => std::sync::Arc::new(s.clone()),
    };
    if s.op() != Some(&OpKind::Or) {
        return None;
    }
    let disjuncts: Vec<SentenceId> = s.elements[1..].iter().filter_map(sub_id).collect();
    if disjuncts.len() != 3 {
        return None;
    }
    let mut inst_atoms: Vec<(SymbolId, SymbolId)> = Vec::new(); // (subject, class)
    let mut d_atoms: Vec<(SymbolId, SymbolId, SymbolId)> = Vec::new(); // (head, a, b)
    for &dj in &disjuncts {
        let dj_s = sent(syn, dj)?;
        let atom_s = sent(syn, as_not(&dj_s)?)?;
        let b = as_binary(&atom_s)?;
        if b.is_var {
            return None;
        }
        let (a, c) = (var_id(b.a)?, var_id(b.b)?);
        if b.head == inst {
            inst_atoms.push((a, c));
        } else {
            d_atoms.push((b.head, a, c));
        }
    }
    if inst_atoms.len() != 2 || d_atoms.len() != 1 {
        return None;
    }
    let ((i1, k1), (i2, k2)) = (inst_atoms[0], inst_atoms[1]);
    let (dh, da, db) = d_atoms[0];
    let classes_ok = (da == k1 && db == k2) || (da == k2 && db == k1);
    (i1 == i2 && k1 != k2 && classes_ok).then_some(dh)
}

/// The class-disjointness operator in the stored direction of the
/// biconditional-definition form:
/// ```text
/// (=> (D ?c1 ?c2) (forall (?i) (or (not (I ?i ?c1)) (not (I ?i ?c2)))))
/// ```
/// Returns `D`. `inst` is the recovered `instance` id.
fn match_disjoint_forall(
    syn:  &crate::syntactic::SyntacticLayer,
    s:    &Sentence,
    inst: SymbolId,
) -> Option<SymbolId> {
    let (ant, con) = as_implies(s)?;
    let ant_s = sent(syn, ant)?;
    let d = as_binary(&ant_s)?;
    if d.is_var {
        return None;
    }
    let (c1, c2) = (var_id(d.a)?, var_id(d.b)?);
    if c1 == c2 {
        return None;
    }
    // (forall (i) (or (not (I i c1)) (not (I i c2))))
    let con_s = sent(syn, con)?;
    let body = sent(syn, under_forall(&con_s)?)?;
    let (o1, o2) = as_or2(&body)?;
    let o1_s = sent(syn, o1)?;
    let o2_s = sent(syn, o2)?;
    let a1_s = sent(syn, as_not(&o1_s)?)?;
    let a2_s = sent(syn, as_not(&o2_s)?)?;
    let a1 = as_binary(&a1_s)?;
    let a2 = as_binary(&a2_s)?;
    if a1.is_var || a2.is_var || a1.head != inst || a2.head != inst {
        return None;
    }
    let (i1, k1) = (var_id(a1.a)?, var_id(a1.b)?);
    let (i2, k2) = (var_id(a2.a)?, var_id(a2.b)?);
    let classes_ok = (k1 == c1 && k2 == c2) || (k1 == c2 && k2 == c1);
    (i1 == i2 && classes_ok).then_some(d.head)
}

/// The exhaustive-decomposition operator, from `partition`'s fixed-arity
/// exhaustiveness axiom:
/// ```text
/// (=> (and (P ?s ?a ?b) (instance ?i ?s) (not (instance ?i ?a))) (instance ?i ?b))
/// ```
/// i.e. an instance of the super that is not in one block must be in the
/// other → returns the ternary `P`.  `inst` is the recovered `instance`.
fn match_partition(
    syn:  &crate::syntactic::SyntacticLayer,
    s:    &Sentence,
    inst: SymbolId,
) -> Option<SymbolId> {
    let (ant, con) = as_implies(s)?;
    let ant_s = sent(syn, ant)?;
    let conjuncts = as_and_n(&ant_s)?;
    if conjuncts.len() != 3 {
        return None;
    }
    // (instance ?i ?b)
    let con_s = sent(syn, con)?;
    let cc = as_binary(&con_s)?;
    if cc.is_var || cc.head != inst {
        return None;
    }
    let (i, b) = (var_id(cc.a)?, var_id(cc.b)?);

    // Locate the ternary P atom, the plain `(instance i s)`, and the
    // `(not (instance i a))` among the (order-free) conjuncts.
    let mut p_head = None;
    let mut p_args: Option<(SymbolId, SymbolId, SymbolId)> = None;
    let mut s_super = None;   // from (instance i s)
    let mut a_block = None;   // from (not (instance i a))
    for &cj in &conjuncts {
        let cjs = sent(syn, cj)?;
        if let Some(neg) = as_not(&cjs) {
            // (not (instance i a))
            let neg_s = sent(syn, neg)?;
            let na = as_binary(&neg_s)?;
            if na.is_var || na.head != inst || var_id(na.a)? != i {
                return None;
            }
            a_block = Some(var_id(na.b)?);
        } else if let Some((h, false, args)) = atom_var_args(&cjs) {
            if h == inst && args.len() == 2 && args[0] == i {
                s_super = Some(args[1]); // (instance i s)
            } else if args.len() == 3 {
                p_head = Some(h);
                p_args = Some((args[0], args[1], args[2]));
            } else {
                return None;
            }
        } else {
            return None;
        }
    }
    let (head, (ps, pa, pb)) = (p_head?, p_args?);
    // Line up: P(s,a,b), instance(i,s), ¬instance(i,a), instance(i,b).
    (s_super? == ps && a_block? == pa && pb == b).then_some(head)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntactic::SyntacticLayer;

    fn recognize_kif(kif: &str) -> TaxonomyRoles {
        let mut store = SyntacticLayer::default();
        store.load_kif(kif, "base");
        TaxonomyRoles::recognize(&store, store.root_sids())
    }

    #[test]
    fn recognizes_renamed_roles_from_defining_axioms() {
        // A dialect that renames every taxonomy operator/class.
        let roles = recognize_kif(
            "
            (=> (and (zsubc ?X ?Y) (zinst ?Z ?X)) (zinst ?Z ?Y))
            (=> (zinst ?REL zTransRel)
                (forall (?A ?B ?C)
                   (=> (and (?REL ?A ?B) (?REL ?B ?C)) (?REL ?A ?C))))
            (=> (zinst ?REL zSymRel)
                (forall (?A ?B) (=> (?REL ?A ?B) (?REL ?B ?A))))
            (=> (and (zsubrel ?R1 ?R2) (?R1 ?A ?B)) (?R2 ?A ?B))
            (=> (and (zdom ?R ?N ?C1) (zdom ?R ?N ?C2))
                (or (zsubc ?C1 ?C2) (zsubc ?C2 ?C1)))
            (=> (and (zrng ?R ?C1) (zrng ?R ?C2))
                (or (zsubc ?C1 ?C2) (zsubc ?C2 ?C1)))
            (=> (zdisj ?C1 ?C2)
                (not (exists (?I) (and (zinst ?I ?C1) (zinst ?I ?C2)))))
            (=> (and (zpart ?S ?A ?B) (zinst ?I ?S) (not (zinst ?I ?A)))
                (zinst ?I ?B))
            ",
        );
        assert_eq!(roles.instance,    Symbol::hash_name("zinst"));
        assert_eq!(roles.subclass,    Symbol::hash_name("zsubc"));
        assert_eq!(roles.subrelation, Symbol::hash_name("zsubrel"));
        assert_eq!(roles.transitive,  Symbol::hash_name("zTransRel"));
        assert_eq!(roles.symmetric,   Symbol::hash_name("zSymRel"));
        assert_eq!(roles.domain,      Symbol::hash_name("zdom"));
        assert_eq!(roles.range,       Symbol::hash_name("zrng"));
        assert_eq!(roles.disjoint,    Symbol::hash_name("zdisj"));
        assert_eq!(roles.partition,   Symbol::hash_name("zpart"));
    }

    #[test]
    fn standard_names_recover_the_defaults() {
        // Standard vocabulary reproduces the hard-coded ids.
        let roles = recognize_kif(
            "
            (=> (and (subclass ?X ?Y) (instance ?Z ?X)) (instance ?Z ?Y))
            (=> (instance ?REL TransitiveRelation)
                (forall (?A ?B ?C)
                   (=> (and (?REL ?A ?B) (?REL ?B ?C)) (?REL ?A ?C))))
            (=> (and (subrelation ?R1 ?R2) (?R1 ?A ?B)) (?R2 ?A ?B))
            (=> (and (domain ?R ?N ?C1) (domain ?R ?N ?C2))
                (or (subclass ?C1 ?C2) (subclass ?C2 ?C1)))
            (=> (and (range ?R ?C1) (range ?R ?C2))
                (or (subclass ?C1 ?C2) (subclass ?C2 ?C1)))
            (=> (disjoint ?C1 ?C2)
                (not (exists (?I) (and (instance ?I ?C1) (instance ?I ?C2)))))
            (=> (and (partition ?S ?A ?B) (instance ?I ?S) (not (instance ?I ?A)))
                (instance ?I ?B))
            ",
        );
        assert_eq!(roles.instance,    Symbol::hash_name("instance"));
        assert_eq!(roles.subclass,    Symbol::hash_name("subclass"));
        assert_eq!(roles.subrelation, Symbol::hash_name("subrelation"));
        assert_eq!(roles.transitive,  Symbol::hash_name("TransitiveRelation"));
        assert_eq!(roles.domain,      Symbol::hash_name("domain"));
        assert_eq!(roles.range,       Symbol::hash_name("range"));
        assert_eq!(roles.disjoint,    Symbol::hash_name("disjoint"));
        assert_eq!(roles.partition,   Symbol::hash_name("partition"));
        // No symmetry axiom present → falls back to the default.
        assert_eq!(roles.symmetric,   Symbol::hash_name("SymmetricRelation"));
    }

    #[test]
    fn recognizes_opencyc_dialect_bridge_and_flat_disjoint() {
        // The `isa`/`genls` bridge coexists with a `genls` transitivity
        // axiom (a degenerate bridge with one head), which must not
        // collapse instance==subclass; disjointness is the flat
        // universally-closed negation.
        let roles = recognize_kif(
            "
            (=> (and (isa ?x ?c1) (genls ?c1 ?c2)) (isa ?x ?c2))
            (=> (and (genls ?x ?y) (genls ?z ?x)) (genls ?z ?y))
            (not (and (isa ?o ?c1) (isa ?o ?c2) (disjointwith ?c1 ?c2)))
            ",
        );
        assert_eq!(roles.instance, Symbol::hash_name("isa"));
        assert_eq!(roles.subclass, Symbol::hash_name("genls"));
        assert_ne!(roles.instance, roles.subclass, "transitivity must not collapse the roles");
        assert_eq!(roles.disjoint, Symbol::hash_name("disjointwith"));
    }

    #[test]
    fn recognizes_disjoint_from_biconditional_forall_form() {
        // The `disjoint` definition in its forall form, stored as an
        // or-of-nots, which `match_disjoint_forall` must recover.
        std::env::set_var("SIGMA_DISJOINT_DECOMP", "1"); // the forall form is behind this flag
        let roles = recognize_kif(
            "
            (=> (and (zsubc ?X ?Y) (zinst ?Z ?X)) (zinst ?Z ?Y))
            (=> (zdisj ?c1 ?c2)
                (forall (?i) (not (and (zinst ?i ?c1) (zinst ?i ?c2)))))
            ",
        );
        std::env::remove_var("SIGMA_DISJOINT_DECOMP");
        assert_eq!(roles.instance, Symbol::hash_name("zinst"));
        assert_eq!(roles.disjoint, Symbol::hash_name("zdisj"));
    }

    #[test]
    fn no_taxonomy_axioms_yields_all_defaults() {
        let roles = recognize_kif("(p a b)\n(q c d)");
        assert_eq!(roles, TaxonomyRoles::default());
    }
}
