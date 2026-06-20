// crates/core/src/trans/lower.rs
//
// SUMO sentence -> `ir::Formula` lowering, performed directly on the
// `TranslationLayer` and leaning on its per-symbol caches (`sort_annotation`,
// `sort_for_symbol`, `numeric_sorts`).  This is the engine that replaced the
// retired `NativeConverter`: the `formulas_tff` / `formulas_fof` caches call
// [`TranslationLayer::lower_root`], and the export path (`kb/export.rs`) calls
// [`TranslationLayer::lower_axiom`] / [`TranslationLayer::lower_conjecture`].
//
// One engine, switched by a [`Mode`]:
//   * `typed`         — TFF typed-predicate encoding `s__P(a, b)` with per-call-site
//                       sort suffixes and gathered sort/fn/pred declarations; vs
//                       FOF untyped `s__P(a, b)` with no declarations.
//   * `hide_numbers`  — numeric literals as opaque `$i` constants (`n__42`) vs raw
//                       TPTP numbers.  Usually `!typed`, but the export path can
//                       override it independently.
//
// VarIds come straight from `Element::Variable.var_index` (stamped root-wide at
// build time), so there is no per-sentence variable allocator.  Higher-order
// content (a quantifier / operator / relation used as a *term*) is dropped.

use std::collections::{HashMap, HashSet};

use crate::types::InternedSym;
use crate::{Element, Literal, OpKind, Sentence, SentenceId, SymbolId};
use crate::semantics::types::Scope;
use crate::trans::builtins::{numeric_constant_value, SumoArith, TPTPConstant};
use crate::trans::ir::{Formula, Function, Interp, Predicate, Sort as IrSort, Term, VarId};
use crate::trans::sort::numeric_literal_sort;
use crate::trans::term_sorts::{unified_numeric, unify_numeric_call, widen_term, VarSorts};
use crate::trans::{Sort, TranslationLayer};
use crate::types::CachedFormula;

/// Map recorded for a converted conjecture so downstream binding extraction can
/// rejoin the prover's `X<n>` variable names with the original KIF names.
///
/// Produced by [`TranslationLayer::lower_conjecture`]; consumed by the
/// integrated-prover binding extractor
/// (`prover/external/backends/vampire/bindings.rs`).
#[derive(Debug, Default, Clone)]
pub struct QueryVarMap {
    /// Variable `SymbolId` -> (TPTP var index, KIF display name).
    pub var_mapping: std::collections::HashMap<SymbolId, (u32, String)>,
    /// Free-variable symbols, in binder order.
    pub free_vars: Vec<SymbolId>,
}

/// Conversion mode: TFF-vs-FOF encoding plus number-hiding, decoupled so the
/// export path can request raw numbers in either dialect, plus the semantic
/// [`Scope`] the sentence lives in.
#[derive(Clone, Copy)]
struct Mode {
    /// TFF typed encoding (typed predicates, sort suffixes, declarations,
    /// sort-typed quantifiers) when `true`; untyped FOF when `false`.
    typed: bool,
    /// Emit numeric literals as opaque `$i` constants (`n__42`) when `true`.
    hide_numbers: bool,
    /// The scope classification/typing evidence is resolved in: `Base` for
    /// promoted axioms, the owning session for staged assertions — a session
    /// hypothesis like `(equal Value 40.0)` must type `Value` from evidence in
    /// its OWN session, which Base-scoped inference cannot see.
    scope: Scope,
}

/// Per-`CachedFormula` declaration accumulator (TFF only).  Deduped within a
/// single sentence; cross-sentence dedup is the consumer's job.  Built-in sorts
/// and interpreted functions emit nothing.
#[derive(Default)]
struct Decls {
    sorts: Vec<IrSort>,
    fns:   Vec<Function>,
    preds: Vec<Predicate>,
    seen_sorts: HashSet<String>,
    seen_fns:   HashSet<(String, u32)>,
    seen_preds: HashSet<(String, u32)>,
}

impl Decls {
    fn ensure_sort(&mut self, s: &IrSort) {
        if s.is_builtin() {
            return;
        }
        if self.seen_sorts.insert(s.tptp_name().to_string()) {
            self.sorts.push(s.clone());
        }
    }

    fn ensure_pred(&mut self, p: &Predicate) {
        if !p.is_typed() {
            return;
        }
        if self.seen_preds.insert((p.name().to_string(), p.arity())) {
            for s in p.arg_sorts() {
                self.ensure_sort(s);
            }
            self.preds.push(p.clone());
        }
    }

    fn ensure_fn(&mut self, f: &Function) {
        if !f.is_typed() {
            return;
        }
        if self.seen_fns.insert((f.name().to_string(), f.arity())) {
            for s in f.arg_sorts() {
                self.ensure_sort(s);
            }
            if let Some(r) = f.ret_sort() {
                self.ensure_sort(r);
            }
            self.fns.push(f.clone());
        }
    }
}

impl TranslationLayer {
    // -- public entry points -------------------------------------------------

    /// Lower a root sentence to a TFF/FOF [`CachedFormula`] for the formula
    /// caches.  Free variables are universally quantified; `hide_numbers`
    /// follows the dialect (`!typed`).  `None` for suppressed / higher-order /
    /// unconvertible sentences.
    pub(crate) fn lower_root(&self, sid: SentenceId, typed: bool) -> Option<CachedFormula> {
        self.lower_inner(sid, typed, !typed, /*existential*/ false, None, None)
            .map(|(cf, _)| cf)
    }

    /// Lower a sentence as an **axiom** (universal free-var wrap) with an
    /// explicit `hide_numbers`.
    pub(crate) fn lower_axiom(
        &self,
        sid: SentenceId,
        typed: bool,
        hide_numbers: bool,
    ) -> Option<CachedFormula> {
        self.lower_inner(sid, typed, hide_numbers, /*existential*/ false, None, None)
            .map(|(cf, _)| cf)
    }

    /// Lower `sid` as an axiom with explicit variable-sort OVERRIDES layered
    /// over the per-root classification — the polymorphic variant-expansion
    /// entry (`trans/poly_expand.rs`): the same rule lowers once per plausible
    /// numeric sort of its poly-position variables, so it can join facts
    /// emitted at those variants.
    pub(crate) fn lower_axiom_variant(
        &self,
        sid:       SentenceId,
        overrides: &VarSorts,
    ) -> Option<CachedFormula> {
        self.lower_inner(sid, /*typed*/ true, /*hide*/ false, /*existential*/ false, None, Some(overrides))
            .map(|(cf, _)| cf)
    }

    /// Lower a sentence as a **conjecture** (existential free-var wrap), also
    /// returning the [`QueryVarMap`] for proof-binding extraction.  `scope` is
    /// the asking session's scope (the conjecture reasons over the session's
    /// evidence by definition); `None` falls back to the sentence's own owner.
    pub(crate) fn lower_conjecture(
        &self,
        sid: SentenceId,
        typed: bool,
        hide_numbers: bool,
        scope: Option<Scope>,
    ) -> Option<(CachedFormula, QueryVarMap)> {
        self.lower_inner(sid, typed, hide_numbers, /*existential*/ true, scope, None)
    }

    /// Lower a **multi-sentence conjecture** as one conjunction: each `sid`'s
    /// body converts unwrapped, the bodies conjoin, and the UNION of free
    /// variables wraps existentially once.
    ///
    /// This is not equivalent to conjoining per-sid `lower_conjecture` results:
    /// CAF splits an `(and …)` query into separate roots that SHARE variable
    /// ids, so per-conjunct wrapping loses the cross-conjunct linkage — and
    /// proving one arbitrarily-ordered conjunct alone is unsound in both
    /// directions (a bare `(greaterThan ?X ?Y)` conjunct is trivially true).
    /// All-or-nothing: `None` if any conjunct fails to convert.
    pub(crate) fn lower_conjecture_set(
        &self,
        sids: &[SentenceId],
        typed: bool,
        hide_numbers: bool,
        scope: Option<Scope>,
    ) -> Option<(CachedFormula, QueryVarMap)> {
        // Single sid: identical to the plain path.
        if let [sid] = sids {
            return self.lower_conjecture(*sid, typed, hide_numbers, scope);
        }
        if sids.is_empty() {
            return None;
        }
        let mut bodies: Vec<Formula> = Vec::with_capacity(sids.len());
        let mut decls = Decls::default();
        // `var_index` numbering is PER ROOT (each split conjunct numbers its
        // own variables from 0), while SymbolIds are shared across the split —
        // so conjoined bodies must be re-indexed into one global space keyed by
        // SymbolId, or `?C` and `?F` from different conjuncts both emit as
        // `X0` and the conjunction's meaning collapses.
        let mut global_idx: HashMap<SymbolId, u32> = HashMap::new();
        let mut next_idx: u32 = 0;
        let mut bound: HashSet<SymbolId> = HashSet::new();
        let mut var_sorts_all: VarSorts = VarSorts::new();

        for &sid in sids {
            if self.suppressed.read().unwrap().contains(&sid) {
                return None;
            }
            let s = scope.unwrap_or_else(|| self.scope_of(sid));
            let mode = Mode { typed, hide_numbers, scope: s };
            let sentence = self.semantic.syntactic.sentence(sid)?;

            // This root's local variable numbering, in local-index order for
            // deterministic global assignment.
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

            let var_sorts: VarSorts = if typed {
                self.semantic
                    .classify_formula_scoped(sid, s)
                    .into_iter()
                    .filter(|(k, _)| vids.contains_key(k))
                    .map(|(k, sc)| (k, self.class_inference_to_sort(&sc.class)))
                    .collect()
            } else {
                VarSorts::new()
            };

            let mut body = self.sid_to_formula(&sentence, mode, &var_sorts, &mut decls)?;
            remap_formula_vars(&mut body, &remap);
            bodies.push(body);
            self.semantic.syntactic.collect_bound_vars(sid, true, &mut bound);
            // Shared variable ids must agree; a later conjunct's evidence can
            // only sharpen (most-specific wins via `max`).
            for (k, v) in var_sorts {
                var_sorts_all
                    .entry(k)
                    .and_modify(|e| *e = (*e).max(v))
                    .or_insert(v);
            }
        }

        // Only `typed` matters for the wrap; the per-sid scopes have already
        // been folded into `var_sorts_all`.
        let mode = Mode { typed, hide_numbers, scope: scope.unwrap_or(Scope::Base) };
        let mut free: Vec<(SymbolId, u32)> = global_idx
            .iter()
            .filter(|(id, _)| !bound.contains(id))
            .map(|(id, idx)| (*id, *idx))
            .collect();
        free.sort_by_key(|&(_, idx)| idx);

        let mut formula = Formula::and(bodies);
        for &(id, idx) in &free {
            formula = self.wrap_quantifier(idx, id, formula, /*exist*/ true, mode, &var_sorts_all);
        }

        let qvm = QueryVarMap {
            var_mapping: global_idx
                .iter()
                .map(|(&id, &idx)| (id, (idx, self.sym_name_string(id))))
                .collect(),
            free_vars: free.iter().map(|&(id, _)| id).collect(),
        };
        let cf = CachedFormula {
            formula,
            sort_decls: decls.sorts,
            fn_decls: decls.fns,
            pred_decls: decls.preds,
        };
        Some((cf, qvm))
    }

    /// The scope a sentence's classification evidence resolves in: `Base` for
    /// promoted axioms, the owning session for staged assertions (first owner
    /// when several — content-addressed re-asserts share one translation).
    pub(in crate::trans) fn scope_of(&self, sid: SentenceId) -> Scope {
        let syn = &self.semantic.syntactic;
        if syn.sessions.is_axiom(sid) {
            return Scope::Base;
        }
        match syn.sessions.sessions_of(sid).first() {
            Some(&s) => Scope::Session(s),
            None     => Scope::Base,
        }
    }

    /// The shared core: convert the body, wrap free vars (universal for axioms,
    /// existential for conjectures), and build the query-variable map.
    fn lower_inner(
        &self,
        sid: SentenceId,
        typed: bool,
        hide_numbers: bool,
        existential: bool,
        scope_override: Option<Scope>,
        sort_overrides: Option<&VarSorts>,
    ) -> Option<(CachedFormula, QueryVarMap)> {
        let scope = scope_override.unwrap_or_else(|| self.scope_of(sid));
        let mode = Mode { typed, hide_numbers, scope };

        // Suppressed sentences (rewrite-pass originals replaced by a synthetic,
        // predicate-variable schema templates, …) must not be emitted — checked
        // before the classification walk below, which is the expensive part.
        if self.suppressed.read().unwrap().contains(&sid) {
            return None;
        }
        let sentence = self.semantic.syntactic.sentence(sid)?;

        // All variables of the root, collected once: the classification filter,
        // the query map, and the free-var wrap all read this.
        let mut all: HashMap<SymbolId, u32> = HashMap::new();
        self.semantic.syntactic.collect_vars(sid, &mut all);

        // Variable sorts come from the formula ITSELF (instance guards, domain
        // positions, defining literal equalities), resolved once per root.
        let mut var_sorts: VarSorts = if typed {
            self.semantic
                .classify_formula_scoped(sid, scope)
                .into_iter()
                .filter(|(k, _)| all.contains_key(k))
                .map(|(k, sc)| (k, self.class_inference_to_sort(&sc.class)))
                .collect()
        } else {
            VarSorts::new()
        };
        // Variant expansion pins specific variables to specific numeric sorts
        // on top of (and overriding) the classification.
        if let Some(ov) = sort_overrides {
            for (k, v) in ov {
                var_sorts.insert(*k, *v);
            }
        }
        let mut decls = Decls::default();
        let body = self.sid_to_formula(&sentence, mode, &var_sorts, &mut decls)?;

        let mut bound: HashSet<SymbolId> = HashSet::new();
        self.semantic.syntactic.collect_bound_vars(sid, true, &mut bound);
        let mut free: Vec<(SymbolId, u32)> = all
            .iter()
            .filter(|(id, _)| !bound.contains(id))
            .map(|(id, idx)| (*id, *idx))
            .collect();
        free.sort_by_key(|&(_, idx)| idx); // determinism

        let mut formula = body;
        for &(id, idx) in &free {
            formula = self.wrap_quantifier(idx, id, formula, existential, mode, &var_sorts);
        }

        let qvm = QueryVarMap {
            var_mapping: all
                .iter()
                .map(|(&id, &idx)| (id, (idx, self.sym_name_string(id))))
                .collect(),
            free_vars: free.iter().map(|&(id, _)| id).collect(),
        };

        let cf = CachedFormula {
            formula,
            sort_decls: decls.sorts,
            fn_decls: decls.fns,
            pred_decls: decls.preds,
        };
        Some((cf, qvm))
    }

    fn sym_name_string(&self, id: SymbolId) -> String {
        self.semantic
            .syntactic
            .sym_name(id)
            .map(|s| s.name().to_string())
            .unwrap_or_default()
    }

    // -- formula construction ------------------------------------------------

    /// Dispatch a sentence to its formula form (no top-level free-var wrap —
    /// that happens once, in `lower_inner`).
    fn sid_to_formula(
        &self,
        sentence: &Sentence,
        mode: Mode,
        vars: &VarSorts,
        decls: &mut Decls,
    ) -> Option<Formula> {
        match sentence.elements.first()? {
            Element::Op(_) => self.operator_to_formula(sentence, mode, vars, decls),
            Element::Symbol(_) => self.atomic_to_formula(sentence, mode, vars, decls),
            // A variable / literal / sub head is a predicate-variable atom or
            // otherwise not a first-order formula head — drop it.
            _ => None,
        }
    }

    /// Map an operator-headed sentence to its `Formula`.  Connectives are
    /// all-or-`None` (a single unconvertible child drops the whole sentence,
    /// preserving soundness).
    fn operator_to_formula(
        &self,
        sentence: &Sentence,
        mode: Mode,
        vars: &VarSorts,
        decls: &mut Decls,
    ) -> Option<Formula> {
        let op = sentence.op()?;
        let args = &sentence.elements[1..];
        match op {
            OpKind::And | OpKind::Or => {
                let mut fs = Vec::with_capacity(args.len());
                for e in args {
                    fs.push(self.element_to_formula(e, mode, vars, decls)?);
                }
                if fs.is_empty() {
                    return None;
                }
                Some(if matches!(op, OpKind::And) { Formula::and(fs) } else { Formula::or(fs) })
            }
            OpKind::Not => Some(Formula::not(self.element_to_formula(args.first()?, mode, vars, decls)?)),
            OpKind::Implies => {
                let a = self.element_to_formula(args.first()?, mode, vars, decls)?;
                let b = self.element_to_formula(args.get(1)?, mode, vars, decls)?;
                Some(Formula::imp(a, b))
            }
            OpKind::Iff => {
                let a = self.element_to_formula(args.first()?, mode, vars, decls)?;
                let b = self.element_to_formula(args.get(1)?, mode, vars, decls)?;
                Some(Formula::iff(a, b))
            }
            OpKind::Equal => {
                let a_el = args.first()?;
                let b_el = args.get(1)?;
                // Each side is converted with the OTHER side's inferred sort as
                // its expected sort, so a typed term widens an untyped literal.
                let a_sort = if mode.typed { self.infer_term_sort(a_el, mode.scope, vars) } else { None };
                let b_sort = if mode.typed { self.infer_term_sort(b_el, mode.scope, vars) } else { None };
                if mode.typed {
                    if let (Some(sa), Some(sb)) = (a_sort, b_sort) {
                        let numeric = |s: Sort| s != Sort::Individual;
                        // Mixed $i / numeric equality is not TFF-typable.
                        if numeric(sa) != numeric(sb) {
                            return None;
                        }
                    }
                }
                let a = self.element_to_term(a_el, b_sort, a_sort, mode, vars, decls)?;
                let b = self.element_to_term(b_el, a_sort, b_sort, mode, vars, decls)?;
                Some(Formula::eq(a, b))
            }
            OpKind::ForAll | OpKind::Exists => {
                let exist = matches!(op, OpKind::Exists);
                // args[0] is the variable list `(?a ?b ...)` as a sub-sentence.
                let ids: Vec<(SymbolId, u32)> = match args.first()? {
                    Element::Sub(vl_sid) => {
                        let vl = self.semantic.syntactic.sentence(*vl_sid)?;
                        vl.elements
                            .iter()
                            .filter_map(|e| match e {
                                Element::Variable { id, var_index, .. } => Some((*id, *var_index)),
                                _ => None,
                            })
                            .collect()
                    }
                    _ => Vec::new(),
                };
                let body = self.element_to_formula(args.get(1)?, mode, vars, decls)?;
                let mut f = body;
                for (id, idx) in ids.into_iter().rev() {
                    f = self.wrap_quantifier(idx, id, f, exist, mode, vars);
                }
                Some(f)
            }
        }
    }

    /// A child element in *formula* position: a sub-sentence recurses; `True` /
    /// `False` map to the logical constants; anything else is not a formula.
    fn element_to_formula(&self, el: &Element, mode: Mode, vars: &VarSorts, decls: &mut Decls) -> Option<Formula> {
        match el {
            Element::Sub(sid) => {
                let s = self.semantic.syntactic.sentence(*sid)?;
                self.sid_to_formula(&s, mode, vars, decls)
            }
            Element::Symbol(sym) => TPTPConstant::from_sym(&sym.name()).map(Into::into),
            _ => None,
        }
    }

    /// Build the `Formula::atom` for a Symbol-headed sentence `(P a b ...)`.
    fn atomic_to_formula(&self, sentence: &Sentence, mode: Mode, vars: &VarSorts, decls: &mut Decls) -> Option<Formula> {
        let n_args = sentence.elements.len().saturating_sub(1);
        let head = match sentence.elements.first()? {
            Element::Symbol(sym) => sym,
            _ => return None,
        };
        // A function symbol can't head a first-order formula.
        if self.semantic.is_function_scoped(head.id(), mode.scope) {
            return None;
        }
        let elems = &sentence.elements[1..];

        if mode.typed {
            // Raw inferred sort per argument, kept for the widening tail
            // (`actual`), then canonicalized/unified into the expected sorts.
            let actual_sorts: Vec<Option<Sort>> = elems
                .iter()
                .map(|e| self.infer_term_sort(e, mode.scope, vars))
                .collect();
            // Declaration-driven: declared-numeric positions pin the variant,
            // call-site inference fills the undeclared/incompatible rest —
            // every use of a declared relation lands on the same variant.
            let mut call_sorts: Vec<Sort> =
                self.resolve_call_sorts(head.id(), &actual_sorts, mode.scope);
            // An interpreted comparison ($less/$greater/…) requires every
            // argument at a single numeric sort: unify an all-numeric call to
            // the widest member so narrower terms widen up (`$to_real`), same
            // as the interpreted-function path in `sub_to_term`.
            if let Some(arith) = SumoArith::from_sumo_name(&head.name()) {
                if arith.is_predicate() {
                    unify_numeric_call(&mut call_sorts);
                }
            }
            let pred = self.tff_pred(head, &call_sorts, decls);
            let mut args = Vec::with_capacity(n_args);
            for (i, e) in elems.iter().enumerate() {
                let expected = call_sorts.get(i).copied();
                args.push(self.element_to_term(e, expected, actual_sorts[i], mode, vars, decls)?);
            }
            Some(Formula::atom(pred, args))
        } else {
            let pred = Predicate::new(&self.rel_name(head, n_args), n_args as u32);
            let mut args = Vec::with_capacity(n_args);
            for e in elems {
                args.push(self.element_to_term(e, None, None, mode, vars, decls)?);
            }
            Some(Formula::atom(pred, args))
        }
    }

    /// The 2×2 quantifier wrap (universal/existential × FOF/TFF).  TFF binds the
    /// variable at its scope-resolved sort — the same source the argument sort
    /// inference uses, so a binder and its occurrences never disagree.
    fn wrap_quantifier(
        &self,
        idx: u32,
        var_id: SymbolId,
        body: Formula,
        exist: bool,
        mode: Mode,
        vars: &VarSorts,
    ) -> Formula {
        if mode.typed {
            // The binder sort comes from the per-root formula classification —
            // the same map `infer_term_sort` reads for occurrences, so binder
            // and use sites never disagree.
            let sort: IrSort = vars.get(&var_id).copied().unwrap_or(Sort::Individual).into();
            if exist {
                Formula::exists_typed(VarId(idx), sort, body)
            } else {
                Formula::forall_typed(VarId(idx), sort, body)
            }
        } else if exist {
            Formula::exists(VarId(idx), body)
        } else {
            Formula::forall(VarId(idx), body)
        }
    }

    // -- term construction ---------------------------------------------------

    /// Convert an argument element to a `Term`.  `expected` is the sort the
    /// enclosing position wants (TFF only); a numeric term narrower than
    /// `expected` is widened with a `$to_real` / `$to_rat` coercion.  `actual`
    /// is the caller's already-inferred raw sort for `el` (every caller has
    /// just computed it for the call-sort vector) so the widening tail does
    /// not re-run the recursive inference per node.
    fn element_to_term(
        &self,
        el: &Element,
        expected: Option<Sort>,
        actual: Option<Sort>,
        mode: Mode,
        vars: &VarSorts,
        decls: &mut Decls,
    ) -> Option<Term> {
        if let Element::Literal(lit) = el {
            return self.literal_to_term(lit, expected, mode);
        }

        let t = match el {
            Element::Symbol(sym) => {
                let id = sym.id();
                // TFF: a recognised numeric constant (Pi, NaperianBase) emits as
                // a numeric literal.
                if mode.typed {
                    if let Some(v) = numeric_constant_value(&sym.name()) {
                        return self.literal_to_term(&Literal::Number(v.to_string()), expected, mode);
                    }
                }
                if self.semantic.is_relation_scoped(id, mode.scope) {
                    // A relation used as a value → its `$i` mention constant.
                    Term::constant(Function::new(&sym.tptp_mention_name(), 0))
                } else if mode.typed {
                    if let Some(sort) = self.typed_constant_sort(id, mode.scope) {
                        let f = Function::typed(&sym.tptp_sym_name(), &[], sort.into());
                        decls.ensure_fn(&f);
                        Term::constant(f)
                    } else {
                        Term::constant(Function::new(&sym.tptp_sym_name(), 0))
                    }
                } else {
                    Term::constant(Function::new(&sym.tptp_sym_name(), 0))
                }
            }
            Element::Variable { var_index, .. } => Term::var(*var_index),
            Element::Sub(sid) => self.sub_to_term(*sid, mode, vars, decls)?,
            Element::Op(op) => Term::constant(Function::new(&op.tptp_sym_name(), 0)),
            Element::Literal(_) => unreachable!("handled above"),
        };

        // TFF widening tail: coerce a numeric term up to the expected sort.
        if mode.typed {
            if let (Some(exp), Some(act)) = (expected, actual) {
                if let Some(coerced) = widen_term(t.clone(), act, exp) {
                    return Some(coerced);
                }
            }
        }
        Some(t)
    }

    /// Convert a sub-sentence in *term* position: a function application becomes
    /// `Term::apply`; a quantifier / operator / relation used as a value is
    /// higher-order and is dropped (`None`).
    fn sub_to_term(&self, sid: SentenceId, mode: Mode, vars: &VarSorts, decls: &mut Decls) -> Option<Term> {
        let sentence = self.semantic.syntactic.sentence(sid)?;
        let n_args = sentence.elements.len().saturating_sub(1);

        let head = match sentence.elements.first()? {
            Element::Symbol(sym) if self.semantic.is_function_scoped(sym.id(), mode.scope) => sym,
            // Quantifier / operator / relation in term position = higher-order.
            _ => return None,
        };
        let elems = &sentence.elements[1..];

        // Per-call-site argument sorts (TFF): raw inferred sorts (kept as the
        // widening tail's `actual`), then declared-`Number` canonicalization
        // and interpreted-arithmetic unification, same as the predicate path.
        let mut actual_sorts: Vec<Option<Sort>> = Vec::new();
        let mut call_sorts: Vec<Sort> = Vec::new();
        if mode.typed {
            actual_sorts = elems
                .iter()
                .map(|e| self.infer_term_sort(e, mode.scope, vars))
                .collect();
            // Same declaration-driven resolution as the predicate path.
            call_sorts = self.resolve_call_sorts(head.id(), &actual_sorts, mode.scope);
            if let Some(arith) = SumoArith::from_sumo_name(&head.name()) {
                if !arith.is_predicate() {
                    unify_numeric_call(&mut call_sorts);
                }
            }
        }

        let mut args = Vec::with_capacity(n_args);
        for (i, e) in elems.iter().enumerate() {
            let (expected, actual) = if mode.typed {
                (call_sorts.get(i).copied(), actual_sorts[i])
            } else {
                (None, None)
            };
            args.push(self.element_to_term(e, expected, actual, mode, vars, decls)?);
        }
        if args.len() != n_args {
            return None;
        }

        let func = if mode.typed {
            self.tff_fn(head, &call_sorts, mode.scope, decls)?
        } else {
            Function::new(&self.rel_name(head, n_args), n_args as u32)
        };
        Some(Term::apply(func, args))
    }

    /// Convert a literal to a `Term`, honouring the expected sort (widening
    /// integer literals up) and `mode.hide_numbers` (opaque `$i` constants).
    fn literal_to_term(&self, lit: &Literal, expected: Option<Sort>, mode: Mode) -> Option<Term> {
        match lit {
            Literal::Number(n) => {
                if mode.hide_numbers {
                    // Opaque `$i` constant (`42` -> `n__42`, `-5` -> `n__neg_5`).
                    let safe = n.replace('.', "_").replace('-', "neg_");
                    return Some(Term::constant(Function::new(&format!("n__{safe}"), 0)));
                }
                let shape = numeric_literal_sort(n);
                if self.reals_only() {
                    // Reals-only mode: every literal emits at `$real`.
                    // Integer-shaped gains a decimal point (TPTP reals need
                    // one); a rational `a/b` becomes interpreted real
                    // division — exact and coercion-free.
                    return Some(match shape {
                        Sort::Integer => Term::real(format!("{n}.0")),
                        Sort::Rational => {
                            let (a, b) = n.split_once('/')?;
                            Term::apply(
                                Function::interpreted("$quotient", Interp::RealQuotient),
                                vec![Term::real(format!("{a}.0")), Term::real(format!("{b}.0"))],
                            )
                        }
                        _ => Term::real(n.clone()),
                    });
                }
                // Raw-number widening pre-pass: an integer-shaped literal in a
                // wider expected position emits directly at that sort.
                if shape == Sort::Integer {
                    match expected {
                        Some(Sort::Real) => return Some(Term::real(format!("{n}.0"))),
                        Some(Sort::Rational) => return Some(Term::rational(format!("{n}/1"))),
                        _ => {}
                    }
                }
                Some(match shape {
                    Sort::Rational => Term::rational(n.clone()),
                    Sort::Real => Term::real(n.clone()),
                    _ => Term::int(n.clone()),
                })
            }
            Literal::Str(s) => {
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
                Some(Term::constant(Function::new(&name, 0)))
            }
        }
    }

    // -- TFF typing helpers --------------------------------------------------

    /// The base TPTP relation name (`s__P`), with an arity suffix for variadic
    /// relations so each fixed-arity use gets a distinct symbol.
    pub(in crate::trans) fn rel_name(&self, head: &InternedSym, actual_arity: usize) -> String {
        let base = head.tptp_sym_name();
        if self.semantic.arity(head.id()) == Some(-1) {
            format!("{base}__{actual_arity}")
        } else {
            base
        }
    }

    /// Build the typed predicate for `(P args)` (sort-suffixed variant name +
    /// declaration).  A SUMO comparison over all-numeric arguments maps to the
    /// TPTP interpreted theory predicate (`$greater`, `$less`, …) — no
    /// declaration needed — unlocking native arithmetic evaluation
    /// (`$greater(-100.0, 0.0)` reduces to `$false`).  Mixed / non-numeric
    /// calls stay opaque (`s__lessThan…`).
    fn tff_pred(&self, head: &InternedSym, arg_sorts: &[Sort], decls: &mut Decls) -> Predicate {
        if let Some(arith) = SumoArith::from_sumo_name(&head.name()) {
            if arith.is_predicate() {
                if let Some(sort) = unified_numeric(arg_sorts) {
                    if let Some(p) = arith.to_ir_pred(sort) {
                        return p;
                    }
                }
            }
        }
        let name = format!(
            "{}{}",
            self.rel_name(head, arg_sorts.len()),
            tff_sort_suffix(arg_sorts, None),
        );
        let ir_args: Vec<IrSort> = arg_sorts.iter().map(|s| (*s).into()).collect();
        let p = Predicate::typed(&name, &ir_args);
        decls.ensure_pred(&p);
        p
    }

    /// Build the typed function for a function application.  A SUMO arithmetic
    /// function with all-numeric arguments becomes an interpreted theory
    /// function (`$sum`, …); everything else is a sort-suffixed typed function.
    fn tff_fn(
        &self,
        head:      &InternedSym,
        arg_sorts: &[Sort],
        scope:     Scope,
        decls:     &mut Decls,
    ) -> Option<Function> {
        if let Some(arith) = SumoArith::from_sumo_name(&head.name()) {
            if !arith.is_predicate() {
                // `arg_sorts` arrive pre-unified from `sub_to_term`, so the
                // widest member IS the call's single numeric sort.
                if let Some(sort) = unified_numeric(arg_sorts) {
                    if let Some(f) = arith.to_ir_fn(sort) {
                        return Some(f);
                    }
                }
            }
        }
        let ret = self.ret_sort(head.id(), scope);
        let name = format!(
            "{}{}",
            self.rel_name(head, arg_sorts.len()),
            tff_sort_suffix(arg_sorts, Some(ret)),
        );
        let ir_args: Vec<IrSort> = arg_sorts.iter().map(|s| (*s).into()).collect();
        let f = Function::typed(&name, &ir_args, ret.into());
        decls.ensure_fn(&f);
        Some(f)
    }

}

/// The TFF sort-suffix for a relation/function variant: each non-Individual
/// position contributes `<pos><code>` (`In`/`Ra`/`Re`), with the return at
/// position 0 for functions.  All-Individual signatures get the empty suffix.
fn tff_sort_suffix(args: &[Sort], ret: Option<Sort>) -> String {
    let any_typed = args.iter().any(|s| *s != Sort::Individual)
        || ret.is_some_and(|r| r != Sort::Individual);
    if !any_typed {
        return String::new();
    }
    let code = |s: Sort| -> Option<&'static str> {
        match s {
            Sort::Integer => Some("In"),
            Sort::Rational => Some("Ra"),
            Sort::Real => Some("Re"),
            Sort::Individual => None,
        }
    };
    let mut out = String::from("__");
    let mut pos: u32 = 0;
    if let Some(r) = ret {
        if let Some(c) = code(r) {
            out.push_str(&format!("{pos}{c}"));
        }
        pos += 1;
    }
    for a in args {
        if let Some(c) = code(*a) {
            out.push_str(&format!("{pos}{c}"));
        }
        pos += 1;
    }
    out.push_str(if ret.is_some() { "Fn" } else { "Pred" });
    out
}

/// Rewrite every variable index in `f` (binders and occurrences) through
/// `map`.  Indices absent from the map are left unchanged.  Used when
/// conjoining separately-lowered bodies whose per-root `var_index` spaces
/// collide.
fn remap_formula_vars(f: &mut Formula, map: &HashMap<u32, u32>) {
    fn remap_var(v: &mut VarId, map: &HashMap<u32, u32>) {
        if let Some(&g) = map.get(&v.0) {
            v.0 = g;
        }
    }
    fn remap_term(t: &mut Term, map: &HashMap<u32, u32>) {
        match t {
            Term::Var(v) => remap_var(v, map),
            Term::Apply(_, args) => {
                for a in args {
                    remap_term(a, map);
                }
            }
            Term::Int(_) | Term::Real(_) | Term::Rational(_) => {}
        }
    }
    match f {
        Formula::Atom { args, .. } => {
            for a in args {
                remap_term(a, map);
            }
        }
        Formula::Eq(l, r) | Formula::EqTyped { lhs: l, rhs: r, .. } => {
            remap_term(l, map);
            remap_term(r, map);
        }
        Formula::And(fs) | Formula::Or(fs) => {
            for g in fs {
                remap_formula_vars(g, map);
            }
        }
        Formula::Not(g) => remap_formula_vars(g, map),
        Formula::Imp(a, b) | Formula::Iff(a, b) => {
            remap_formula_vars(a, map);
            remap_formula_vars(b, map);
        }
        Formula::Forall(v, g)
        | Formula::Exists(v, g) => {
            remap_var(v, map);
            remap_formula_vars(g, map);
        }
        Formula::ForallTyped(v, _, g)
        | Formula::ExistsTyped(v, _, g) => {
            remap_var(v, map);
            remap_formula_vars(g, map);
        }
        Formula::True | Formula::False => {}
    }
}



