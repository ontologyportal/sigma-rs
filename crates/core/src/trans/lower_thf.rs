// crates/core/src/trans/lower_thf.rs
//
// SUMO sentence -> THF (`ThfExpr`) lowering — the HIGHER-ORDER sibling of
// `lower.rs`, and a deliberately separate subsystem: the first-order lowering
// is untouched by everything here.
//
// Bi-sorted phase-1 typing (the SUMO-in-THF "plain" scheme):
//   * every individual is `$i`; SUMO's class discipline stays as `instance`
//     guards exactly as in FOF — so, unlike TFF, NO classification walk is
//     needed and variable sorts come purely from signature positions;
//   * an argument position declared at the `Formula` class carries `$o`, and
//     the argument lowers as a FORMULA (this is where `knows`/`believes`/
//     `holdsDuring`/`modalAttribute` content that the FO pipeline drops
//     becomes first-class);
//   * numbers are hidden as `n__…: $i` constants and strings as `str__…: $i`
//     (FOF parity — bi-sorted THF carries no arithmetic);
//   * DUAL CONSTANTS: an APPLIED relation emits at its arrow sort under its
//     usual name (`s__part : $i > $i > $o`), while a relation used as an
//     INDIVIDUAL (e.g. `(instance part TransitiveRelation)`) emits its `$i`
//     mention constant (`s__part__m`) — retyping instead would make every
//     taxonomy fact about relations ill-typed;
//   * `KappaFn` builds a genuine lambda: `(KappaFn ?V φ)` lowers to
//     `(s__KappaFn @ ^[V:$i]: φ)`, and problem assembly appends the
//     comprehension axiom connecting it to `instance`.
//
// Sentences that cannot lower carry a structured [`ThfDrop`] reason instead
// of a silent `None` — the auditable-exclusion principle.

use std::collections::{HashMap, HashSet};

use crate::semantics::types::Scope;
use crate::trans::caches::ho_signatures::{HoSignature, KAPPA_FN};
#[cfg(feature = "ask")]
use crate::trans::ir::HoProblem;
use crate::trans::ir::{HoSort, ThfConst, ThfExpr};
use crate::trans::TranslationLayer;
use crate::types::{Element, Literal, OpKind, Sentence};
use crate::{SentenceId, SymbolId};

/// A lowered root sentence: the (free-var-wrapped) expression plus the
/// constant declarations it needs.  The THF analog of `CachedFormula`.
#[derive(Debug, Clone, PartialEq)]
pub struct ThfCached {
    pub expr:  ThfExpr,
    pub decls: Vec<ThfConst>,
    /// Definition axioms introduced by lambda lifting (`kdef_<sid>`): each
    /// `KappaFn` lambda becomes a fresh defined predicate closed over its
    /// captured variables — Vampire 5.0.1 proves the lifted (lambda-free)
    /// form directly, while explicit `^`-terms defeat its calculus.
    pub defs:  Vec<ThfExpr>,
}

/// Why a sentence did not lower to THF.  Structured so exclusions are
/// auditable (surfaced as debug logs / future diagnostics), never silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThfDrop {
    /// Suppressed by the rewrite pass (a synthetic stands in for it).
    Suppressed,
    /// The sentence body is gone from the store.
    MissingSentence,
    /// A row variable survived to lowering (row expansion happens upstream;
    /// a leftover one cannot be typed).
    RowVariable,
    /// A quantifier/operator/function where a formula head must be, or a
    /// bare non-formula construct in formula position.
    UnsupportedHead,
    /// A predicate application (a `$o` expression) in a `$i` term position.
    FormulaInTermPosition,
    /// A variable used both as an individual and as a formula in one root.
    MixedVarSort,
}

/// Cache value for `translation::formulas_thf`.
#[derive(Debug, Clone, PartialEq)]
pub enum ThfEntry {
    Formula(ThfCached),
    Dropped(ThfDrop),
}

/// Per-sentence declaration accumulator (deduped within one sentence;
/// cross-sentence dedup happens in [`HoProblem::declare`]).
#[derive(Default)]
struct ThfDecls {
    list: Vec<ThfConst>,
    seen: HashSet<String>,
    defs: Vec<ThfExpr>,
}

impl ThfDecls {
    fn ensure(&mut self, name: &str, sort: HoSort) {
        if self.seen.insert(name.to_string()) {
            self.list.push(ThfConst { name: name.to_string(), sort });
        }
    }
}

/// Per-root lowering context.
struct ThfCtx<'a> {
    scope: Scope,
    /// Variable SymbolId -> THF sort (`$o` iff the variable occurs in a
    /// formula position somewhere in this root).
    vars: &'a HashMap<SymbolId, HoSort>,
}

impl TranslationLayer {
    // -- public entry points -------------------------------------------------

    /// Lower a root sentence for the `formulas_thf` cache: free variables
    /// universally quantified at their inferred THF sorts.
    pub(crate) fn lower_root_thf(&self, sid: SentenceId) -> ThfEntry {
        if self.suppressed.read().unwrap().contains(&sid) {
            return ThfEntry::Dropped(ThfDrop::Suppressed);
        }
        let scope = self.scope_of(sid);
        match self.lower_thf_inner(sid, scope, /*existential*/ false) {
            Ok((cached, _)) => ThfEntry::Formula(cached),
            Err(drop) => ThfEntry::Dropped(drop),
        }
    }

    /// Lower a multi-root conjecture as ONE conjunction with the free-variable
    /// union wrapped existentially once (same soundness argument as the FO
    /// `lower_conjecture_set`: CAF-split `(and …)` queries share variable
    /// SymbolIds but not indices).
    #[cfg(feature = "ask")]
    pub(crate) fn lower_conjecture_thf(
        &self,
        sids:  &[SentenceId],
        scope: Option<Scope>,
    ) -> Option<ThfCached> {
        if sids.is_empty() {
            return None;
        }
        let mut bodies: Vec<ThfExpr> = Vec::with_capacity(sids.len());
        let mut decls = ThfDecls::default();
        let mut global_idx: HashMap<SymbolId, u32> = HashMap::new();
        let mut next_idx: u32 = 0;
        let mut bound: HashSet<SymbolId> = HashSet::new();
        let mut var_sorts_all: HashMap<SymbolId, HoSort> = HashMap::new();

        for &sid in sids {
            if self.suppressed.read().unwrap().contains(&sid) {
                return None;
            }
            let s = scope.unwrap_or_else(|| self.scope_of(sid));
            let sentence = self.semantic.syntactic.sentence(sid)?;

            let mut vids: HashMap<SymbolId, u32> = HashMap::new();
            self.semantic.syntactic.collect_vars(sid, &mut vids);
            let mut locals: Vec<(SymbolId, u32)> =
                vids.iter().map(|(k, v)| (*k, *v)).collect();
            locals.sort_by_key(|&(_, l)| l);
            let mut remap: HashMap<u32, u32> = HashMap::new();
            for (sym, local) in locals {
                let g = *global_idx.entry(sym).or_insert_with(|| {
                    let g = next_idx;
                    next_idx += 1;
                    g
                });
                remap.insert(local, g);
            }

            let var_sorts = self.thf_var_sorts(sid, s);
            let ctx = ThfCtx { scope: s, vars: &var_sorts };
            let mut body = self.thf_formula(&sentence, &ctx, &mut decls).ok()?;
            remap_thf_vars(&mut body, &remap);
            bodies.push(body);
            self.semantic.syntactic.collect_bound_vars(sid, true, &mut bound);
            for (k, v) in var_sorts {
                // `$o` evidence from any conjunct wins for the shared binder.
                var_sorts_all
                    .entry(k)
                    .and_modify(|e| {
                        if v == HoSort::O {
                            *e = HoSort::O;
                        }
                    })
                    .or_insert(v);
            }
        }

        let mut free: Vec<(SymbolId, u32)> = global_idx
            .iter()
            .filter(|(id, _)| !bound.contains(id))
            .map(|(id, idx)| (*id, *idx))
            .collect();
        free.sort_by_key(|&(_, idx)| idx);

        let mut expr = if bodies.len() == 1 {
            bodies.pop().unwrap()
        } else {
            ThfExpr::And(bodies)
        };
        for &(id, idx) in free.iter().rev() {
            let sort = var_sorts_all.get(&id).cloned().unwrap_or(HoSort::I);
            expr = ThfExpr::Exists(idx, sort, Box::new(expr));
        }
        Some(ThfCached { expr, decls: decls.list, defs: decls.defs })
    }

    // -- the shared core -------------------------------------------------------

    fn lower_thf_inner(
        &self,
        sid:         SentenceId,
        scope:       Scope,
        existential: bool,
    ) -> Result<(ThfCached, HashMap<SymbolId, HoSort>), ThfDrop> {
        let sentence = self
            .semantic
            .syntactic
            .sentence(sid)
            .ok_or(ThfDrop::MissingSentence)?;

        let var_sorts = self.thf_var_sorts(sid, scope);
        let mut decls = ThfDecls::default();
        let ctx = ThfCtx { scope, vars: &var_sorts };
        let body = self.thf_formula(&sentence, &ctx, &mut decls)?;

        // Free variables wrap at their inferred sorts.
        let mut all: HashMap<SymbolId, u32> = HashMap::new();
        self.semantic.syntactic.collect_vars(sid, &mut all);
        let mut bound: HashSet<SymbolId> = HashSet::new();
        self.semantic.syntactic.collect_bound_vars(sid, true, &mut bound);
        let mut free: Vec<(SymbolId, u32)> = all
            .iter()
            .filter(|(id, _)| !bound.contains(id))
            .map(|(id, idx)| (*id, *idx))
            .collect();
        free.sort_by_key(|&(_, idx)| idx);

        let mut expr = body;
        for &(id, idx) in free.iter().rev() {
            let sort = var_sorts.get(&id).cloned().unwrap_or(HoSort::I);
            expr = if existential {
                ThfExpr::Exists(idx, sort, Box::new(expr))
            } else {
                ThfExpr::Forall(idx, sort, Box::new(expr))
            };
        }
        Ok((
            ThfCached { expr, decls: decls.list, defs: decls.defs },
            var_sorts,
        ))
    }

    /// One walk over the root: a variable is `$o` iff it occurs in a formula
    /// position — a `$o`-declared argument slot, or directly under a logical
    /// connective / as a quantifier body.  Everything else is `$i`.
    fn thf_var_sorts(&self, sid: SentenceId, scope: Scope) -> HashMap<SymbolId, HoSort> {
        let mut out: HashMap<SymbolId, HoSort> = HashMap::new();
        let mut stack = vec![sid];
        let mut seen: HashSet<SentenceId> = HashSet::new();
        let syn = &self.semantic.syntactic;
        while let Some(s) = stack.pop() {
            if !seen.insert(s) {
                continue;
            }
            let Some(sentence) = syn.sentence(s) else { continue };
            for el in sentence.elements.iter() {
                if let Element::Sub(sub) = el {
                    stack.push(*sub);
                }
            }
            match sentence.elements.first() {
                Some(Element::Op(op)) => {
                    // Variables sitting directly in connective/body positions
                    // are formulas.  (Quantifier var-lists are element 1 of
                    // ForAll/Exists and contain Variables at `$i`-binder
                    // positions — skip them.)
                    let body_args: &[Element] = match op {
                        OpKind::ForAll | OpKind::Exists => {
                            &sentence.elements[2..]
                        }
                        _ => &sentence.elements[1..],
                    };
                    for el in body_args {
                        if let Element::Variable { id, .. } = el {
                            out.insert(*id, HoSort::O);
                        }
                    }
                }
                Some(Element::Symbol(head)) => {
                    if head.name().as_ref() == KAPPA_FN {
                        // (KappaFn ?V body): ?V is the `$i` binder; the body
                        // slot is a formula position handled by recursion.
                        if let Some(Element::Variable { id, .. }) = sentence.elements.get(2) {
                            out.insert(*id, HoSort::O);
                        }
                        continue;
                    }
                    let sig = self.ho_signature_scoped(head.id(), scope);
                    if let Some(sig) = sig {
                        for (i, el) in sentence.elements[1..].iter().enumerate() {
                            if let Element::Variable { id, .. } = el {
                                if sig.args.get(i) == Some(&HoSort::O) {
                                    out.insert(*id, HoSort::O);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }

    // -- formula-position lowering ---------------------------------------------

    fn thf_formula(
        &self,
        sentence: &Sentence,
        ctx:      &ThfCtx<'_>,
        decls:    &mut ThfDecls,
    ) -> Result<ThfExpr, ThfDrop> {
        match sentence.elements.first().ok_or(ThfDrop::UnsupportedHead)? {
            Element::Op(op) => self.thf_operator(op.clone(), sentence, ctx, decls),
            Element::Symbol(_) => self.thf_atom(sentence, ctx, decls),
            _ => Err(ThfDrop::UnsupportedHead),
        }
    }

    fn thf_operator(
        &self,
        op:       OpKind,
        sentence: &Sentence,
        ctx:      &ThfCtx<'_>,
        decls:    &mut ThfDecls,
    ) -> Result<ThfExpr, ThfDrop> {
        let args = &sentence.elements[1..];
        match op {
            OpKind::And | OpKind::Or => {
                let mut es = Vec::with_capacity(args.len());
                for e in args {
                    es.push(self.thf_child_formula(e, ctx, decls)?);
                }
                if es.is_empty() {
                    return Err(ThfDrop::UnsupportedHead);
                }
                Ok(if matches!(op, OpKind::And) {
                    ThfExpr::And(es)
                } else {
                    ThfExpr::Or(es)
                })
            }
            OpKind::Not => Ok(ThfExpr::Not(Box::new(self.thf_child_formula(
                args.first().ok_or(ThfDrop::UnsupportedHead)?,
                ctx,
                decls,
            )?))),
            OpKind::Implies => {
                let a = self.thf_child_formula(args.first().ok_or(ThfDrop::UnsupportedHead)?, ctx, decls)?;
                let b = self.thf_child_formula(args.get(1).ok_or(ThfDrop::UnsupportedHead)?, ctx, decls)?;
                Ok(ThfExpr::Imp(Box::new(a), Box::new(b)))
            }
            OpKind::Iff => {
                let a = self.thf_child_formula(args.first().ok_or(ThfDrop::UnsupportedHead)?, ctx, decls)?;
                let b = self.thf_child_formula(args.get(1).ok_or(ThfDrop::UnsupportedHead)?, ctx, decls)?;
                Ok(ThfExpr::Iff(Box::new(a), Box::new(b)))
            }
            OpKind::Equal => {
                let a = self.thf_term(args.first().ok_or(ThfDrop::UnsupportedHead)?, ctx, decls)?;
                let b = self.thf_term(args.get(1).ok_or(ThfDrop::UnsupportedHead)?, ctx, decls)?;
                Ok(ThfExpr::Eq(Box::new(a), Box::new(b)))
            }
            OpKind::ForAll | OpKind::Exists => {
                let exist = matches!(op, OpKind::Exists);
                let ids: Vec<(SymbolId, u32)> = match args.first().ok_or(ThfDrop::UnsupportedHead)? {
                    Element::Sub(vl_sid) => {
                        let vl = self
                            .semantic
                            .syntactic
                            .sentence(*vl_sid)
                            .ok_or(ThfDrop::MissingSentence)?;
                        vl.elements
                            .iter()
                            .filter_map(|e| match e {
                                Element::Variable { id, var_index, is_row: false, .. } => {
                                    Some((*id, *var_index))
                                }
                                _ => None,
                            })
                            .collect()
                    }
                    _ => Vec::new(),
                };
                let body = self.thf_child_formula(args.get(1).ok_or(ThfDrop::UnsupportedHead)?, ctx, decls)?;
                let mut f = body;
                for (id, idx) in ids.into_iter().rev() {
                    let sort = ctx.vars.get(&id).cloned().unwrap_or(HoSort::I);
                    f = if exist {
                        ThfExpr::Exists(idx, sort, Box::new(f))
                    } else {
                        ThfExpr::Forall(idx, sort, Box::new(f))
                    };
                }
                Ok(f)
            }
        }
    }

    /// A child element in FORMULA position.
    fn thf_child_formula(
        &self,
        el:    &Element,
        ctx:   &ThfCtx<'_>,
        decls: &mut ThfDecls,
    ) -> Result<ThfExpr, ThfDrop> {
        match el {
            Element::Sub(sid) => {
                let s = self
                    .semantic
                    .syntactic
                    .sentence(*sid)
                    .ok_or(ThfDrop::MissingSentence)?;
                self.thf_formula(&s, ctx, decls)
            }
            Element::Variable { id, is_row, .. } => {
                if *is_row {
                    return Err(ThfDrop::RowVariable);
                }
                // The var-sort pass marked formula-position variables `$o`;
                // a variable reaching here at `$i` is used both ways.
                if ctx.vars.get(id) != Some(&HoSort::O) {
                    return Err(ThfDrop::MixedVarSort);
                }
                let idx = self.thf_var_index(el)?;
                Ok(ThfExpr::Var(idx))
            }
            Element::Symbol(sym) => {
                // `True` / `False` constants, else a propositional atom at `$o`
                // (a formula-valued individual, e.g. `Form` in
                // `(modalAttribute Form Obligation)`): a DISTINCT `__o`
                // constant, so it never collides with a `$i` use of the same
                // symbol elsewhere.
                match &*sym.name() {
                    "True" => Ok(ThfExpr::True),
                    "False" => Ok(ThfExpr::False),
                    _ => {
                        let name = format!("{}__o", sym.tptp_sym_name());
                        decls.ensure(&name, HoSort::O);
                        Ok(ThfExpr::Const(name))
                    }
                }
            }
            _ => Err(ThfDrop::UnsupportedHead),
        }
    }

    /// A Symbol-headed sentence in formula position: a (curried) predicate
    /// application at its arrow sort.
    fn thf_atom(
        &self,
        sentence: &Sentence,
        ctx:      &ThfCtx<'_>,
        decls:    &mut ThfDecls,
    ) -> Result<ThfExpr, ThfDrop> {
        let head = match sentence.elements.first() {
            Some(Element::Symbol(sym)) => sym,
            _ => return Err(ThfDrop::UnsupportedHead),
        };
        if head.name().as_ref() == KAPPA_FN {
            // KappaFn is a term former; it cannot head a formula.
            return Err(ThfDrop::UnsupportedHead);
        }
        // A function application cannot be a formula.
        if self.semantic.is_function_scoped(head.id(), ctx.scope) {
            return Err(ThfDrop::UnsupportedHead);
        }
        let args = &sentence.elements[1..];
        let sig = self
            .ho_signature_scoped(head.id(), ctx.scope)
            .unwrap_or(HoSignature { args: Vec::new(), ret: None });

        let name = self.rel_name(head, args.len());
        decls.ensure(&name, sig.arrow_sort(args.len()));

        let mut lowered = Vec::with_capacity(args.len());
        for (i, el) in args.iter().enumerate() {
            let e = if sig.args.get(i) == Some(&HoSort::O) {
                self.thf_child_formula(el, ctx, decls)?
            } else {
                self.thf_term(el, ctx, decls)?
            };
            lowered.push(e);
        }
        Ok(ThfExpr::apply(ThfExpr::Const(name), lowered))
    }

    // -- term-position lowering --------------------------------------------------

    fn thf_term(
        &self,
        el:    &Element,
        ctx:   &ThfCtx<'_>,
        decls: &mut ThfDecls,
    ) -> Result<ThfExpr, ThfDrop> {
        match el {
            Element::Variable { id, is_row, .. } => {
                if *is_row {
                    return Err(ThfDrop::RowVariable);
                }
                if ctx.vars.get(id) == Some(&HoSort::O) {
                    // Used as a formula elsewhere but as an individual here.
                    return Err(ThfDrop::MixedVarSort);
                }
                Ok(ThfExpr::Var(self.thf_var_index(el)?))
            }
            Element::Literal(Literal::Number(n)) => {
                // FOF parity: numbers hide as opaque `$i` constants.
                let safe = n.replace('.', "_").replace('-', "neg_");
                let name = format!("n__{safe}");
                decls.ensure(&name, HoSort::I);
                Ok(ThfExpr::Const(name))
            }
            Element::Literal(lit @ Literal::Str(s)) => {
                let inner = s
                    .strip_prefix('"')
                    .and_then(|x| x.strip_suffix('"'))
                    .unwrap_or(s);
                let mut needs_hash = false;
                let safe: String = inner
                    .chars()
                    .take(48)
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || c == '_' {
                            c
                        } else {
                            needs_hash = true;
                            '_'
                        }
                    })
                    .collect();
                let name = if needs_hash {
                    format!("str__{safe}_{:08x}", lit.hash())
                } else {
                    format!("str__{safe}")
                };
                decls.ensure(&name, HoSort::I);
                Ok(ThfExpr::Const(name))
            }
            Element::Symbol(sym) => {
                let id = sym.id();
                let name = if self.semantic.is_relation_scoped(id, ctx.scope) {
                    // Dual constant: a relation used as an individual value.
                    sym.tptp_mention_name()
                } else {
                    sym.tptp_sym_name()
                };
                decls.ensure(&name, HoSort::I);
                Ok(ThfExpr::Const(name))
            }
            Element::Sub(sid) => self.thf_sub_term(*sid, ctx, decls),
            Element::Op(_) => Err(ThfDrop::UnsupportedHead),
        }
    }

    /// A sub-sentence in term position: a function application, or the
    /// `KappaFn` lambda former.
    fn thf_sub_term(
        &self,
        sid:   SentenceId,
        ctx:   &ThfCtx<'_>,
        decls: &mut ThfDecls,
    ) -> Result<ThfExpr, ThfDrop> {
        let sentence = self
            .semantic
            .syntactic
            .sentence(sid)
            .ok_or(ThfDrop::MissingSentence)?;
        let head = match sentence.elements.first() {
            Some(Element::Symbol(sym)) => sym,
            _ => return Err(ThfDrop::UnsupportedHead),
        };

        // (KappaFn ?V body) -> LAMBDA-LIFTED: a fresh defined predicate
        // `kdef_<sid>` closed over the body's captured variables,
        //   usage:      (s__KappaFn @ (kdef_<sid> @ captured…))
        //   definition: ![captured…, V]: (kdef_<sid> @ captured… @ V) <=> body
        // (Vampire 5.0.1 handles the lifted applicative form directly;
        // explicit `^`-lambdas defeat its calculus — measured.)
        if head.name().as_ref() == KAPPA_FN {
            let (binder_id, binder_idx) = match sentence.elements.get(1) {
                Some(Element::Variable { id, var_index, is_row: false, .. }) => (*id, *var_index),
                _ => return Err(ThfDrop::UnsupportedHead),
            };
            let body_el = sentence.elements.get(2).ok_or(ThfDrop::UnsupportedHead)?;
            let body = self.thf_child_formula(body_el, ctx, decls)?;

            // Captured variables: the body's vars minus the binder, in
            // var_index order (deterministic).
            let mut body_vars: HashMap<SymbolId, u32> = HashMap::new();
            if let Element::Sub(bsid) = body_el {
                self.semantic.syntactic.collect_vars(*bsid, &mut body_vars);
            } else if let Element::Variable { id, var_index, .. } = body_el {
                body_vars.insert(*id, *var_index);
            }
            body_vars.remove(&binder_id);
            let mut captured: Vec<(SymbolId, u32)> =
                body_vars.iter().map(|(k, v)| (*k, *v)).collect();
            captured.sort_by_key(|&(_, idx)| idx);

            // `sid` is content-addressed, so the lifted name is stable across
            // cache refills.
            let kdef = format!("kdef_{sid}");
            let arity = captured.len() + 1;
            decls.ensure(&kdef, HoSort::curry(&vec![HoSort::I; arity], HoSort::O));
            decls.ensure(
                &head.tptp_sym_name(),
                HoSort::Fn(
                    Box::new(HoSort::Fn(Box::new(HoSort::I), Box::new(HoSort::O))),
                    Box::new(HoSort::I),
                ),
            );

            // Definition axiom (self-closed over captured + binder).
            let mut def_args: Vec<ThfExpr> =
                captured.iter().map(|&(_, idx)| ThfExpr::Var(idx)).collect();
            def_args.push(ThfExpr::Var(binder_idx));
            let lhs = ThfExpr::apply(ThfExpr::Const(kdef.clone()), def_args);
            let mut def = ThfExpr::Iff(Box::new(lhs), Box::new(body));
            def = ThfExpr::Forall(binder_idx, HoSort::I, Box::new(def));
            for &(_, idx) in captured.iter().rev() {
                def = ThfExpr::Forall(idx, HoSort::I, Box::new(def));
            }
            decls.defs.push(def);

            // Usage: KappaFn applied to the (partially applied) definition.
            let partial = ThfExpr::apply(
                ThfExpr::Const(kdef),
                captured.iter().map(|&(_, idx)| ThfExpr::Var(idx)).collect(),
            );
            return Ok(ThfExpr::App(
                Box::new(ThfExpr::Const(head.tptp_sym_name())),
                Box::new(partial),
            ));
        }

        let args = &sentence.elements[1..];
        let sig = self.ho_signature_scoped(head.id(), ctx.scope);
        match sig {
            Some(sig) if sig.ret.is_some() => {
                let name = self.rel_name(head, args.len());
                decls.ensure(&name, sig.arrow_sort(args.len()));
                let mut lowered = Vec::with_capacity(args.len());
                for (i, el) in args.iter().enumerate() {
                    let e = if sig.args.get(i) == Some(&HoSort::O) {
                        self.thf_child_formula(el, ctx, decls)?
                    } else {
                        self.thf_term(el, ctx, decls)?
                    };
                    lowered.push(e);
                }
                Ok(ThfExpr::apply(ThfExpr::Const(name), lowered))
            }
            // A predicate application is a `$o` expression — ill-sorted in a
            // `$i` position.
            Some(_) => Err(ThfDrop::FormulaInTermPosition),
            // Undeclared head in term position: treat as an untyped `$i`
            // function application (FOF parity for session-local functions).
            None => {
                let name = self.rel_name(head, args.len());
                decls.ensure(&name, HoSort::curry(&vec![HoSort::I; args.len()], HoSort::I));
                let mut lowered = Vec::with_capacity(args.len());
                for el in args {
                    lowered.push(self.thf_term(el, ctx, decls)?);
                }
                Ok(ThfExpr::apply(ThfExpr::Const(name), lowered))
            }
        }
    }

    fn thf_var_index(&self, el: &Element) -> Result<u32, ThfDrop> {
        match el {
            Element::Variable { var_index, .. } => Ok(*var_index),
            _ => Err(ThfDrop::UnsupportedHead),
        }
    }
}

/// Rewrite every variable index in `e` through `map` (indices absent from the
/// map are unchanged) — the THF analog of `remap_formula_vars`, for conjoining
/// separately-lowered conjecture roots whose per-root index spaces collide.
#[cfg(feature = "ask")]
fn remap_thf_vars(e: &mut ThfExpr, map: &HashMap<u32, u32>) {
    match e {
        ThfExpr::Var(v) => {
            if let Some(&g) = map.get(v) {
                *v = g;
            }
        }
        ThfExpr::Const(_) | ThfExpr::True | ThfExpr::False => {}
        ThfExpr::App(a, b) | ThfExpr::Imp(a, b) | ThfExpr::Iff(a, b) | ThfExpr::Eq(a, b) => {
            remap_thf_vars(a, map);
            remap_thf_vars(b, map);
        }
        ThfExpr::Not(a) => remap_thf_vars(a, map),
        ThfExpr::And(es) | ThfExpr::Or(es) => {
            for x in es {
                remap_thf_vars(x, map);
            }
        }
        ThfExpr::Lam(v, _, b) | ThfExpr::Forall(v, _, b) | ThfExpr::Exists(v, _, b) => {
            if let Some(&g) = map.get(v) {
                *v = g;
            }
            remap_thf_vars(b, map);
        }
    }
}

// -- problem assembly -----------------------------------------------------------

impl TranslationLayer {
    /// Assemble a complete THF [`HoProblem`] from a pre-selected axiom set
    /// plus a conjecture — the THF sibling of `assemble_problem`.  Reuses the
    /// same synthetic-eligibility scan (rewrite replacements + per-problem
    /// predicate-variable instantiation) on the sid set, then lowers through
    /// the `formulas_thf` cache.
    ///
    /// When `KappaFn` occurs anywhere in the problem, the comprehension axiom
    /// connecting it to `instance` is appended:
    ///   `![P: $i > $o, X: $i]: (instance @ X @ (KappaFn @ P)) <=> (P @ X)`.
    #[cfg(feature = "ask")]
    pub(crate) fn assemble_problem_thf(
        &self,
        axiom_sids:  &[SentenceId],
        seed_sids:   &[SentenceId],
        conjecture:  &[SentenceId],
        query_scope: Option<Scope>,
    ) -> (HoProblem, Vec<SentenceId>) {
        // Same sid-set preparation as the FO assembly.
        let mut sids: Vec<SentenceId> = axiom_sids.to_vec();
        sids.sort_unstable();
        sids.dedup();
        let extra = self.synthetic_replacements(&sids);
        sids.extend(extra);
        let pv = {
            let mut seed: Vec<SentenceId> = conjecture.to_vec();
            seed.extend(seed_sids.iter().copied());
            let mut scope: Vec<SentenceId> = conjecture.to_vec();
            scope.extend(sids.iter().copied());
            self.instantiate_predvars(
                &seed, &scope,
                query_scope.unwrap_or(Scope::Base),
            )
        };
        sids.extend(pv);
        sids.sort_unstable();
        sids.dedup();

        // Prewarm the per-sentence cache in parallel (read-only vs `self`).
        #[cfg(feature = "parallel")]
        {
            use rayon::prelude::*;
            sids.par_iter().for_each(|&sid| {
                let _ = self.formula_thf(sid);
            });
        }

        let mut problem = HoProblem::new();
        let mut sid_map: Vec<SentenceId> = Vec::new();
        let mut dropped = 0usize;
        for &sid in &sids {
            match self.formula_thf(sid) {
                ThfEntry::Formula(cf) => {
                    for d in cf.decls {
                        problem.declare(d);
                    }
                    problem.with_axiom(cf.expr);
                    sid_map.push(sid);
                    for def in cf.defs {
                        problem.with_axiom(def);
                        sid_map.push(sid); // named kb_<sid>_v<n> by the assembler
                    }
                }
                ThfEntry::Dropped(reason) => {
                    dropped += 1;
                    crate::log!(Debug, "sigmakee_rs_core::trans", format!(
                        "thf: dropped sid {sid}: {reason:?}"));
                }
            }
        }
        if dropped > 0 {
            crate::log!(Debug, "sigmakee_rs_core::trans", format!(
                "thf: {dropped} of {} selected sentences dropped", sids.len()));
        }

        // Conjecture (multi-root conjunction, existential wrap).
        if let Some(cf) = self.lower_conjecture_thf(conjecture, query_scope) {
            for d in cf.decls {
                problem.declare(d);
            }
            for def in cf.defs {
                problem.with_axiom(def);
                sid_map.push(*conjecture.first().unwrap_or(&0));
            }
            problem.conjecture(cf.expr);
        }

        // Epistemic conjunction distribution (the K-axiom fragment) for the
        // regular epistemic operators, when present: SUMO's `knows`/`believes`
        // are idealized (consequence-closed) attitudes, and without at least
        // conjunction distribution `knows(a, p ∧ q) ⊢ knows(a, p)` is
        // INVALID for an uninterpreted `$o > $o` operator (a countermodel
        // assigns the operator by truth-value).  Mirrors the tinySUMO THF
        // theory used by SigmaKEE's experiments.
        for rel in ["s__knows", "s__believes"] {
            if problem.decls().iter().any(|d| d.name == rel) {
                // ![A:$i, P:$o, Q:$o]: (rel @ A @ (P & Q)) => ((rel @ A @ P) & (rel @ A @ Q))
                let app = |arg: ThfExpr| {
                    ThfExpr::apply(
                        ThfExpr::Const(rel.to_string()),
                        vec![ThfExpr::Var(0), arg],
                    )
                };
                let lhs = app(ThfExpr::And(vec![ThfExpr::Var(1), ThfExpr::Var(2)]));
                let rhs = ThfExpr::And(vec![app(ThfExpr::Var(1)), app(ThfExpr::Var(2))]);
                let ax = ThfExpr::Forall(
                    0,
                    HoSort::I,
                    Box::new(ThfExpr::Forall(
                        1,
                        HoSort::O,
                        Box::new(ThfExpr::Forall(
                            2,
                            HoSort::O,
                            Box::new(ThfExpr::Imp(Box::new(lhs), Box::new(rhs))),
                        )),
                    )),
                );
                problem.with_axiom(ax);
            }
        }

        // KappaFn comprehension, when the lambda former is in play.
        if problem.decls().iter().any(|d| d.name == format!("s__{KAPPA_FN}")) {
            let inst = ThfConst {
                name: "s__instance".to_string(),
                sort: HoSort::curry(&[HoSort::I, HoSort::I], HoSort::O),
            };
            problem.declare(inst);
            // ![X0: $i > $o]: ![X1: $i]:
            //   (s__instance @ X1 @ (s__KappaFn @ X0)) <=> (X0 @ X1)
            let p = || ThfExpr::Var(0);
            let x = || ThfExpr::Var(1);
            let lhs = ThfExpr::apply(
                ThfExpr::Const("s__instance".into()),
                vec![
                    x(),
                    ThfExpr::App(
                        Box::new(ThfExpr::Const(format!("s__{KAPPA_FN}"))),
                        Box::new(p()),
                    ),
                ],
            );
            let rhs = ThfExpr::App(Box::new(p()), Box::new(x()));
            let body = ThfExpr::Iff(Box::new(lhs), Box::new(rhs));
            let ax = ThfExpr::Forall(
                0,
                HoSort::Fn(Box::new(HoSort::I), Box::new(HoSort::O)),
                Box::new(ThfExpr::Forall(1, HoSort::I, Box::new(body))),
            );
            problem.with_axiom(ax);
            // No sid_map entry: `to_thf` names trailing unmapped axioms ax_<i>.
        }

        (problem, sid_map)
    }
}
