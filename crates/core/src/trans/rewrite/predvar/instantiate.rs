use std::collections::HashSet;

use smallvec::smallvec;

use super::reduce::{contains_head, reduce_root, ReduceSymbols};
use super::{
    atom_is_variables, is_bare_positive_assertion, Bound, PredVarSchema, PvGuard, SignatureKind,
};
use crate::parse::ast::OpKind;
use crate::semantics::consts::{INSTANCE_RELATION, SUBRELATION_RELATION};
use crate::semantics::types::{RelationDomain, RelationRange};
use crate::syntactic::SyntacticLayer;
use crate::trans::rewrite::subst::{collect_conjuncts, decompose_implication, substitute_var};
use crate::trans::TranslationLayer;
use crate::types::{Element, ElementVec, InternedSym, Literal, SentenceId, SymbolId, TaxRelation};

/// `true` iff `csid` is a taxonomy guard atom (`instance`/`subrelation`
/// headed) mentioning one of the predicate variables - i.e. a guard to drop
/// during instantiation (as opposed to a body atom).
fn is_taxonomy_guard_atom(
    syntactic: &SyntacticLayer,
    csid: SentenceId,
    pv_set: &HashSet<SymbolId>,
    tax_ids: &HashSet<SymbolId>,
) -> bool {
    let Some(s) = syntactic.sentence(csid) else {
        return false;
    };
    if !matches!(s.elements.first(), Some(Element::Symbol(sym)) if tax_ids.contains(&sym.id())) {
        return false;
    }
    s.elements[1..]
        .iter()
        .any(|e| matches!(e, Element::Variable { id, .. } if pv_set.contains(id)))
}

/// `true` iff `csid` is the schema's signature guard atom (same head, exactly
/// the guard's variables), which the binding discharges.
fn is_signature_guard_atom(
    syntactic: &SyntacticLayer,
    csid: SentenceId,
    guards: &[PvGuard],
) -> bool {
    let Some(sentence) = syntactic.sentence(csid) else {
        return false;
    };
    let Some(Element::Symbol(head)) = sentence.elements.first() else {
        return false;
    };
    guards.iter().any(|guard| match guard {
        PvGuard::Signature {
            head: expected,
            vars,
            ..
        } => head.id() == *expected && atom_is_variables(&sentence.elements, vars),
        _ => false,
    })
}

/// Instantiate `schema` for one binding of its guard-bound variables:
/// substitute every bound variable, drop the guard conjuncts the binding
/// discharges, reduce the definitional list / assignment terms, and push the
/// result as a synthetic implication (origin = the schema).
///
/// `None` when the rule would be unsound (a guards-only antecedent leaving a
/// bare positive assertion) or useless (a `ListOrderFn` term the reducer could
/// not resolve survives in the consequent).
fn instantiate_schema(
    syntactic: &SyntacticLayer,
    schema: &PredVarSchema,
    binding: &[(SymbolId, Bound)],
    tax_ids: &HashSet<SymbolId>,
    reduce: &ReduceSymbols,
) -> Option<SentenceId> {
    let pv_set: HashSet<SymbolId> = schema.pred_vars.iter().copied().collect();
    let (ant_sid, con_sid) = decompose_implication(syntactic, schema.schema_sid)?;

    // Substitute all guard-bound variables through a sentence, then reduce.
    let instantiate = |mut s: SentenceId| -> Option<SentenceId> {
        for &(var, value) in binding {
            let replacement = match value {
                Bound::Sym(sym) => syntactic
                    .sym_name(sym)
                    .map(|name| Element::Symbol(InternedSym(name)))?,
                Bound::Pos(pos) => Element::Literal(Literal::Number(pos.to_string())),
            };
            s = substitute_var(syntactic, s, var, &replacement, schema.schema_sid);
        }
        Some(reduce_root(syntactic, s, schema.schema_sid, reduce))
    };

    let mut new_conjuncts: Vec<SentenceId> = Vec::new();
    for csid in collect_conjuncts(syntactic, ant_sid) {
        if is_taxonomy_guard_atom(syntactic, csid, &pv_set, tax_ids)
            || is_signature_guard_atom(syntactic, csid, &schema.guards)
        {
            continue;
        }
        new_conjuncts.push(instantiate(csid)?);
    }
    let new_con = instantiate(con_sid)?;
    if contains_head(syntactic, new_con, reduce.list_order_fn) {
        return None;
    }

    let new_ant = match new_conjuncts.len() {
        // Guards-only antecedent -> the body is the consequent; reject the
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
    /// concrete rule (`?REL -> R`, guard dropped) - e.g. `located` ->
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
    pub(crate) fn instantiate_predvars(
        &self,
        seed_sids: &[SentenceId],
        problem_sids: &[SentenceId],
        scope: crate::semantics::types::Scope,
    ) -> Vec<SentenceId> {
        let prog = self.rewrite_program();
        if prog.predvar_schemas.is_empty() {
            return Vec::new();
        }
        // Suppress the schema templates (and their originals): per-problem
        // instantiations stand in for them.  Rule-source suppression stays
        // with `run_rewrite_pass`.
        {
            let mut suppressed = self.suppressed.write().unwrap();
            for sc in &prog.predvar_schemas {
                suppressed.insert(sc.schema_sid);
                if let Some(origin) = self
                    .semantic
                    .syntactic
                    .synthetic_origin
                    .get(&sc.schema_sid)
                    .copied()
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

        let tax_ids: HashSet<SymbolId> = [INSTANCE_RELATION.id(), SUBRELATION_RELATION.id()]
            .into_iter()
            .collect();
        let reduce = ReduceSymbols::from_consts();
        let mut out: Vec<SentenceId> = Vec::new();

        for schema in &prog.predvar_schemas {
            let cap = if schema
                .guards
                .iter()
                .any(|guard| matches!(guard, PvGuard::Signature { .. }))
            {
                crate::syntactic::sine::scale_signature_predvar_cap()
            } else {
                crate::syntactic::sine::scale_predvar_cap()
            };
            let bindings =
                self.find_predvar_bindings(schema, &ordered_syms, &seed_syms, cap, scope);

            for binding in bindings {
                let cached = self
                    .predvar_cache
                    .read()
                    .unwrap()
                    .get(&(schema.schema_sid, binding.clone()))
                    .copied();
                if let Some(cached) = cached {
                    self.predvar_instances.write().unwrap().insert(cached);
                    out.push(cached);
                    continue;
                }
                let pairs: Vec<(SymbolId, Bound)> = schema
                    .bound_vars
                    .iter()
                    .copied()
                    .zip(binding.iter().copied())
                    .collect();
                if let Some(sid) =
                    instantiate_schema(&self.semantic.syntactic, schema, &pairs, &tax_ids, &reduce)
                {
                    self.predvar_cache
                        .write()
                        .unwrap()
                        .insert((schema.schema_sid, binding), sid);
                    self.predvar_instances.write().unwrap().insert(sid);
                    out.push(sid);
                }
            }
        }
        crate::log!(
            Debug,
            "sigmakee_rs_core::trans",
            format!(
                "instantiate_predvars: {} schema(s), {} problem syms -> {} instantiated rule(s)",
                prog.predvar_schemas.len(),
                ordered_syms.len(),
                out.len()
            )
        );
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
    fn find_predvar_bindings(
        &self,
        schema: &PredVarSchema,
        ordered_syms: &[SymbolId],
        seed_syms: &HashSet<SymbolId>,
        cap: usize,
        scope: crate::semantics::types::Scope,
    ) -> Vec<Vec<Bound>> {
        use std::collections::HashMap;
        let pred_vars = &schema.pred_vars;

        let mut candidates: Vec<HashMap<SymbolId, Bound>> = Vec::new();

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

        let signature_guard = schema.guards.iter().find_map(|guard| match guard {
            PvGuard::Signature { kind, vars, .. } => Some((*kind, vars.as_slice())),
            _ => None,
        });

        if let Some((kind, vars)) = signature_guard {
            for &sym in sorted_syms {
                if !arity_ok(sym) {
                    continue;
                }
                match kind {
                    SignatureKind::Domain | SignatureKind::DomainSubclass => {
                        for (position, domain) in
                            self.semantic.domain_scoped(sym, scope).iter().enumerate()
                        {
                            if schema.body_arity.is_some_and(|arity| position + 1 > arity) {
                                continue;
                            }
                            let class = match (kind, domain) {
                                (SignatureKind::Domain, RelationDomain::Domain(class))
                                | (
                                    SignatureKind::DomainSubclass,
                                    RelationDomain::DomainSubclass(class),
                                ) => *class,
                                _ => continue,
                            };
                            let Ok(position) = u32::try_from(position + 1) else {
                                continue;
                            };
                            let mut binding = HashMap::new();
                            binding.insert(vars[0], Bound::Sym(sym));
                            binding.insert(vars[1], Bound::Pos(position));
                            binding.insert(vars[2], Bound::Sym(class));
                            candidates.push(binding);
                        }
                    }
                    SignatureKind::Range | SignatureKind::RangeSubclass => {
                        if !self.semantic.is_function_scoped(sym, scope) {
                            continue;
                        }
                        let class = match (kind, self.semantic.range_scoped(sym, scope)) {
                            (SignatureKind::Range, RelationRange::Range(class))
                            | (SignatureKind::RangeSubclass, RelationRange::RangeSubclass(class)) => {
                                class
                            }
                            _ => continue,
                        };
                        let mut binding = HashMap::new();
                        binding.insert(vars[0], Bound::Sym(sym));
                        binding.insert(vars[1], Bound::Sym(class));
                        candidates.push(binding);
                    }
                }
                if candidates.len() >= collect_max {
                    break;
                }
            }
        } else if let Some((gv1, gv2)) = subrel_guard {
            // Only the two-variable subrelation shape is handled here.
            if pred_vars.len() != 2 {
                return Vec::new();
            }
            for &r1 in sorted_syms {
                if !arity_ok(r1) {
                    continue;
                }
                let mut parents = self.semantic.parents_of_scoped(r1, scope);
                parents.sort_unstable_by_key(|(p, _)| *p);
                for (parent, rel) in parents {
                    if !matches!(rel, TaxRelation::Subrelation) {
                        continue;
                    }
                    let mut b = HashMap::new();
                    b.insert(gv1, Bound::Sym(r1));
                    b.insert(gv2, Bound::Sym(parent));
                    candidates.push(b);
                }
                if candidates.len() > collect_max {
                    break;
                }
            }
        } else {
            // Instance-guard-only: per-var domain, cross product.
            let mut domains: Vec<(SymbolId, Vec<SymbolId>)> = Vec::new();
            for &v in pred_vars {
                let class = schema.guards.iter().find_map(|g| match g {
                    PvGuard::Instance { var, class } if *var == v => Some(*class),
                    _ => None,
                });
                let Some(class) = class else {
                    return Vec::new();
                };
                let dom: Vec<SymbolId> = sorted_syms
                    .iter()
                    .copied()
                    .filter(|&r| {
                        r != class
                            && arity_ok(r)
                            && self.reaches_via_instance_scoped(r, class, scope)
                    })
                    .collect();
                domains.push((v, dom));
            }
            // Cross product (bounded).
            candidates.push(HashMap::new());
            for (v, dom) in &domains {
                let mut next: Vec<HashMap<SymbolId, Bound>> = Vec::new();
                for base in &candidates {
                    for &r in dom {
                        let mut b = base.clone();
                        b.insert(*v, Bound::Sym(r));
                        next.push(b);
                        if next.len() > collect_max {
                            break;
                        }
                    }
                    if next.len() > collect_max {
                        break;
                    }
                }
                candidates = next;
            }
        }

        // Validate every candidate against all guards; dedup.
        let mut out: Vec<Vec<Bound>> = Vec::new();
        let mut seen: HashSet<Vec<Bound>> = HashSet::new();
        for b in candidates {
            if !self.binding_satisfies_guards(schema, &b, scope) {
                continue;
            }
            let key: Vec<Bound> = schema.bound_vars.iter().map(|v| b[v]).collect();
            if seen.insert(key.clone()) {
                out.push(key);
            }
        }
        // Rank seed-touching bindings first (stable sort preserves the
        // seed-first symbol ordering within each group), then truncate to cap.
        out.sort_by_key(|binding| {
            !binding
                .iter()
                .any(|bound| matches!(bound, Bound::Sym(sym) if seed_syms.contains(sym)))
        });
        out.truncate(cap);
        out
    }

    /// `true` iff `binding` (var->relation) satisfies all of `schema`'s guards.
    fn binding_satisfies_guards(
        &self,
        schema: &PredVarSchema,
        binding: &std::collections::HashMap<SymbolId, Bound>,
        scope: crate::semantics::types::Scope,
    ) -> bool {
        schema.guards.iter().all(|g| match g {
            PvGuard::Instance { var, class } => match binding.get(var) {
                Some(Bound::Sym(rel)) => self.reaches_via_instance_scoped(*rel, *class, scope),
                _ => false,
            },
            PvGuard::Subrelation { v1, v2 } => match (binding.get(v1), binding.get(v2)) {
                (Some(Bound::Sym(r1)), Some(Bound::Sym(r2))) => self
                    .semantic
                    .parents_of_scoped(*r1, scope)
                    .into_iter()
                    .any(|(p, rel)| matches!(rel, TaxRelation::Subrelation) && p == *r2),
                _ => false,
            },
            PvGuard::Signature { kind, vars, .. } => match kind {
                SignatureKind::Domain | SignatureKind::DomainSubclass => {
                    let (Some(Bound::Sym(rel)), Some(Bound::Pos(pos)), Some(Bound::Sym(class))) =
                        (binding.get(&vars[0]), binding.get(&vars[1]), binding.get(&vars[2]))
                    else {
                        return false;
                    };
                    let Some(position) = pos.checked_sub(1).map(|value| value as usize) else {
                        return false;
                    };
                    self.semantic
                        .domain_scoped(*rel, scope)
                        .get(position)
                        .is_some_and(|domain| match (kind, domain) {
                            (SignatureKind::Domain, RelationDomain::Domain(found))
                            | (
                                SignatureKind::DomainSubclass,
                                RelationDomain::DomainSubclass(found),
                            ) => found == class,
                            _ => false,
                        })
                }
                SignatureKind::Range | SignatureKind::RangeSubclass => {
                    let (Some(Bound::Sym(func)), Some(Bound::Sym(class))) =
                        (binding.get(&vars[0]), binding.get(&vars[1]))
                    else {
                        return false;
                    };
                    self.semantic.is_function_scoped(*func, scope)
                        && match (kind, self.semantic.range_scoped(*func, scope)) {
                            (SignatureKind::Range, RelationRange::Range(found))
                            | (
                                SignatureKind::RangeSubclass,
                                RelationRange::RangeSubclass(found),
                            ) => found == *class,
                            _ => false,
                        }
                }
            },
        })
    }

    /// `sym` is an instance of `class` (directly or via a subclass),
    /// evaluated in `scope`: the
    /// `instance` edge and the subclass walk both see the session overlay,
    /// so a relation declared transitive inside a test session satisfies the
    /// schema guard.  `Base` scope is byte-identical to the unscoped form.
    fn reaches_via_instance_scoped(
        &self,
        sym: SymbolId,
        class: SymbolId,
        scope: crate::semantics::types::Scope,
    ) -> bool {
        self.semantic
            .parents_of_scoped(sym, scope)
            .into_iter()
            .any(|(k, rel)| {
                matches!(rel, TaxRelation::Instance)
                    && (k == class || self.semantic.has_ancestor_scoped(k, class, scope))
            })
    }
}
