// crates/sumo-kb/src/vampire/converter.rs
//
// Native converter: KIF sentence -> vampire_prover::ir::Formula.
//
// Produces pure-Rust IR values that can be consumed by either the embedded
// solver (`lower_problem(...).solve()`) or the subprocess solver
// (`problem.to_tptp()` piped to vampire stdin).  Declarations for typed
// sorts, functions, and predicates are registered on the Problem as the
// conversion proceeds, so the resulting Problem can be serialised directly
// without a separate preamble pass.
//
// Two modes are supported:
//
//   Mode::Tff: direct typed-predicate encoding
//     `(instance A Entity)` -> `instance(A, Entity)` with
//     `Predicate::typed("instance", &[$i, $i])` declared once.
//
//   Mode::Fof: holds-reification encoding
//     `(instance A Entity)` -> `s__holds(s__instance__m, A, Entity)` with
//     `Predicate::new("s__holds", 3)`.
//
// Gated: requires the `vampire` feature.

use std::collections::{HashMap, HashSet};

use vampire_prover::ir::{
    Formula as IrF, Function as IrFn, Predicate as IrPd, Problem as IrProblem,
    Sort as IrSort, Term as IrT, VarId,
};

use crate::kif_store::KifStore;
use crate::parse::ast::OpKind;
use crate::semantic::{SemanticLayer, Sort as KifSort};
use crate::types::{Element, Literal, SentenceId, SymbolId};

/// TPTP dialect used by the produced problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Fof,
    Tff,
}

const S: &str = "s__";
const M: &str = "__m";

fn sym_name(name: &str) -> String {
    format!("{}{}", S, name.replace('.', "_").replace('-', "_"))
}

fn mention_name(name: &str) -> String {
    format!("{}{}{}", S, name.replace('.', "_").replace('-', "_"), M)
}

/// Map recorded for each converted conjecture so that downstream binding
/// extraction can rejoin Vampire's `X<n>` variable names with the original
/// KIF names.
#[derive(Debug, Default, Clone)]
pub struct QueryVarMap {
    /// Variable index -> KIF variable name.
    pub idx_to_kif: HashMap<u32, String>,
    /// Free-variable indices in sorted order.
    pub free_var_indices: Vec<u32>,
}

/// Stateful builder that walks KIF sentences and produces an `ir::Problem`.
///
/// The builder owns per-sentence variable allocation state (`vars`,
/// `var_ids`, `next_var`) which is reset at the start of every
/// `add_axiom` / `set_conjecture` call, and cross-sentence declaration
/// dedup state (`declared_*`).  `sid_map` records the SentenceId of each
/// axiom in insertion order so callers can perform proof back-translation.
pub struct NativeConverter<'a> {
    store: &'a KifStore,
    layer: &'a SemanticLayer,
    problem: IrProblem,
    mode: Mode,

    /// When `true`, numeric literals are encoded as opaque symbolic
    /// constants (`n__42`, `n__3_14`).  When `false`, they're emitted as
    /// raw TPTP integer / real literals.  Default: `true` — matches the
    /// existing embedded-prover code path, which never interprets numbers.
    hide_numbers: bool,

    // -- per-sentence state (reset per add_axiom / set_conjecture) ------------
    vars:     HashMap<String, u32>,
    var_ids:  HashMap<String, SymbolId>,
    next_var: u32,

    // -- cross-sentence state ------------------------------------------------
    declared_sorts: HashSet<String>,
    declared_funcs: HashSet<(String, u32)>,
    declared_preds: HashSet<(String, u32)>,
    sid_map:        Vec<SentenceId>,
}

impl<'a> NativeConverter<'a> {
    /// Construct a converter with a fresh empty problem in the given mode.
    pub fn new(store: &'a KifStore, layer: &'a SemanticLayer, mode: Mode) -> Self {
        let problem = match mode {
            Mode::Tff => IrProblem::new_tff(),
            Mode::Fof => IrProblem::new(),
        };
        Self::from_parts(store, layer, problem, Vec::new(), mode)
    }

    /// Construct a converter that extends an already-populated problem.
    ///
    /// Used by callers that clone a cached axiom problem and want to add
    /// further assertions or a conjecture on top.  `sid_map` must be the
    /// companion vector for the seed problem's axioms.  Declaration state
    /// is reset — the caller is responsible for ensuring the seed problem
    /// already contains its own declarations (which it will if it came
    /// from a previous `NativeConverter`).
    pub fn from_parts(
        store:   &'a KifStore,
        layer:   &'a SemanticLayer,
        problem: IrProblem,
        sid_map: Vec<SentenceId>,
        mode:    Mode,
    ) -> Self {
        // Seed the declared-symbol sets from the problem's existing decls so
        // we don't re-register anything when extending.
        let declared_sorts: HashSet<String> = problem
            .sort_decls()
            .iter()
            .map(|s| s.tptp_name().to_string())
            .collect();
        let declared_funcs: HashSet<(String, u32)> = problem
            .fn_decls()
            .iter()
            .map(|f| (f.name().to_string(), f.arity()))
            .collect();
        let declared_preds: HashSet<(String, u32)> = problem
            .pred_decls()
            .iter()
            .map(|p| (p.name().to_string(), p.arity()))
            .collect();

        Self {
            store,
            layer,
            problem,
            mode,
            hide_numbers: true,
            vars: HashMap::new(),
            var_ids: HashMap::new(),
            next_var: 0,
            declared_sorts,
            declared_funcs,
            declared_preds,
            sid_map,
        }
    }

    /// Toggle numeric-literal encoding.  `true` (default) emits `n__<N>`
    /// symbolic constants; `false` emits raw TPTP numeric literals.
    pub fn with_hide_numbers(mut self, hide: bool) -> Self {
        self.hide_numbers = hide;
        self
    }

    /// Convert and append `sid` as an axiom. Returns `true` on success,
    /// `false` if the sentence could not be converted.
    pub fn add_axiom(&mut self, sid: SentenceId) -> bool {
        let Some(f) = self.sid_to_top(sid, /*existential=*/ false) else { return false };
        self.problem.with_axiom(f);
        self.sid_map.push(sid);
        true
    }

    /// Convert and install `sid` as the problem's conjecture (existentially
    /// wrapping any free variables).  Returns a `QueryVarMap` recording the
    /// conjecture's free variables so bindings can be resolved after the
    /// solve.
    pub fn set_conjecture(&mut self, sid: SentenceId) -> Option<QueryVarMap> {
        let qvm = self.query_var_map_for(sid);
        let f = self.sid_to_top(sid, /*existential=*/ true)?;
        self.problem.conjecture(f);
        Some(qvm)
    }

    /// Consume the converter and return the accumulated problem plus the
    /// `sid_map` (one entry per axiom, in insertion order).
    pub fn finish(self) -> (IrProblem, Vec<SentenceId>) {
        (self.problem, self.sid_map)
    }

    // -- Per-sentence entry point ---------------------------------------------

    fn sid_to_top(&mut self, sid: SentenceId, existential: bool) -> Option<IrF> {
        self.reset_sentence_state();
        self.alloc_vars(sid);
        let mut bound: HashSet<String> = HashSet::new();
        collect_bound_var_names(sid, self.store, &mut bound);
        let body = self.sid_to_formula(sid)?;
        Some(self.wrap_free_vars(body, &bound, existential))
    }

    fn reset_sentence_state(&mut self) {
        self.vars.clear();
        self.var_ids.clear();
        self.next_var = 0;
    }

    fn alloc_vars(&mut self, sid: SentenceId) {
        collect_all_var_ids(sid, self.store, &mut self.var_ids);
        for name in self.var_ids.keys() {
            self.vars.entry(name.clone()).or_insert_with(|| {
                let idx = self.next_var;
                self.next_var += 1;
                idx
            });
        }
    }

    fn query_var_map_for(&mut self, sid: SentenceId) -> QueryVarMap {
        let mut var_ids: HashMap<String, SymbolId> = HashMap::new();
        collect_all_var_ids(sid, self.store, &mut var_ids);
        let mut bound: HashSet<String> = HashSet::new();
        collect_bound_var_names(sid, self.store, &mut bound);

        let mut sorted_names: Vec<&String> = var_ids.keys().collect();
        sorted_names.sort();

        let mut idx_to_kif = HashMap::new();
        let mut free_var_indices = Vec::new();
        let mut next = self.next_var; // will be re-aligned inside sid_to_top
        for name in &sorted_names {
            let idx = next;
            next += 1;
            idx_to_kif.insert(idx, name.to_string());
            if !bound.contains(*name) {
                free_var_indices.push(idx);
            }
        }
        QueryVarMap { idx_to_kif, free_var_indices }
    }

    // -- Declaration registration ---------------------------------------------

    /// Register a sort declaration on the Problem if it hasn't been seen.
    fn ensure_sort(&mut self, sort: &IrSort) {
        if sort.is_builtin() {
            return;
        }
        let key = sort.tptp_name().to_string();
        if self.declared_sorts.insert(key) {
            self.problem.declare_sort(sort.clone());
        }
    }

    /// Register a typed function declaration if new.  Untyped/interpreted
    /// functions produce no declaration, so calls on them are no-ops.
    fn ensure_fn(&mut self, f: &IrFn) {
        if !f.is_typed() {
            return;
        }
        let key = (f.name().to_string(), f.arity());
        if self.declared_funcs.insert(key) {
            for s in f.arg_sorts() {
                self.ensure_sort(s);
            }
            if let Some(r) = f.ret_sort() {
                self.ensure_sort(r);
            }
            self.problem.declare_function(f.clone());
        }
    }

    /// Register a typed predicate declaration if new.
    fn ensure_pred(&mut self, p: &IrPd) {
        if !p.is_typed() {
            return;
        }
        let key = (p.name().to_string(), p.arity());
        if self.declared_preds.insert(key) {
            for s in p.arg_sorts() {
                self.ensure_sort(s);
            }
            self.problem.declare_predicate(p.clone());
        }
    }

    // -- Variable / sort helpers ---------------------------------------------

    fn var_term(&self, name: &str) -> IrT {
        let idx = self.vars.get(name).copied().unwrap_or_else(|| {
            log::warn!(target: "sumo_kb::converter",
                "unknown variable '{}' -- defaulting to index 0", name);
            0
        });
        IrT::var(idx)
    }

    /// Return the declared argument sorts for `id`, padded or truncated to
    /// `n_args`.  Missing positions default to `Individual`.
    fn arg_sorts(&self, id: SymbolId, n_args: usize) -> Vec<KifSort> {
        let sa = self.layer.sort_annotations();
        let sa = sa.as_ref().unwrap();
        let base = sa.symbol_arg_sorts.get(&id).cloned().unwrap_or_default();
        let last = base.last().copied().unwrap_or(KifSort::Individual);
        (0..n_args).map(|i| base.get(i).copied().unwrap_or(last)).collect()
    }

    fn ret_sort(&self, id: SymbolId) -> KifSort {
        let sa = self.layer.sort_annotations();
        sa.as_ref()
            .and_then(|sa| sa.symbol_return_sorts.get(&id).copied())
            .unwrap_or(KifSort::Individual)
    }

    // -- Symbol builders (mode-aware) ----------------------------------------

    /// Build an IR Function for a KIF function symbol.
    ///
    /// In TFF mode, uses `Function::typed` when every sort collapses to
    /// `$i` (matches the existing conservative behaviour).  Otherwise falls
    /// back to an untyped Function.  In FOF mode, always untyped.
    fn ir_fn_for(&mut self, id: SymbolId, name: &str, actual_arity: usize) -> IrFn {
        let base_name = sym_name(name);
        let fn_name = if self.layer.arity(id) == Some(-1) {
            format!("{}__{}", base_name, actual_arity)
        } else {
            base_name
        };

        match self.mode {
            Mode::Fof => IrFn::new(&fn_name, actual_arity as u32),
            Mode::Tff => {
                let arg_kif = self.arg_sorts(id, actual_arity);
                let ret_kif = self.ret_sort(id);
                if arg_kif.iter().all(|s| *s == KifSort::Individual)
                    && ret_kif == KifSort::Individual
                {
                    let ir_args: Vec<IrSort> =
                        arg_kif.iter().map(|_| IrSort::default_sort()).collect();
                    let f = IrFn::typed(&fn_name, &ir_args, IrSort::default_sort());
                    self.ensure_fn(&f);
                    f
                } else {
                    IrFn::new(&fn_name, actual_arity as u32)
                }
            }
        }
    }

    /// Build an IR Predicate for a KIF relation/predicate symbol.
    fn ir_pred_for(&mut self, id: SymbolId, name: &str, actual_arity: usize) -> IrPd {
        let base_name = sym_name(name);
        let pred_name = if self.layer.arity(id) == Some(-1) {
            format!("{}__{}", base_name, actual_arity)
        } else {
            base_name
        };

        match self.mode {
            Mode::Fof => IrPd::new(&pred_name, actual_arity as u32),
            Mode::Tff => {
                let arg_kif = self.arg_sorts(id, actual_arity);
                if arg_kif.iter().all(|s| *s == KifSort::Individual) {
                    let ir_args: Vec<IrSort> =
                        arg_kif.iter().map(|_| IrSort::default_sort()).collect();
                    let p = IrPd::typed(&pred_name, &ir_args);
                    self.ensure_pred(&p);
                    p
                } else {
                    IrPd::new(&pred_name, actual_arity as u32)
                }
            }
        }
    }

    // -- Formula conversion ---------------------------------------------------

    fn sid_to_formula(&mut self, sid: SentenceId) -> Option<IrF> {
        let sentence = &self.store.sentences[self.store.sent_idx(sid)];
        if sentence.is_operator() {
            self.operator_sid_to_formula(sid)
        } else {
            self.atomic_sid_to_formula(sid)
        }
    }

    fn atomic_sid_to_formula(&mut self, sid: SentenceId) -> Option<IrF> {
        let sentence = &self.store.sentences[self.store.sent_idx(sid)];
        let n_args = sentence.elements.len().saturating_sub(1);

        match sentence.elements.first()? {
            Element::Symbol(head_id) => {
                let head_id = *head_id;
                let head_name = self.store.sym_name(head_id).to_owned();
                let elems: Vec<Element> = sentence.elements[1..].to_vec();

                match self.mode {
                    Mode::Tff if self.layer.is_function(head_id) => {
                        // TFF: function-result in formula position -> wrap in s__holds__1.
                        let func = self.ir_fn_for(head_id, &head_name, n_args);
                        let args: Vec<IrT> = elems
                            .iter()
                            .filter_map(|e| self.element_to_term(e))
                            .collect();
                        if args.len() != n_args {
                            return None;
                        }
                        let result = IrT::apply(func, args);
                        let holds = IrPd::new("s__holds__1", 1);
                        Some(IrF::atom(holds, vec![result]))
                    }
                    Mode::Tff => {
                        // TFF: direct typed predicate call.
                        let pred = self.ir_pred_for(head_id, &head_name, n_args);
                        let args: Vec<IrT> = elems
                            .iter()
                            .filter_map(|e| self.element_to_term(e))
                            .collect();
                        if args.len() != n_args {
                            return None;
                        }
                        Some(IrF::atom(pred, args))
                    }
                    Mode::Fof => {
                        // FOF: holds-reification.
                        //   (pred a b) -> s__holds(s__pred__m, a, b)
                        let mention = IrT::constant(IrFn::new(&mention_name(&head_name), 0));
                        let mut args: Vec<IrT> = vec![mention];
                        for e in &elems {
                            args.push(self.element_to_term(e)?);
                        }
                        let pred = IrPd::new("s__holds", (n_args + 1) as u32);
                        Some(IrF::atom(pred, args))
                    }
                }
            }
            Element::Variable { name, .. } => {
                // Higher-order: variable in head position -> holds_app encoding.
                let name = name.clone();
                let var_t = self.var_term(&name);
                let mut args: Vec<IrT> = vec![var_t];
                for elem in &sentence.elements[1..].to_vec() {
                    args.push(self.element_to_term(elem)?);
                }
                let pred = IrPd::new("s__holds_app", (n_args + 1) as u32);
                Some(IrF::atom(pred, args))
            }
            _ => None,
        }
    }

    fn operator_sid_to_formula(&mut self, sid: SentenceId) -> Option<IrF> {
        let sentence = &self.store.sentences[self.store.sent_idx(sid)];
        let op = sentence.op()?.clone();
        let args: Vec<Element> = sentence.elements[1..].to_vec();

        match op {
            OpKind::And => {
                let fs: Vec<IrF> = args.iter().filter_map(|e| self.element_to_formula(e)).collect();
                if fs.is_empty() { None } else { Some(IrF::and(fs)) }
            }
            OpKind::Or => {
                let fs: Vec<IrF> = args.iter().filter_map(|e| self.element_to_formula(e)).collect();
                if fs.is_empty() { None } else { Some(IrF::or(fs)) }
            }
            OpKind::Not => {
                let inner = self.element_to_formula(args.first()?)?;
                Some(IrF::not(inner))
            }
            OpKind::Implies => {
                let a = self.element_to_formula(args.first()?)?;
                let b = self.element_to_formula(args.get(1)?)?;
                Some(IrF::imp(a, b))
            }
            OpKind::Iff => {
                let a = self.element_to_formula(args.first()?)?;
                let b = self.element_to_formula(args.get(1)?)?;
                Some(IrF::iff(a, b))
            }
            OpKind::Equal => {
                let a = self.element_to_term(args.first()?)?;
                let b = self.element_to_term(args.get(1)?)?;
                Some(IrF::eq(a, b))
            }
            OpKind::ForAll => {
                let names = self.extract_quantifier_vars(args.first()?);
                let body = self.element_to_formula(args.get(1)?)?;
                let mut f = body;
                for name in names.iter().rev() {
                    if let Some(&idx) = self.vars.get(name) {
                        // Untyped quantifiers regardless of mode -- matches the
                        // existing converter's conservative behaviour, avoids
                        // sort-mismatch crashes in Vampire's kernel when a
                        // numeric-sort variable appears in a `$i` position.
                        f = IrF::forall(VarId(idx), f);
                    }
                }
                Some(f)
            }
            OpKind::Exists => {
                let names = self.extract_quantifier_vars(args.first()?);
                let body = self.element_to_formula(args.get(1)?)?;
                let mut f = body;
                for name in names.iter().rev() {
                    if let Some(&idx) = self.vars.get(name) {
                        f = IrF::exists(VarId(idx), f);
                    }
                }
                Some(f)
            }
        }
    }

    fn extract_quantifier_vars(&self, elem: &Element) -> Vec<String> {
        match elem {
            Element::Sub(vl_sid) => self.store.sentences[self.store.sent_idx(*vl_sid)]
                .elements
                .iter()
                .filter_map(|e| match e {
                    Element::Variable { name, .. } => Some(name.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    fn element_to_formula(&mut self, elem: &Element) -> Option<IrF> {
        match elem {
            Element::Sub(sid) => self.sid_to_formula(*sid),
            Element::Symbol(id) => {
                // Bare symbol in formula position.
                let name = self.store.sym_name(*id).to_owned();
                match self.mode {
                    Mode::Tff => {
                        let c = IrT::constant(IrFn::new(&sym_name(&name), 0));
                        let holds = IrPd::new("s__holds__1", 1);
                        Some(IrF::atom(holds, vec![c]))
                    }
                    Mode::Fof => {
                        let mention = IrT::constant(IrFn::new(&mention_name(&name), 0));
                        let holds = IrPd::new("s__holds", 1);
                        Some(IrF::atom(holds, vec![mention]))
                    }
                }
            }
            Element::Variable { name, .. } => {
                let var_t = self.var_term(name);
                let holds_name = match self.mode {
                    Mode::Tff => "s__holds__1",
                    Mode::Fof => "s__holds",
                };
                Some(IrF::atom(IrPd::new(holds_name, 1), vec![var_t]))
            }
            _ => None,
        }
    }

    fn element_to_term(&mut self, elem: &Element) -> Option<IrT> {
        match elem {
            Element::Symbol(id) => {
                let id = *id;
                let name = self.store.sym_name(id).to_owned();
                if self.mode == Mode::Tff && self.layer.is_function(id) {
                    // Typed 0-arity function constant.
                    let f = self.ir_fn_for(id, &name, 0);
                    Some(IrT::apply(f, vec![]))
                } else {
                    Some(IrT::constant(IrFn::new(&sym_name(&name), 0)))
                }
            }
            Element::Variable { name, .. } => Some(self.var_term(name)),
            Element::Literal(lit) => Some(self.literal_to_term(lit)),
            Element::Sub(sid) => self.sid_to_term(*sid),
            Element::Op(op) => Some(IrT::constant(IrFn::new(&sym_name(op.name()), 0))),
        }
    }

    fn sid_to_term(&mut self, sid: SentenceId) -> Option<IrT> {
        let sentence = &self.store.sentences[self.store.sent_idx(sid)];
        let n_args = sentence.elements.len().saturating_sub(1);

        if sentence.is_operator() {
            // Operator in term position: opaque symbolic function.
            let op = sentence.op()?.clone();
            let func = IrFn::new(&format!("s__{}_op", op.name()), n_args as u32);
            let args: Vec<IrT> = sentence.elements[1..]
                .iter()
                .filter_map(|e| self.element_to_term(e))
                .collect();
            if args.len() == n_args {
                return Some(IrT::apply(func, args));
            }
            return None;
        }

        match sentence.elements.first()? {
            Element::Symbol(head_id) => {
                let head_id = *head_id;
                let head_name = self.store.sym_name(head_id).to_owned();
                let args: Vec<IrT> = sentence.elements[1..]
                    .iter()
                    .filter_map(|e| self.element_to_term(e))
                    .collect();
                if args.len() != n_args {
                    return None;
                }
                if self.layer.is_function(head_id) {
                    let func = self.ir_fn_for(head_id, &head_name, n_args);
                    Some(IrT::apply(func, args))
                } else {
                    // Relation/predicate in term position: holds_app(mention, args...).
                    let mention = IrT::constant(IrFn::new(&mention_name(&head_name), 0));
                    let mut all_args = vec![mention];
                    all_args.extend(args);
                    let n = all_args.len();
                    let holds_app = IrFn::new(&format!("s__holds_app_{}", n), n as u32);
                    Some(IrT::apply(holds_app, all_args))
                }
            }
            Element::Variable { name, .. } => Some(self.var_term(name)),
            _ => None,
        }
    }

    /// Convert a KIF literal to an IR term.  Numeric literals follow the
    /// `hide_numbers` setting: `true` → `n__<N>` symbolic constants,
    /// `false` → raw TPTP integer / real literals.
    fn literal_to_term(&self, lit: &Literal) -> IrT {
        match lit {
            Literal::Str(s) => {
                let inner = &s[1..s.len() - 1];
                let safe: String = inner
                    .chars()
                    .filter(|c| c.is_alphanumeric() || *c == '_')
                    .take(48)
                    .collect();
                IrT::constant(IrFn::new(&format!("str__{}", safe), 0))
            }
            Literal::Number(n) => {
                if self.hide_numbers {
                    let safe = n.replace('.', "_").replace('-', "neg_");
                    IrT::constant(IrFn::new(&format!("n__{}", safe), 0))
                } else if n.contains('.') {
                    IrT::real(n.clone())
                } else {
                    IrT::int(n.clone())
                }
            }
        }
    }

    fn wrap_free_vars(
        &self,
        formula: IrF,
        bound: &HashSet<String>,
        existential: bool,
    ) -> IrF {
        let mut free: Vec<u32> = self
            .vars
            .iter()
            .filter(|(name, _)| !bound.contains(*name))
            .map(|(_, &idx)| idx)
            .collect();
        free.sort_unstable();

        let mut result = formula;
        for idx in free.into_iter().rev() {
            result = if existential {
                IrF::exists(VarId(idx), result)
            } else {
                IrF::forall(VarId(idx), result)
            };
        }
        result
    }
}


// -- Variable collection (free standing, reused by QueryVarMap builder) ------

fn collect_all_var_ids(
    sid: SentenceId,
    store: &KifStore,
    out: &mut HashMap<String, SymbolId>,
) {
    for elem in &store.sentences[store.sent_idx(sid)].elements {
        match elem {
            Element::Variable { id, name, .. } => {
                out.entry(name.clone()).or_insert(*id);
            }
            Element::Sub(sub) => collect_all_var_ids(*sub, store, out),
            _ => {}
        }
    }
}

fn collect_bound_var_names(
    sid: SentenceId,
    store: &KifStore,
    out: &mut HashSet<String>,
) {
    let sentence = &store.sentences[store.sent_idx(sid)];
    if let Some(op) = sentence.op() {
        if matches!(op, OpKind::ForAll | OpKind::Exists) {
            if let Some(Element::Sub(vl_sid)) = sentence.elements.get(1) {
                for e in &store.sentences[store.sent_idx(*vl_sid)].elements {
                    if let Element::Variable { name, .. } = e {
                        out.insert(name.clone());
                    }
                }
            }
        }
    }
    for elem in &sentence.elements {
        if let Element::Sub(sub) = elem {
            collect_bound_var_names(*sub, store, out);
        }
    }
}
