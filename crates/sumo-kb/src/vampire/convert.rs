// vampire/convert.rs
//
// TFF (Typed First-order Form) native converter for the embedded Vampire prover.
//
// Converts KIF SentenceIds directly to typed vampire_prover::Formula objects,
// bypassing the TPTP string round-trip.  Uses SemanticLayer's precomputed sort
// data (VarTypeInference, SortAnnotations) to annotate every variable and
// symbol with the correct TFF sort.
//
// Key encoding differences from the FOF embedded.rs converter:
//  - Relations/predicates in formula position: direct typed predicate call
//    `s__Pred(arg1, arg2)` instead of holds-encoding `s__holds(s__Pred__m, arg1, arg2)`.
//  - Functions in term position: `Function::typed("s__Fn", &arg_sorts, ret_sort)`.
//  - Quantifiers: `Formula::new_forall_typed(var, sort, body)` when the variable
//    has a non-Individual sort from VarTypeInference.
//  - Individual constants: `Function::typed("s__Const", &[], sort)`.
//  - Numeric literals: encoded as symbolic constants `n__N` (no native TFF numeric
//    literal API in vampire-prover; arithmetic fallback to symbolic form).
//
// Gated: requires `integrated-prover` feature (implies `vampire`).

use std::collections::{HashMap, HashSet};

use vampire_prover::{Formula, Function, Predicate, Sort as VSort, Term};

use crate::kif_store::KifStore;
use crate::parse::ast::OpKind;
use crate::semantic::{SemanticLayer, Sort};
use crate::types::{Element, Literal, SentenceId, SymbolId};

// -- Symbol name helpers -------------------------------------------------------

const S: &str = "s__";
const M: &str = "__m";

fn sym_tff(name: &str) -> String {
    format!("{}{}", S, name.replace('.', "_").replace('-', "_"))
}

fn mention_tff(name: &str) -> String {
    format!("{}{}{}", S, name.replace('.', "_").replace('-', "_"), M)
}

// -- Variable collection -------------------------------------------------------

/// Collect all variable (name → scoped SymbolId) pairs from the sentence tree.
/// If the same name appears with different scoped IDs (unusual but possible),
/// the first occurrence wins (which is fine within a single sentence).
pub(crate) fn collect_all_var_ids(
    sid:   SentenceId,
    store: &KifStore,
    out:   &mut HashMap<String, SymbolId>,
) {
    for elem in &store.sentences[store.sent_idx(sid)].elements {
        match elem {
            Element::Variable { id, name, .. } => { out.entry(name.clone()).or_insert(*id); }
            Element::Sub(sub) => collect_all_var_ids(*sub, store, out),
            _ => {}
        }
    }
}

/// Collect names of variables explicitly bound by forall/exists in the tree.
pub(crate) fn collect_bound_var_names(
    sid:   SentenceId,
    store: &KifStore,
    out:   &mut HashSet<String>,
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

/// Allocate vampire variable indices for a sentence; return:
///   (vars, var_ids, next_base)
/// where `vars`    = name → vampire var index
///       `var_ids` = name → scoped SymbolId  (for VTI sort lookup)
///       `next_base` = the next free index after this allocation
pub(crate) fn alloc_vars_tff(
    sid:   SentenceId,
    store: &KifStore,
    base:  u32,
) -> (HashMap<String, u32>, HashMap<String, SymbolId>, u32) {
    let mut var_ids: HashMap<String, SymbolId> = HashMap::new();
    collect_all_var_ids(sid, store, &mut var_ids);
    let mut next = base;
    let vars: HashMap<String, u32> = var_ids
        .keys()
        .map(|name| {
            let idx = next;
            next += 1;
            (name.clone(), idx)
        })
        .collect();
    (vars, var_ids, next)
}

// -- TffConverter --------------------------------------------------------------

/// Converts KIF sentences to typed vampire_prover::Formula values.
///
/// The TFF encoding uses direct predicate/function calls without the
/// holds-reification encoding of the FOF path.  All symbols are declared as
/// typed via `Function::typed` / `Predicate::typed` (idempotent in the global
/// vampire registry).
pub(crate) struct TffConverter<'a> {
    store:   &'a KifStore,
    layer:   &'a SemanticLayer,
    /// name → vampire variable index
    vars:    &'a HashMap<String, u32>,
    /// name → scoped SymbolId (for VarTypeInference sort lookup)
    var_ids: &'a HashMap<String, SymbolId>,
}

impl<'a> TffConverter<'a> {
    pub(crate) fn new(
        store:   &'a KifStore,
        layer:   &'a SemanticLayer,
        vars:    &'a HashMap<String, u32>,
        var_ids: &'a HashMap<String, SymbolId>,
    ) -> Self {
        Self { store, layer, vars, var_ids }
    }

    // -- Variable term helper --------------------------------------------------

    /// Build a vampire variable term for the named variable.
    fn var_term(&self, name: &str) -> Term {
        let idx = self.vars.get(name).copied().unwrap_or_else(|| {
            log::warn!(target: "sumo_kb::tff_converter",
                "TFF: unknown variable '{}' -- defaulting to index 0", name);
            0
        });
        Term::new_var(idx)
    }

    /// Return the declared arg sorts for a symbol (clamped to `n_args`).
    fn arg_sorts(&self, id: SymbolId, n_args: usize) -> Vec<Sort> {
        let sa = self.layer.sort_annotations();
        let sa = sa.as_ref().unwrap();
        let base = sa.symbol_arg_sorts.get(&id).cloned().unwrap_or_default();
        let last = base.last().copied().unwrap_or(Sort::Individual);
        (0..n_args).map(|i| base.get(i).copied().unwrap_or(last)).collect()
    }

    /// Return the declared return sort of a function.
    fn ret_sort(&self, id: SymbolId) -> Sort {
        let sa = self.layer.sort_annotations();
        sa.as_ref()
            .and_then(|sa| sa.symbol_return_sorts.get(&id).copied())
            .unwrap_or(Sort::Individual)
    }

    /// Build a `Function` for a SUMO function symbol.
    ///
    /// Uses typed declaration only when all arg sorts are `$i` (Individual) to
    /// avoid sort-conflict crashes in Vampire's kernel when variable terms (which
    /// default to `$i` with untyped quantifiers) are passed to non-`$i` positions.
    fn typed_fn(&self, id: SymbolId, name: &str, actual_arity: usize) -> Function {
        let tff_name = sym_tff(name);
        let fn_name = if self.layer.arity(id) == Some(-1) {
            format!("{}__{}", tff_name, actual_arity)
        } else {
            tff_name
        };
        let arg_sorts = self.arg_sorts(id, actual_arity);
        let ret = self.ret_sort(id);
        if arg_sorts.iter().all(|s| *s == Sort::Individual) && ret == Sort::Individual {
            let vsorts: Vec<VSort> = arg_sorts.iter().map(|_| VSort::default_sort()).collect();
            Function::typed(&fn_name, &vsorts, VSort::default_sort())
        } else {
            Function::new(&fn_name, actual_arity as u32)
        }
    }

    /// Build a `Predicate` for a SUMO relation/predicate.
    ///
    /// Uses typed declaration only when all arg sorts are `$i` (Individual).
    fn typed_pred(&self, id: SymbolId, name: &str, actual_arity: usize) -> Predicate {
        let tff_name = sym_tff(name);
        let pred_name = if self.layer.arity(id) == Some(-1) {
            format!("{}__{}", tff_name, actual_arity)
        } else {
            tff_name
        };
        let arg_sorts = self.arg_sorts(id, actual_arity);
        if arg_sorts.iter().all(|s| *s == Sort::Individual) {
            let vsorts: Vec<VSort> = arg_sorts.iter().map(|_| VSort::default_sort()).collect();
            Predicate::typed(&pred_name, &vsorts)
        } else {
            Predicate::new(&pred_name, actual_arity as u32)
        }
    }

    // -- Top-level formula builder ---------------------------------------------

    pub(crate) fn sid_to_formula(&mut self, sid: SentenceId) -> Option<Formula> {
        let sentence = &self.store.sentences[self.store.sent_idx(sid)];
        if sentence.is_operator() {
            return self.operator_sid_to_formula(sid);
        }
        self.predicate_sid_to_formula(sid)
    }

    // -- Predicate sentence → Formula ------------------------------------------

    fn predicate_sid_to_formula(&mut self, sid: SentenceId) -> Option<Formula> {
        let sentence = &self.store.sentences[self.store.sent_idx(sid)];
        let n_args = sentence.elements.len().saturating_sub(1);

        match sentence.elements.first()? {
            Element::Symbol(head_id) => {
                let head_id = *head_id;
                let head_name = self.store.sym_name(head_id).to_owned();
                use std::io::Write;

                if self.layer.is_function(head_id) {
                    // Function result in formula position: wrap in s__holds__1
                    let func = self.typed_fn(head_id, &head_name, n_args);
                    let args: Vec<Term> = sentence.elements[1..]
                        .iter()
                        .filter_map(|e| self.element_to_term(e))
                        .collect();
                    if args.len() != n_args { return None; }
                    let result = func.with(args.as_slice());
                    let holds = Predicate::new("s__holds__1", 1);
                    Some(holds.with(result))
                } else {
                    // Relation or predicate: direct predicate call (no holds-encoding)
                    let pred = self.typed_pred(head_id, &head_name, n_args);
                    let args: Vec<Term> = sentence.elements[1..]
                        .iter()
                        .filter_map(|e| self.element_to_term(e))
                        .collect();
                    if args.is_empty() && n_args == 0 {
                        Some(pred.with(()))
                    } else if args.len() == n_args {
                        Some(pred.with(args.as_slice()))
                    } else {
                        None
                    }
                }
            }
            Element::Variable { name, .. } => {
                // Variable in head position: holds_app encoding (same as FOF)
                let name = name.clone();
                let var_t = self.var_term(&name);
                let mut args: Vec<Term> = vec![var_t];
                for elem in sentence.elements[1..].iter() {
                    if let Some(t) = self.element_to_term(elem) {
                        args.push(t);
                    }
                }
                let pred = Predicate::new("s__holds_app", (n_args + 1) as u32);
                Some(pred.with(args.as_slice()))
            }
            _ => None,
        }
    }

    // -- Operator sentence → Formula -------------------------------------------

    fn operator_sid_to_formula(&mut self, sid: SentenceId) -> Option<Formula> {
        let sentence = &self.store.sentences[self.store.sent_idx(sid)];
        let op = sentence.op()?.clone();
        let args: Vec<Element> = sentence.elements[1..].to_vec();

        match op {
            OpKind::And => {
                let formulas: Vec<Formula> = args.iter()
                    .filter_map(|e| self.element_to_formula(e))
                    .collect();
                match formulas.len() {
                    0 => None,
                    1 => Some(formulas.into_iter().next().unwrap()),
                    _ => Some(Formula::new_and(&formulas)),
                }
            }
            OpKind::Or => {
                let formulas: Vec<Formula> = args.iter()
                    .filter_map(|e| self.element_to_formula(e))
                    .collect();
                match formulas.len() {
                    0 => None,
                    1 => Some(formulas.into_iter().next().unwrap()),
                    _ => Some(Formula::new_or(&formulas)),
                }
            }
            OpKind::Not => {
                let inner = self.element_to_formula(args.first()?)?;
                Some(Formula::new_not(inner))
            }
            OpKind::Implies => {
                let a = self.element_to_formula(args.first()?)?;
                let b = self.element_to_formula(args.get(1)?)?;
                Some(a >> b)
            }
            OpKind::Iff => {
                let a = self.element_to_formula(args.first()?)?;
                let b = self.element_to_formula(args.get(1)?)?;
                Some(a.iff(b))
            }
            OpKind::Equal => {
                let a = self.element_to_term(args.first()?)?;
                let b = self.element_to_term(args.get(1)?)?;
                Some(a.eq(b))
            }
            OpKind::ForAll => {
                let var_names = self.extract_var_names(args.first()?);
                let body = self.element_to_formula(args.get(1)?)?;
                let mut formula = body;
                for name in var_names.iter().rev() {
                    if let Some(&idx) = self.vars.get(name) {
                        // Use untyped quantifiers to avoid sort-mismatch crashes in
                        // Vampire's C++ kernel when a variable with a numeric sort
                        // appears in a $i-typed predicate position.  Typed predicates
                        // and functions still provide TFF benefits.
                        formula = Formula::new_forall(idx, formula);
                    }
                }
                Some(formula)
            }
            OpKind::Exists => {
                let var_names = self.extract_var_names(args.first()?);
                let body = self.element_to_formula(args.get(1)?)?;
                let mut formula = body;
                for name in var_names.iter().rev() {
                    if let Some(&idx) = self.vars.get(name) {
                        formula = Formula::new_exists(idx, formula);
                    }
                }
                Some(formula)
            }
        }
    }

    fn extract_var_names(&self, elem: &Element) -> Vec<String> {
        match elem {
            Element::Sub(vl_sid) => self.store.sentences[self.store.sent_idx(*vl_sid)]
                .elements
                .iter()
                .filter_map(|e| {
                    if let Element::Variable { name, .. } = e { Some(name.clone()) }
                    else { None }
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    // -- Element conversions ---------------------------------------------------

    pub(crate) fn element_to_formula(&mut self, elem: &Element) -> Option<Formula> {
        match elem {
            Element::Sub(sid) => self.sid_to_formula(*sid),
            Element::Symbol(id) => {
                // Bare symbol in formula position: wrap in s__holds__1
                let name = self.store.sym_name(*id).to_owned();
                let sym_t = sym_const_tff(&name, self.layer, *id);
                let holds = Predicate::new("s__holds__1", 1);
                Some(holds.with(sym_t))
            }
            Element::Variable { name, .. } => {
                let var_t = self.var_term(name);
                let holds = Predicate::new("s__holds__1", 1);
                Some(holds.with(var_t))
            }
            _ => None,
        }
    }

    pub(crate) fn element_to_term(&mut self, elem: &Element) -> Option<Term> {
        match elem {
            Element::Symbol(id) => {
                let name = self.store.sym_name(*id).to_owned();
                if self.layer.is_function(*id) {
                    // 0-arity typed function constant
                    Some(self.typed_fn(*id, &name, 0).with(()))
                } else {
                    Some(sym_const_tff(&name, self.layer, *id))
                }
            }
            Element::Variable { name, .. } => Some(self.var_term(name)),
            Element::Literal(lit) => Some(literal_to_term(lit)),
            Element::Sub(sid) => self.sid_to_term(*sid),
            Element::Op(op) => Some(sym_const_basic(op.name())),
        }
    }

    fn sid_to_term(&mut self, sid: SentenceId) -> Option<Term> {
        let sentence = &self.store.sentences[self.store.sent_idx(sid)];
        let n_args = sentence.elements.len().saturating_sub(1);

        if sentence.is_operator() {
            // Operator in term position: encode as opaque symbolic function
            let op = sentence.op()?.clone();
            let func = Function::new(&format!("s__{}_op", op.name()), n_args as u32);
            let args: Vec<Term> = sentence.elements[1..]
                .iter()
                .filter_map(|e| self.element_to_term(e))
                .collect();
            if args.len() == n_args {
                return Some(func.with(args.as_slice()));
            }
            return None;
        }

        match sentence.elements.first()? {
            Element::Symbol(head_id) => {
                let head_id = *head_id;
                let head_name = self.store.sym_name(head_id).to_owned();
                let args: Vec<Term> = sentence.elements[1..]
                    .iter()
                    .filter_map(|e| self.element_to_term(e))
                    .collect();
                if args.len() != n_args { return None; }

                if self.layer.is_function(head_id) {
                    let func = self.typed_fn(head_id, &head_name, n_args);
                    Some(func.with(args.as_slice()))
                } else {
                    // Relation/predicate in term position:
                    // use holds_app(mention_const, args...) → $i-sorted result
                    let mention = mention_const_tff(&head_name);
                    let mut all_args = vec![mention];
                    all_args.extend(args);
                    let n = all_args.len();
                    let holds_app = Function::new(&format!("s__holds_app_{}", n), n as u32);
                    Some(holds_app.with(all_args.as_slice()))
                }
            }
            Element::Variable { name, .. } => Some(self.var_term(name)),
            _ => None,
        }
    }
}

// -- Free-variable wrapping ----------------------------------------------------

/// Wrap a formula with quantifiers for its free variables (top-level axiom/conjecture).
/// Free variables = all variables minus those bound by explicit forall/exists.
/// Uses untyped quantifiers to avoid sort-mismatch crashes in Vampire's kernel.
pub(crate) fn wrap_free_vars_tff(
    formula:     Formula,
    vars:        &HashMap<String, u32>,
    _var_ids:    &HashMap<String, SymbolId>,
    bound:       &HashSet<String>,
    _layer:      &SemanticLayer,
    existential: bool,   // true for conjectures (wrap in exists)
) -> Formula {
    let mut free: Vec<u32> = vars
        .iter()
        .filter(|(name, _)| !bound.contains(*name))
        .map(|(_, &idx)| idx)
        .collect();
    free.sort_unstable();

    let mut result = formula;
    for idx in free.into_iter().rev() {
        result = if existential {
            Formula::new_exists(idx, result)
        } else {
            Formula::new_forall(idx, result)
        };
    }
    result
}

// -- Static helper functions ---------------------------------------------------

/// Build a constant term for a symbol.
fn sym_const_tff(name: &str, _layer: &SemanticLayer, _id: SymbolId) -> Term {
    Function::constant(&sym_tff(name))
}

/// Build a constant term for an operator name.
fn sym_const_basic(name: &str) -> Term {
    Function::constant(&sym_tff(name))
}

/// Mention constant for a predicate used in term position.
fn mention_const_tff(name: &str) -> Term {
    let m = mention_tff(name);
    Function::constant(&m)
}

/// Convert a literal to a term.  Numeric literals are symbolically encoded as
/// `n__N` constants (vampire-prover has no native numeric literal API).
fn literal_to_term(lit: &Literal) -> Term {
    match lit {
        Literal::Str(s) => {
            let inner = &s[1..s.len() - 1];
            let safe: String = inner
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .take(48)
                .collect();
            Function::constant(&format!("str__{}", safe))
        }
        Literal::Number(n) => {
            // Symbolic encoding: n__42, n__neg_1, n__3_14
            let safe = n.replace('.', "_").replace('-', "neg_");
            Function::constant(&format!("n__{}", safe))
        }
    }
}
