use std::collections::{HashMap, HashSet};

use smallvec::smallvec;

use super::reduce::{contains_head, reduce_root, ReduceSymbols};
use super::{is_bare_positive_assertion, Bound, GuardArg, PredVarSchema, PvGuard, SignatureKind};
use crate::parse::ast::OpKind;
use crate::semantics::types::{RelationDomain, RelationRange, Scope};
use crate::syntactic::SyntacticLayer;
use crate::trans::rewrite::subst::{collect_conjuncts, decompose_implication, substitute_var};
use crate::trans::TranslationLayer;
use crate::types::{Element, ElementVec, InternedSym, Literal, SentenceId, SymbolId, TaxRelation};

/// A (partial) assignment of values to a schema's guard-bound variables.
type Binding = HashMap<SymbolId, Bound>;

/// The variable values one guard contributes to a binding.
type Row = Vec<(SymbolId, Bound)>;

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
    reduce: &ReduceSymbols,
) -> Option<SentenceId> {
    let (ant_sid, mut con_sid) = decompose_implication(syntactic, schema.schema_sid)?;
    let mut conjuncts = collect_conjuncts(syntactic, ant_sid);
    if let Some((inner_ant, inner_con)) = schema.inner {
        conjuncts.extend(collect_conjuncts(syntactic, inner_ant));
        con_sid = inner_con;
    }

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
    for csid in conjuncts {
        if schema.guard_sids.contains(&csid) {
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
        scope: Scope,
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
                    instantiate_schema(&self.semantic.syntactic, schema, &pairs, &reduce)
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

    /// Enumerate bindings of `schema.bound_vars` (values aligned to it) that
    /// satisfy every guard, restricted to higher-order values occurring in
    /// `ordered_syms`.
    ///
    /// Every guard except `instance` contributes the rows it can bind (the
    /// signature caches, subrelation edges, or matching ground facts), and the
    /// rows are joined on shared variables.  A higher-order variable no such
    /// guard binds is then enumerated from the problem symbols that are
    /// instances of its `instance` guard's class.
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
        scope: Scope,
    ) -> Vec<Vec<Bound>> {
        // Collection bound: well above `cap` so ranking (below) sees the full
        // realistic candidate set, but still a hard ceiling on join blowup for
        // pathological schemas.
        let collect_max = cap.saturating_mul(16).max(cap);

        let mut candidates: Vec<Binding> = vec![Binding::new()];
        for guard in &schema.guards {
            let rows = match guard {
                PvGuard::Instance { .. } => continue,
                PvGuard::Subrelation { v1, v2 } => {
                    self.subrelation_rows(schema, *v1, *v2, ordered_syms, scope)
                }
                PvGuard::Signature { kind, vars, .. } => {
                    self.signature_rows(schema, *kind, vars, ordered_syms, scope)
                }
                PvGuard::Fact { head, args } => {
                    self.fact_rows(schema, *head, args, ordered_syms, scope)
                }
            };
            candidates = join(&candidates, &rows, collect_max);
        }
        for var in schema.higher_order_vars() {
            if candidates.first().is_none_or(|b| b.contains_key(&var)) {
                continue;
            }
            let class = schema.guards.iter().find_map(|g| match g {
                PvGuard::Instance { var: v, class } if *v == var => Some(*class),
                _ => None,
            });
            let Some(class) = class else {
                return Vec::new();
            };
            let rows: Vec<Row> = ordered_syms
                .iter()
                .copied()
                .filter(|&r| {
                    r != class
                        && self.arity_ok(schema, var, r)
                        && self.reaches_via_instance_scoped(r, class, scope)
                })
                .map(|r| vec![(var, Bound::Sym(r))])
                .collect();
            candidates = join(&candidates, &rows, collect_max);
        }

        // Validate every candidate against all guards; dedup.
        let mut out: Vec<Vec<Bound>> = Vec::new();
        let mut seen: HashSet<Vec<Bound>> = HashSet::new();
        for b in candidates {
            if !self.binding_satisfies_guards(schema, &b, scope) {
                continue;
            }
            let Some(key) = schema
                .bound_vars
                .iter()
                .map(|v| b.get(v).copied())
                .collect::<Option<Vec<Bound>>>()
            else {
                continue;
            };
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

    /// Whether `value` may be bound to the higher-order variable `var`: only
    /// at the arity `var` is applied with (row-var expansion stamps one arity
    /// per variant); variadic / unknown-arity symbols accept any variant.
    fn arity_ok(&self, schema: &PredVarSchema, var: SymbolId, value: SymbolId) -> bool {
        match (schema.call_arity.get(&var), self.semantic.arity(value)) {
            (Some(&k), Some(a)) if a >= 0 => a as usize == k,
            _ => true,
        }
    }

    /// Rows of a `domain*` / `range*` signature guard, one per declared
    /// signature of a problem symbol.
    fn signature_rows(
        &self,
        schema: &PredVarSchema,
        kind: SignatureKind,
        vars: &[SymbolId],
        ordered_syms: &[SymbolId],
        scope: Scope,
    ) -> Vec<Row> {
        let mut rows = Vec::new();
        for &sym in ordered_syms {
            if !self.arity_ok(schema, vars[0], sym) {
                continue;
            }
            match kind {
                SignatureKind::Domain | SignatureKind::DomainSubclass => {
                    let applied = schema.call_arity.get(&vars[0]).copied();
                    for (position, domain) in
                        self.semantic.domain_scoped(sym, scope).iter().enumerate()
                    {
                        if applied.is_some_and(|arity| position + 1 > arity) {
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
                        rows.push(vec![
                            (vars[0], Bound::Sym(sym)),
                            (vars[1], Bound::Pos(position)),
                            (vars[2], Bound::Sym(class)),
                        ]);
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
                    rows.push(vec![
                        (vars[0], Bound::Sym(sym)),
                        (vars[1], Bound::Sym(class)),
                    ]);
                }
            }
        }
        rows
    }

    /// Rows of a `(subrelation ?V1 ?V2)` guard: each problem relation paired
    /// with each of its subrelation parents.
    fn subrelation_rows(
        &self,
        schema: &PredVarSchema,
        v1: SymbolId,
        v2: SymbolId,
        ordered_syms: &[SymbolId],
        scope: Scope,
    ) -> Vec<Row> {
        let mut rows = Vec::new();
        for &r1 in ordered_syms {
            if !self.arity_ok(schema, v1, r1) {
                continue;
            }
            let mut parents = self.semantic.parents_of_scoped(r1, scope);
            parents.sort_unstable_by_key(|(p, _)| *p);
            for (parent, rel) in parents {
                if matches!(rel, TaxRelation::Subrelation) {
                    rows.push(vec![(v1, Bound::Sym(r1)), (v2, Bound::Sym(parent))]);
                }
            }
        }
        rows
    }

    /// Rows of a fact guard: one per ground fact with `head` visible in
    /// `scope` that matches the guard's constants and binds every variable to
    /// a symbol, with each higher-order value occurring in the problem.
    fn fact_rows(
        &self,
        schema: &PredVarSchema,
        head: SymbolId,
        args: &[GuardArg],
        ordered_syms: &[SymbolId],
        scope: Scope,
    ) -> Vec<Row> {
        let problem: HashSet<SymbolId> = ordered_syms.iter().copied().collect();
        let ho_vars: HashSet<SymbolId> = schema.higher_order_vars().collect();
        let facts = self
            .semantic
            .scope_filter_sids(self.semantic.syntactic.by_head_id(&head), scope);
        let mut rows: Vec<Row> = facts
            .into_iter()
            .filter_map(|sid| {
                let fact = self.semantic.syntactic.sentence(sid)?;
                if fact.elements.len() != args.len() + 1 {
                    return None;
                }
                let mut row = Row::new();
                for (arg, element) in args.iter().zip(&fact.elements[1..]) {
                    match (arg, element) {
                        (GuardArg::Var(var), Element::Symbol(sym)) => {
                            let value = sym.id();
                            if ho_vars.contains(var)
                                && (!problem.contains(&value)
                                    || !self.arity_ok(schema, *var, value))
                            {
                                return None;
                            }
                            row.push((*var, Bound::Sym(value)));
                        }
                        (GuardArg::Sym(expected), Element::Symbol(sym))
                            if sym.id() == *expected => {}
                        (GuardArg::Lit(expected), Element::Literal(lit)) if lit == expected => {}
                        _ => return None,
                    }
                }
                Some(row)
            })
            .collect();
        rows.sort_unstable();
        rows.dedup();
        rows
    }

    /// `true` iff `binding` (var->value) satisfies all of `schema`'s guards.
    /// A fact guard holds by construction: its rows are the matching facts,
    /// and every candidate was joined with them.
    fn binding_satisfies_guards(
        &self,
        schema: &PredVarSchema,
        binding: &Binding,
        scope: Scope,
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
            PvGuard::Fact { args, .. } => args.iter().all(|arg| match arg {
                GuardArg::Var(var) => binding.contains_key(var),
                _ => true,
            }),
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
    fn reaches_via_instance_scoped(&self, sym: SymbolId, class: SymbolId, scope: Scope) -> bool {
        self.semantic
            .parents_of_scoped(sym, scope)
            .into_iter()
            .any(|(k, rel)| {
                matches!(rel, TaxRelation::Instance)
                    && (k == class || self.semantic.has_ancestor_scoped(k, class, scope))
            })
    }
}

/// Join every candidate with every row it agrees with on shared variables,
/// stopping once `max` bindings are collected.
fn join(candidates: &[Binding], rows: &[Row], max: usize) -> Vec<Binding> {
    let mut out = Vec::new();
    for base in candidates {
        for row in rows {
            let mut binding = base.clone();
            let consistent = row
                .iter()
                .all(|&(var, value)| *binding.entry(var).or_insert(value) == value);
            if consistent {
                out.push(binding);
                if out.len() >= max {
                    return out;
                }
            }
        }
    }
    out
}
