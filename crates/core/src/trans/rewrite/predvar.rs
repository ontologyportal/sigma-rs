use std::collections::HashSet;

#[cfg(feature = "ask")]
use smallvec::smallvec;

use crate::parse::ast::OpKind;
#[cfg(feature = "ask")]
use crate::types::TaxRelation;
use crate::trans::TranslationLayer;
#[cfg(feature = "ask")]
use crate::types::{ElementVec, InternedSym};
use crate::types::{Element, SentenceId, SymbolId};
use crate::syntactic::SyntacticLayer;
use super::preprocess::decompose_implication;
use super::extract::var_appears_as_predicate;
use super::augment::collect_conjuncts;
#[cfg(feature = "ask")]
use super::augment::substitute_var;

// ---------------------------------------------------------------------------
// Stage 4 — Predicate-variable instantiation (per-problem, lazy)
// ---------------------------------------------------------------------------

/// A taxonomy guard constraining a schema's predicate variables.
// Constructed by `detect_predvar_schemas` (live without `ask`); the fields are
// only READ by the `ask`-gated instantiation path below.
#[cfg_attr(not(feature = "ask"), allow(dead_code))]
#[derive(Debug, Clone, Copy)]
pub(crate) enum PvGuard {
    /// `(instance ?V class)` — `?V` must be an instance of `class`.
    Instance { var: SymbolId, class: SymbolId },
    /// `(subrelation ?V1 ?V2)` — `?V1`,`?V2` bound by a subrelation fact.
    Subrelation { v1: SymbolId, v2: SymbolId },
}

/// A predicate-variable schema template: an implication whose antecedent
/// contains *taxonomy guards* (`instance`/`subrelation` atoms) over one or
/// more predicate variables, where every non-guard atom is headed by one of
/// those predicate variables.  Instantiated lazily per query by binding the
/// predicate variables to concrete relations satisfying **all** guards.
///
/// Examples (post-CAF, post-row-var-expansion):
///   - transitivity: `(=> (instance ?R TransitiveRelation) (forall … (=> (and (?R …)(?R …)) (?R …))))`
///   - subrelation propagation: `(=> (and (subrelation ?R1 ?R2)(instance ?R1 Predicate)(instance ?R2 Predicate)(?R1 …)) (?R2 …))`
// Constructed by `detect_predvar_schemas` (live without `ask`); most fields are
// only READ by the `ask`-gated instantiation path below.
#[cfg_attr(not(feature = "ask"), allow(dead_code))]
#[derive(Debug, Clone)]
pub(crate) struct PredVarSchema {
    /// The (CAF-normalized) implication carrying the schema.
    pub schema_sid: SentenceId,
    /// The predicate variables (head-position), in a deterministic order.
    pub pred_vars:  Vec<SymbolId>,
    /// The taxonomy guards constraining `pred_vars`.
    guards:         Vec<PvGuard>,
    /// The arity at which the predicate variables are *applied* in the schema
    /// body (row-var expansion stamps each variant at a fixed arity).  A
    /// concrete relation is only eligible for this variant when its declared
    /// arity matches; variadic / unknown arities accept any variant.
    pub body_arity: Option<usize>,
}

/// The taxonomy relation symbol ids used in guards (`instance`, `subrelation`).
fn taxonomy_guard_ids(syntactic: &SyntacticLayer) -> (Option<SymbolId>, Option<SymbolId>) {
    (syntactic.sym_id("instance"), syntactic.sym_id("subrelation"))
}

/// Scan `implications` for predicate-variable schemas (see [`PredVarSchema`]).
pub(crate) fn detect_predvar_schemas(
    syntactic:    &SyntacticLayer,
    implications: &[SentenceId],
) -> Vec<PredVarSchema> {
    let mut out = Vec::new();
    let (instance_id, subrelation_id) = taxonomy_guard_ids(syntactic);
    if instance_id.is_none() && subrelation_id.is_none() { return out; }
    let tax_ids: HashSet<SymbolId> = [instance_id, subrelation_id].into_iter().flatten().collect();

    for &sid in implications {
        let Some((ant_sid, _con)) = decompose_implication(syntactic, sid) else { continue };

        let mut guards: Vec<PvGuard> = Vec::new();
        let mut guard_vars: HashSet<SymbolId> = HashSet::new();
        for csid in collect_conjuncts(syntactic, ant_sid) {
            let Some(s) = syntactic.sentence(csid) else { continue };
            if s.elements.len() != 3 { continue; }
            let head = match s.elements.first() { Some(Element::Symbol(sym)) => sym.id(), _ => continue };
            if Some(head) == instance_id {
                if let (Some(Element::Variable { id: v, .. }), Some(Element::Symbol(c)))
                    = (s.elements.get(1), s.elements.get(2))
                {
                    guards.push(PvGuard::Instance { var: *v, class: c.id() });
                    guard_vars.insert(*v);
                }
            } else if Some(head) == subrelation_id {
                if let (Some(Element::Variable { id: v1, .. }), Some(Element::Variable { id: v2, .. }))
                    = (s.elements.get(1), s.elements.get(2))
                {
                    guards.push(PvGuard::Subrelation { v1: *v1, v2: *v2 });
                    guard_vars.insert(*v1);
                    guard_vars.insert(*v2);
                }
            }
        }
        if guards.is_empty() { continue; }

        // Predicate variables = guarded vars that appear in predicate-head
        // position.  Every guarded var must be a predicate var, else reject.
        let mut pred_vars: Vec<SymbolId> = guard_vars.iter().copied()
            .filter(|&v| var_appears_as_predicate(syntactic, sid, v))
            .collect();
        if pred_vars.len() != guard_vars.len() || pred_vars.is_empty() { continue; }
        pred_vars.sort_unstable();

        // Two accepted shapes: a single instance-guarded property schema, or
        // the two-variable subrelation-propagation shape.
        let has_subrel = guards.iter().any(|g| matches!(g, PvGuard::Subrelation { .. }));
        let ok_shape = if has_subrel { pred_vars.len() == 2 } else { pred_vars.len() == 1 };
        if !ok_shape { continue; }

        // Purity: every atom is either a taxonomy guard or headed by a
        // predicate variable (no other relation symbols, no `exists`).
        let pv_set: HashSet<SymbolId> = pred_vars.iter().copied().collect();
        if !is_pure_predvar_schema(syntactic, sid, &pv_set, &tax_ids) { continue; }

        let body_arity = predvar_call_arity(syntactic, sid, &pv_set);
        out.push(PredVarSchema { schema_sid: sid, pred_vars, guards, body_arity });
    }
    out
}

/// The argument count at which a predicate variable is applied in the tree
/// rooted at `sid` — the first `(?REL args…)` or `(holds ?REL args…)` atom
/// found.  Row-var expansion stamps each schema variant at one fixed arity, so
/// the first occurrence is representative.
fn predvar_call_arity(
    syntactic: &SyntacticLayer,
    sid:       SentenceId,
    pv_set:    &HashSet<SymbolId>,
) -> Option<usize> {
    let sentence = syntactic.sentence(sid)?;
    // Direct application: (?REL a b …).
    if matches!(sentence.elements.first(), Some(Element::Variable { id, .. }) if pv_set.contains(id)) {
        return Some(sentence.elements.len().saturating_sub(1));
    }
    // holds-style: (holds ?REL a b …).
    if let Some(holds_id) = syntactic.sym_id("holds") {
        if matches!(sentence.elements.first(), Some(Element::Symbol(sym)) if sym.id() == holds_id)
            && matches!(sentence.elements.get(1), Some(Element::Variable { id, .. }) if pv_set.contains(id))
        {
            return Some(sentence.elements.len().saturating_sub(2));
        }
    }
    sentence.elements.iter().find_map(|e| match e {
        Element::Sub(child) => predvar_call_arity(syntactic, *child, pv_set),
        _ => None,
    })
}

/// `true` iff every atom under `sid` is either a taxonomy guard (head in
/// `tax_ids`) or headed by a predicate variable in `pv_set`, combined only
/// with propositional connectives / `forall` (no `exists`, no other symbols).
fn is_pure_predvar_schema(
    syntactic: &SyntacticLayer,
    sid:       SentenceId,
    pv_set:    &HashSet<SymbolId>,
    tax_ids:   &HashSet<SymbolId>,
) -> bool {
    let Some(s) = syntactic.sentence(sid) else { return false };
    match s.elements.first() {
        // `forall` binds the tuple variables — recurse only into the body
        // (last Sub), not the bound-variable list (which structurally looks
        // like a predicate-var atom).
        Some(Element::Op(OpKind::ForAll)) => {
            match s.elements.last() {
                Some(Element::Sub(body)) =>
                    is_pure_predvar_schema(syntactic, *body, pv_set, tax_ids),
                _ => false,
            }
        }
        Some(Element::Op(OpKind::Exists)) => false,
        Some(Element::Op(_)) => s.elements.iter().all(|e| match e {
            Element::Sub(c) => is_pure_predvar_schema(syntactic, *c, pv_set, tax_ids),
            _ => true,
        }),
        // Predicate-variable atom `(?REL …)`.
        Some(Element::Variable { id, .. }) => pv_set.contains(id),
        // Taxonomy guard atom (`instance`/`subrelation` headed).
        Some(Element::Symbol(sym)) => tax_ids.contains(&sym.id()),
        _ => false,
    }
}

/// `true` iff `csid` is a taxonomy guard atom (`instance`/`subrelation`
/// headed) mentioning one of the predicate variables — i.e. a guard to drop
/// during instantiation (as opposed to a body atom).
#[cfg(feature = "ask")]
fn is_taxonomy_guard_atom(
    syntactic: &SyntacticLayer,
    csid:      SentenceId,
    pv_set:    &HashSet<SymbolId>,
    tax_ids:   &HashSet<SymbolId>,
) -> bool {
    let Some(s) = syntactic.sentence(csid) else { return false };
    if !matches!(s.elements.first(), Some(Element::Symbol(sym)) if tax_ids.contains(&sym.id())) {
        return false;
    }
    s.elements[1..].iter().any(|e| matches!(e, Element::Variable { id, .. } if pv_set.contains(id)))
}

/// `true` iff `sid` is a **bare positive relation assertion** — a (possibly
/// universally quantified) atom headed by a concrete relation symbol, with no
/// surrounding propositional structure (no `=>`, `not`, `and`, `or`).
///
/// Used to reject unsound instantiations: when a schema's antecedent is *only*
/// taxonomy guards, instantiation reduces the body to its consequent, and a
/// bare atom asserted for *all* arguments would hold the relation universally.
pub(crate) fn is_bare_positive_assertion(syntactic: &SyntacticLayer, sid: SentenceId) -> bool {
    let Some(s) = syntactic.sentence(sid) else { return false };
    match s.elements.first() {
        // Strip universal quantifiers — recurse into the body (last Sub).
        Some(Element::Op(OpKind::ForAll)) => match s.elements.last() {
            Some(Element::Sub(body)) => is_bare_positive_assertion(syntactic, *body),
            _ => false,
        },
        // A bare atom headed by a concrete relation symbol.
        Some(Element::Symbol(_)) => true,
        _ => false,
    }
}

/// Instantiate `schema` for one binding (`pred_var -> concrete relation`):
/// substitute every predicate variable, drop the taxonomy guard conjuncts,
/// and push the result as a synthetic implication (origin = the schema).
#[cfg(feature = "ask")]
fn instantiate_schema(
    syntactic: &SyntacticLayer,
    schema:    &PredVarSchema,
    binding:   &[(SymbolId, SymbolId)],
    tax_ids:   &HashSet<SymbolId>,
) -> Option<SentenceId> {
    let pv_set: HashSet<SymbolId> = schema.pred_vars.iter().copied().collect();
    let (ant_sid, con_sid) = decompose_implication(syntactic, schema.schema_sid)?;

    // Substitute the binding (all predicate vars) through a sentence.
    let subst_all = |syntactic: &SyntacticLayer, mut s: SentenceId| -> SentenceId {
        for &(var, rel) in binding {
            let rel_sym = syntactic.sym_name(rel).expect("bound relation symbol interned");
            let rel_elem = Element::Symbol(InternedSym(rel_sym));
            s = substitute_var(syntactic, s, var, &rel_elem, schema.schema_sid);
        }
        s
    };

    let mut new_conjuncts: Vec<SentenceId> = Vec::new();
    for csid in collect_conjuncts(syntactic, ant_sid) {
        if is_taxonomy_guard_atom(syntactic, csid, &pv_set, tax_ids) { continue; }
        new_conjuncts.push(subst_all(syntactic, csid));
    }
    let new_con = subst_all(syntactic, con_sid);

    let new_ant = match new_conjuncts.len() {
        // Guards-only antecedent → the body is the consequent; reject the
        // unsound case (see `is_bare_positive_assertion`).
        0 => {
            if is_bare_positive_assertion(syntactic, new_con) {
                return None;
            }
            return Some(new_con);
        }
        1 => new_conjuncts[0],
        _ => {
            let mut and_elems: ElementVec = ElementVec::with_capacity(new_conjuncts.len() + 1);
            and_elems.push(Element::Op(OpKind::And));
            for &c in &new_conjuncts {
                and_elems.push(Element::Sub(c));
            }
            syntactic.push_synthetic_sentence(and_elems, schema.schema_sid)
        }
    };

    let impl_elems: ElementVec = smallvec![
        Element::Op(OpKind::Implies),
        Element::Sub(new_ant),
        Element::Sub(new_con),
    ];
    Some(syntactic.push_synthetic_sentence(impl_elems, schema.schema_sid))
}

impl TranslationLayer {
    /// Lazily instantiate predicate-variable schemas for the relations that
    /// actually occur in `problem_sids` (conjecture + selected axioms +
    /// assertions).  Returns the synthetic implication sids to add to the
    /// problem's axiom set.
    ///
    /// For each detected schema guarded by `(instance ?REL C)` and each
    /// relation `R` in the problem that is an instance of `C`, emit the
    /// concrete rule (`?REL → R`, guard dropped) — e.g. `located` →
    /// `(=> (and (located ?A ?B) (located ?B ?C)) (located ?A ?C))`.  Results
    /// are memoized in `predvar_cache`, so each `(schema, R)` pair is built at
    /// most once across all queries.  Nothing is materialised for relations
    /// not in the problem, so the axiom set never explodes.
    ///
    /// `seed_sids` (the conjecture + session assertions) take **priority**:
    /// their relations are enumerated before any others, so when a schema's
    /// candidate count exceeds the per-problem cap the rules kept are the ones
    /// touching the query/assertions rather than arbitrary KB relations that
    /// happened to ride in on SInE selection.
    #[cfg(feature = "ask")]
    pub(crate) fn instantiate_predvars(
        &self,
        seed_sids:    &[SentenceId],
        problem_sids: &[SentenceId],
        scope:        crate::semantics::types::Scope,
    ) -> Vec<SentenceId> {
        let prog = self.rewrite_program();
        if prog.predvar_schemas.is_empty() { return Vec::new(); }
        // Suppress the schema templates (and their originals): per-problem
        // instantiations stand in for them.  Rule-source suppression stays
        // with `run_rewrite_pass`.
        {
            let mut suppressed = self.suppressed.write().unwrap();
            for sc in &prog.predvar_schemas {
                suppressed.insert(sc.schema_sid);
                if let Some(origin) =
                    self.semantic.syntactic.synthetic_origin.get(&sc.schema_sid).copied()
                {
                    suppressed.insert(origin);
                }
            }
        }

        // Iteration order = seed relations first (sorted), then every other
        // problem relation (sorted), so `find_predvar_bindings`'s cap keeps
        // the query-relevant rules.
        let mut seed_syms: HashSet<SymbolId> = HashSet::new();
        for &sid in seed_sids {
            seed_syms.extend(self.semantic.syntactic.sentence_symbols(sid));
        }
        let mut all_syms: HashSet<SymbolId> = seed_syms.clone();
        for &sid in problem_sids {
            all_syms.extend(self.semantic.syntactic.sentence_symbols(sid));
        }
        let mut ordered_syms: Vec<SymbolId> = seed_syms.iter().copied().collect();
        ordered_syms.sort_unstable();
        let mut rest: Vec<SymbolId> = all_syms.difference(&seed_syms).copied().collect();
        rest.sort_unstable();
        ordered_syms.extend(rest);

        let (instance_id, subrelation_id) = taxonomy_guard_ids(&self.semantic.syntactic);
        let tax_ids: HashSet<SymbolId> = [instance_id, subrelation_id].into_iter().flatten().collect();
        let cap = crate::syntactic::sine::scale_predvar_cap();
        let mut out: Vec<SentenceId> = Vec::new();

        for schema in &prog.predvar_schemas {
            let bindings = self.find_predvar_bindings(schema, &ordered_syms, &seed_syms, cap, scope);

            for binding in bindings {
                // binding[i] is the concrete relation for schema.pred_vars[i].
                let cached = self.predvar_cache.read().unwrap()
                    .get(&(schema.schema_sid, binding.clone())).copied();
                if let Some(cached) = cached {
                    self.predvar_instances.write().unwrap().insert(cached);
                    out.push(cached);
                    continue;
                }
                let pairs: Vec<(SymbolId, SymbolId)> = schema.pred_vars.iter()
                    .copied().zip(binding.iter().copied()).collect();
                if let Some(sid) = instantiate_schema(&self.semantic.syntactic, schema, &pairs, &tax_ids) {
                    self.predvar_cache.write().unwrap().insert((schema.schema_sid, binding), sid);
                    self.predvar_instances.write().unwrap().insert(sid);
                    out.push(sid);
                }
            }
        }
        crate::log!(Debug, "sigmakee_rs_core::trans", format!(
            "instantiate_predvars: {} schema(s), {} problem syms -> {} instantiated rule(s)",
            prog.predvar_schemas.len(), ordered_syms.len(), out.len()));
        out
    }

    /// Enumerate predicate-variable bindings (values aligned to
    /// `schema.pred_vars`) that satisfy every taxonomy guard, restricted to
    /// relations occurring in `problem_syms`.
    ///
    /// - If the schema has a `(subrelation ?V1 ?V2)` guard, that fact is the
    ///   driver: for each problem relation `R1` with a subrelation parent
    ///   `R2`, bind `(?V1,?V2) = (R1,R2)`.
    /// - Otherwise (instance guards only) each variable's domain is the
    ///   problem relations that are instances of its guard class, and the
    ///   bindings are the cross product.
    ///
    /// Every candidate is validated against *all* guards.  At most `cap`
    /// bindings are returned; when the candidate set is larger, bindings that
    /// touch a **seed symbol** (conjecture / assertion relations) are kept
    /// first.
    #[cfg(feature = "ask")]
    fn find_predvar_bindings(
        &self,
        schema:       &PredVarSchema,
        ordered_syms: &[SymbolId],
        seed_syms:    &HashSet<SymbolId>,
        cap:          usize,
        scope:        crate::semantics::types::Scope,
    ) -> Vec<Vec<SymbolId>> {
        use std::collections::HashMap;
        let pred_vars = &schema.pred_vars;

        // Candidate bindings as var->relation maps.
        let mut candidates: Vec<HashMap<SymbolId, SymbolId>> = Vec::new();

        let subrel_guard = schema.guards.iter().find_map(|g| match g {
            PvGuard::Subrelation { v1, v2 } => Some((*v1, *v2)),
            _ => None,
        });

        // Pre-sorted seed-first by the caller, so the `cap` cutoff keeps
        // query/assertion-relevant relations deterministically.
        let sorted_syms = ordered_syms;

        // A relation is eligible for this schema variant only at its declared
        // arity (row-var expansion stamps one arity per variant); variadic /
        // unknown-arity relations accept any variant.
        let arity_ok = |r: SymbolId| -> bool {
            match (schema.body_arity, self.semantic.arity(r)) {
                (Some(k), Some(a)) if a >= 0 => a as usize == k,
                _ => true, // no variant arity, variadic (-1), or unknown
            }
        };

        // Collection bound: well above `cap` so ranking (below) sees the full
        // realistic candidate set, but still a hard ceiling on cross-product
        // blowup for pathological schemas.
        let collect_max = cap.saturating_mul(16).max(cap);

        if let Some((gv1, gv2)) = subrel_guard {
            // Only the two-variable subrelation shape is handled here.
            if pred_vars.len() != 2 { return Vec::new(); }
            for &r1 in sorted_syms {
                if !arity_ok(r1) { continue; }
                let mut parents = self.semantic.parents_of_scoped(r1, scope);
                parents.sort_unstable_by_key(|(p, _)| *p);
                for (parent, rel) in parents {
                    if !matches!(rel, TaxRelation::Subrelation) { continue; }
                    let mut b = HashMap::new();
                    b.insert(gv1, r1);
                    b.insert(gv2, parent);
                    candidates.push(b);
                }
                if candidates.len() > collect_max { break; }
            }
        } else {
            // Instance-guard-only: per-var domain, cross product.
            let mut domains: Vec<(SymbolId, Vec<SymbolId>)> = Vec::new();
            for &v in pred_vars {
                let class = schema.guards.iter().find_map(|g| match g {
                    PvGuard::Instance { var, class } if *var == v => Some(*class),
                    _ => None,
                });
                let Some(class) = class else { return Vec::new() };
                let dom: Vec<SymbolId> = sorted_syms.iter().copied()
                    .filter(|&r| r != class
                        && arity_ok(r)
                        && self.reaches_via_instance_scoped(r, class, scope))
                    .collect();
                domains.push((v, dom));
            }
            // Cross product (bounded).
            candidates.push(HashMap::new());
            for (v, dom) in &domains {
                let mut next: Vec<HashMap<SymbolId, SymbolId>> = Vec::new();
                for base in &candidates {
                    for &r in dom {
                        let mut b = base.clone();
                        b.insert(*v, r);
                        next.push(b);
                        if next.len() > collect_max { break; }
                    }
                    if next.len() > collect_max { break; }
                }
                candidates = next;
            }
        }

        // Validate every candidate against all guards; dedup.
        let mut out: Vec<Vec<SymbolId>> = Vec::new();
        let mut seen: HashSet<Vec<SymbolId>> = HashSet::new();
        for b in candidates {
            if !self.binding_satisfies_guards(schema, &b, scope) { continue; }
            let key: Vec<SymbolId> = pred_vars.iter().map(|v| b[v]).collect();
            if seen.insert(key.clone()) {
                out.push(key);
            }
        }
        // Rank seed-touching bindings first (stable sort preserves the
        // seed-first symbol ordering within each group), then truncate to cap.
        out.sort_by_key(|b| !b.iter().any(|s| seed_syms.contains(s)));
        out.truncate(cap);
        out
    }

    /// `true` iff `binding` (var->relation) satisfies all of `schema`'s guards.
    #[cfg(feature = "ask")]
    fn binding_satisfies_guards(
        &self,
        schema:  &PredVarSchema,
        binding: &std::collections::HashMap<SymbolId, SymbolId>,
        scope:   crate::semantics::types::Scope,
    ) -> bool {
        schema.guards.iter().all(|g| match g {
            PvGuard::Instance { var, class } => binding.get(var)
                .map_or(false, |&r| self.reaches_via_instance_scoped(r, *class, scope)),
            PvGuard::Subrelation { v1, v2 } => {
                match (binding.get(v1), binding.get(v2)) {
                    (Some(&r1), Some(&r2)) => self.semantic.parents_of_scoped(r1, scope).into_iter()
                        .any(|(p, rel)| matches!(rel, TaxRelation::Subrelation) && p == r2),
                    _ => false,
                }
            }
        })
    }

    /// `sym` is an instance of `class` (directly or via a subclass),
    /// evaluated in `scope`: the
    /// `instance` edge and the subclass walk both see the session overlay,
    /// so a relation declared transitive inside a test session satisfies the
    /// schema guard.  `Base` scope is byte-identical to the unscoped form.
    #[cfg(feature = "ask")]
    fn reaches_via_instance_scoped(
        &self,
        sym:   SymbolId,
        class: SymbolId,
        scope: crate::semantics::types::Scope,
    ) -> bool {
        self.semantic.parents_of_scoped(sym, scope).into_iter().any(|(k, rel)| {
            matches!(rel, TaxRelation::Instance)
                && (k == class || self.semantic.has_ancestor_scoped(k, class, scope))
        })
    }
}

