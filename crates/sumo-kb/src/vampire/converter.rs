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

/// FNV-1a 32-bit hash.  Used to disambiguate string literals whose
/// non-ASCII characters collapse to `_` during TPTP-identifier
/// sanitisation — keeps the emitted constants unique per source string
/// without pulling in a hashing dependency.
fn fnv1a_32(s: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for b in s.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h
}

/// Alphanumeric TPTP name for a KIF operator, used when the operator
/// appears in *term position* (reified into `s__<name>_op`).  The KIF
/// surface names `=>` and `<=>` contain characters that TPTP reserves
/// for connectives, so we can't reuse `OpKind::name()` here.  All
/// other names are alphanumeric and passed through unchanged.
fn op_tptp_safe_name(op: &OpKind) -> &'static str {
    match op {
        OpKind::And     => "and",
        OpKind::Or      => "or",
        OpKind::Not     => "not",
        OpKind::Implies => "imp",
        OpKind::Iff     => "iff",
        OpKind::Equal   => "equal",
        OpKind::ForAll  => "forall",
        OpKind::Exists  => "exists",
    }
}

fn sym_name(name: &str) -> String {
    format!("{}{}", S, name.replace('.', "_").replace('-', "_"))
}

fn mention_name(name: &str) -> String {
    format!("{}{}{}", S, name.replace('.', "_").replace('-', "_"), M)
}

/// Map recorded for each converted conjecture so that downstream binding
/// extraction can rejoin Vampire's `X<n>` variable names with the original
/// KIF names.
///
/// Only consumed by `vampire/bindings.rs` (gated on `integrated-prover`).
/// Allow dead_code on the fields for `--no-default-features --features ask`
/// builds where the binding extractor isn't compiled.
#[derive(Debug, Default, Clone)]
pub struct QueryVarMap {
    /// Variable index -> KIF variable name.
    #[allow(dead_code)]
    pub idx_to_kif: HashMap<u32, String>,
    /// Free-variable indices in sorted order.
    #[allow(dead_code)]
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

    /// Scope stack for α-renaming bound variables inside *reified*
    /// quantifiers (`s__exists_op`, `s__forall_op`).  Each entry is a
    /// fresh per-reification mapping from KIF name to TPTP index.
    /// `var_term` checks this stack top-down before falling back to
    /// the sentence-wide `vars` map.
    ///
    /// Without this, an axiom that mentions `?W` in *both* a reified
    /// quantifier (term position) *and* a real quantifier (formula
    /// position) of the same name — e.g.
    ///     `(and (desires ?A (exists (?W) ...))
    ///           (not (exists (?W) ...)))`
    /// — collapses all `?W` occurrences onto a single TPTP index.
    /// The real `?[X]: …` then binds it locally, `wrap_free_vars` skips
    /// it at the top, and Vampire rejects the free occurrence inside
    /// `s__exists_op(X, …)` with "unquantified variable detected".
    reif_scopes: Vec<HashMap<String, u32>>,

    /// Fresh TPTP indices minted for reified-quantifier bound variables.
    /// Tracked so `wrap_free_vars` can add a top-level universal for
    /// each one (they're free in the FOL output: the reified
    /// `s__exists_op(X, …)` is just a ground function term, not a real
    /// binder).  Cleared on every `reset_sentence_state`.
    reif_free: Vec<u32>,

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
            reif_scopes: Vec::new(),
            reif_free: Vec::new(),
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
        self.reif_scopes.clear();
        self.reif_free.clear();
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
        // Walk the reified-quantifier scope stack inside-out first —
        // an entry on the stack shadows the global `vars` map for the
        // duration of a `s__exists_op(…)` / `s__forall_op(…)` body.
        for scope in self.reif_scopes.iter().rev() {
            if let Some(&idx) = scope.get(name) {
                return IrT::var(idx);
            }
        }
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
            Element::Symbol { id: head_id, .. } => {
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
            Element::Sub { sid: vl_sid, .. } => self.store.sentences[self.store.sent_idx(*vl_sid)]
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
            Element::Sub { sid, .. } => self.sid_to_formula(*sid),
            Element::Symbol { id, .. } => {
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
            Element::Symbol { id, .. } => {
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
            Element::Literal { lit, .. } => Some(self.literal_to_term(lit)),
            Element::Sub { sid, .. } => self.sid_to_term(*sid),
            Element::Op { op, .. } => Some(IrT::constant(IrFn::new(&sym_name(op.name()), 0))),
        }
    }

    fn sid_to_term(&mut self, sid: SentenceId) -> Option<IrT> {
        let sentence = &self.store.sentences[self.store.sent_idx(sid)];
        let n_args = sentence.elements.len().saturating_sub(1);

        if sentence.is_operator() {
            // Operator in term position: opaque symbolic function.  We
            // must use the TPTP-safe *alphanumeric* operator name here,
            // not the KIF surface name — `OpKind::name()` returns
            // `"=>"` for Implies and `"<=>"` for Iff, and those chars
            // are reserved connectives in TPTP.  Embedding them into
            // a symbol like `s__=>_op(...)` makes Vampire's parser
            // split around `=>` and report the resulting term as
            // "Non-boolean term X<n> of sort $i used in a formula
            // context".  Map each `OpKind` to a safe-by-construction
            // identifier (`imp`, `iff`, etc.).  The others are already
            // alphanumeric, but routing them through the same helper
            // keeps the encoding in one place.
            let op = sentence.op()?.clone();
            let safe_name = op_tptp_safe_name(&op);
            let func = IrFn::new(&format!("s__{}_op", safe_name), n_args as u32);

            // Special case: reified `(exists (?V ...) body)` or
            // `(forall (?V ...) body)`.  The bound variables *in the
            // reified body* must get fresh TPTP indices to avoid
            // shadow-collisions with other occurrences of the same
            // KIF name elsewhere in the axiom.  Allocate a scope
            // frame, translate both the var-list arg and the body
            // under it, then pop.
            if matches!(op, OpKind::Exists | OpKind::ForAll) {
                let bound_names = sentence.elements.get(1)
                    .map(|e| self.extract_quantifier_vars(e))
                    .unwrap_or_default();
                let mut frame: HashMap<String, u32> = HashMap::with_capacity(bound_names.len());
                for name in &bound_names {
                    let idx = self.next_var;
                    self.next_var += 1;
                    frame.insert(name.clone(), idx);
                    // These fresh indices are free in the FOL output
                    // (the reified `s__exists_op(…)` is just a function
                    // term, not a binder).  Remember them so
                    // `wrap_free_vars` adds a top-level universal.
                    self.reif_free.push(idx);
                }
                self.reif_scopes.push(frame);
                let args: Vec<IrT> = sentence.elements[1..]
                    .iter()
                    .filter_map(|e| self.element_to_term(e))
                    .collect();
                self.reif_scopes.pop();
                if args.len() == n_args {
                    return Some(IrT::apply(func, args));
                }
                return None;
            }

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
            Element::Symbol { id: head_id, .. } => {
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
                // TPTP identifiers must be ASCII: `char::is_alphanumeric`
                // returns `true` for Unicode letters like `花` or `é`, which
                // Vampire rejects with "parse error: Bad character".
                //
                // Restrict to ASCII alphanumerics + underscore and substitute
                // everything else with `_`.  To keep distinct source strings
                // mapped to distinct constants (e.g. `2n是1的簡稱` vs.
                // `2n是1的简称` — traditional vs. simplified Chinese — which
                // would otherwise collide to the same `str__2n_1_`), append
                // an 8-hex-digit FNV-1a hash of the full original inner
                // string whenever any character had to be sanitised.
                let inner = &s[1..s.len() - 1];
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
                    format!("str__{}_{:08x}", safe, fnv1a_32(inner))
                } else {
                    format!("str__{}", safe)
                };
                IrT::constant(IrFn::new(&name, 0))
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
        // Reified-quantifier bound variables got fresh indices that
        // live outside `self.vars`.  They are semantically free in
        // the FOL output — `s__exists_op(X, …)` doesn't bind X — so
        // add a top-level universal for each.
        free.extend(self.reif_free.iter().copied());
        free.sort_unstable();
        free.dedup();

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
            Element::Sub { sid: sub, .. } => collect_all_var_ids(*sub, store, out),
            _ => {}
        }
    }
}

/// Collect the names of KIF variables that are *genuinely* bound by a
/// FOL quantifier in the translated formula.
///
/// A `(forall ...)` / `(exists ...)` only acts as a real binder when it
/// sits in **formula position**.  When it appears nested inside a
/// non-logical relation — e.g. `(hasPurpose ?X (exists (?Y) ...))` —
/// `sid_to_term` reifies it as a ground function term
/// `s__exists_op(?Y, ...)`.  The variables inside that reified term
/// remain free in the surrounding FOL sentence, so they must still
/// receive the top-level universal added by `wrap_free_vars`.
///
/// The previous implementation walked every sub-sentence indiscriminately
/// and added its quantifier variables to `out`, causing
/// `wrap_free_vars` to skip them and emitting formulas like
/// `![X1]: (... => hasPurpose(X1, s__exists_op(X3, p(X3))))` — Vampire
/// rejects these with "unquantified variable detected" because X3 is
/// free at the top level of the FOF clause.
fn collect_bound_var_names(
    sid: SentenceId,
    store: &KifStore,
    out: &mut HashSet<String>,
) {
    collect_bound_var_names_at(sid, store, /*in_formula_pos=*/ true, out);
}

fn collect_bound_var_names_at(
    sid: SentenceId,
    store: &KifStore,
    in_formula_pos: bool,
    out: &mut HashSet<String>,
) {
    let sentence = &store.sentences[store.sent_idx(sid)];

    if in_formula_pos {
        if let Some(op) = sentence.op() {
            if matches!(op, OpKind::ForAll | OpKind::Exists) {
                if let Some(Element::Sub { sid: vl_sid, .. }) = sentence.elements.get(1) {
                    for e in &store.sentences[store.sent_idx(*vl_sid)].elements {
                        if let Element::Variable { name, .. } = e {
                            out.insert(name.clone());
                        }
                    }
                }
            }
        }
    }

    // Dispatch sub-sentence positions by the current operator/head. The
    // rules mirror what `sid_to_formula` vs `sid_to_term` actually do
    // when they recurse, so `bound` stays in lockstep with where real
    // FOL binders end up in the IR output.
    let op = sentence.op();
    let sub_in_formula_pos = match op {
        // Logical connectives keep their children in formula position.
        Some(OpKind::And) | Some(OpKind::Or) | Some(OpKind::Not)
        | Some(OpKind::Implies) | Some(OpKind::Iff) => in_formula_pos,

        // Quantifiers are formula-level when we're at a formula site;
        // their body (and the var-list sub, which collect_all_var_ids
        // already indexes) inherit that position.
        Some(OpKind::ForAll) | Some(OpKind::Exists) => in_formula_pos,

        // `Equal` emits `IrF::eq(term, term)` — both sides are terms.
        Some(OpKind::Equal) => false,

        // Non-operator heads / atomic predicate applications:
        // `atomic_sid_to_formula` processes their args as terms
        // (`element_to_term`).  Inside a term context, everything
        // nested stays a term.
        None => false,
    };

    for elem in &sentence.elements {
        if let Element::Sub { sid: sub, .. } = elem {
            collect_bound_var_names_at(*sub, store, sub_in_formula_pos, out);
        }
    }
}
