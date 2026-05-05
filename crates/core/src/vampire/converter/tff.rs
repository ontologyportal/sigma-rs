// crates/core/src/vampire/converter/tff.rs
//
// TFF-specific impl methods on NativeConverter.
//
// TFF mode uses *direct typed-predicate encoding*:
//
//     (instance A Entity)  ->  s__instance(A, Entity)
//
// with a one-time TFF declaration
//   `tff(pred_s__instance_2, type, s__instance: ($i * $i) > $o).`
//
// Functions in argument position use typed `IrT::apply(IrFn::typed(...), ..)`
// when every sort collapses to `$i` (the conservative path that doesn't
// risk Vampire kernel sort mismatches), and fall back to untyped
// `IrFn::new(...)` when any sort is non-Individual.  Function results in
// formula position are wrapped with the helper predicate `s__holds__1`.
//
// TFF mode is also the only mode that emits sort / function / predicate
// declarations — the `ensure_sort` / `ensure_fn` / `ensure_pred` registry
// helpers live here because FOF never invokes them.
//
// This file is the natural home for the upcoming changes:
//   §A: sort-typed quantifiers (use `IrF::forall_typed` instead of
//       `IrF::forall` in `operator_sid_to_formula`'s ForAll/Exists arms;
//       the dispatch will move from `common::operator_sid_to_formula`
//       into a `tff_quantifier` helper here).
//   §B: native-arithmetic mapping for SUMO predicates / functions
//       (return `Predicate::interpreted(...)` / `Function::interpreted(...)`
//       from `tff_ir_pred` / `tff_ir_fn` when the head matches a known
//       arithmetic name).
//   §C: sort-suffixed function names (rewrite `fn_name` / `pred_name` in
//       `tff_ir_fn` / `tff_ir_pred` using a sort-suffix helper).

use vampire_prover::ir::{Formula as IrF, Function as IrFn, Predicate as IrPd, Sort as IrSort, Term as IrT, VarId};

use super::common::{sym_name, NativeConverter};
use super::sort::Sort as KifSort;
use crate::types::{Element, SymbolId};

impl<'a> NativeConverter<'a> {
    // -- atomic_sid_to_formula paths -----------------------------------------

    /// TFF: function-result in formula position.
    ///
    /// Wraps the function application in the unary helper predicate
    /// `s__holds__1`, e.g.
    ///
    ///   `(SuccessorFn ?N)`  ->  `s__holds__1(s__SuccessorFn(?N))`
    ///
    /// (compare the FOF path which would emit
    /// `s__holds(s__SuccessorFn__m, ?N)`).
    pub(super) fn tff_atomic_function(
        &mut self,
        head_id:   SymbolId,
        head_name: &str,
        elems:     &[Element],
        n_args:    usize,
    ) -> Option<IrF> {
        let func = self.ir_fn_for(head_id, head_name, n_args);
        let args: Vec<IrT> = elems.iter().filter_map(|e| self.element_to_term(e)).collect();
        if args.len() != n_args {
            return None;
        }
        let result = IrT::apply(func, args);
        let holds = IrPd::new("s__holds__1", 1);
        Some(IrF::atom(holds, vec![result]))
    }

    /// TFF: direct typed predicate call.
    ///
    ///   `(P a b)`  ->  `s__P(a, b)`
    ///
    /// with a one-time TFF type declaration registered via `ensure_pred`
    /// (inside `ir_pred_for` -> `tff_ir_pred`).
    pub(super) fn tff_atomic_predicate(
        &mut self,
        head_id:   SymbolId,
        head_name: &str,
        elems:     &[Element],
        n_args:    usize,
    ) -> Option<IrF> {
        let pred = self.ir_pred_for(head_id, head_name, n_args);
        let args: Vec<IrT> = elems.iter().filter_map(|e| self.element_to_term(e)).collect();
        if args.len() != n_args {
            return None;
        }
        Some(IrF::atom(pred, args))
    }

    // -- element_to_formula path ---------------------------------------------

    /// TFF: a bare symbol in formula position becomes
    ///   `s__holds__1(s__name)` — the helper predicate over the symbol.
    pub(super) fn tff_symbol_to_formula(&mut self, name: &str) -> IrF {
        let c = IrT::constant(IrFn::new(&sym_name(name), 0));
        let holds = IrPd::new("s__holds__1", 1);
        IrF::atom(holds, vec![c])
    }

    /// TFF: a bare variable in formula position becomes
    ///   `s__holds__1(?V)`.
    pub(super) fn tff_variable_to_formula(&mut self, var_t: IrT) -> IrF {
        let holds = IrPd::new("s__holds__1", 1);
        IrF::atom(holds, vec![var_t])
    }

    // -- element_to_term path ------------------------------------------------

    /// TFF: a 0-arity function symbol becomes a typed function constant.
    /// Only the TFF mode distinguishes a "function constant" from a
    /// "regular individual" here; FOF returns the bare-name form via the
    /// fallthrough in `common::element_to_term`.
    pub(super) fn tff_function_constant(&mut self, id: SymbolId, name: &str) -> IrT {
        let f = self.ir_fn_for(id, name, 0);
        IrT::apply(f, vec![])
    }

    // -- ir_fn_for / ir_pred_for -- TFF arms ---------------------------------

    /// TFF Function: typed when all argument and return sorts are
    /// `Individual`, untyped otherwise (conservative — see comment on
    /// `operator_sid_to_formula`'s quantifier arm).  Typed functions
    /// register a TFF declaration via `ensure_fn`.
    pub(super) fn tff_ir_fn(&mut self, id: SymbolId, fn_name: &str, actual_arity: usize) -> IrFn {
        let arg_kif = self.arg_sorts(id, actual_arity);
        let ret_kif = self.ret_sort(id);
        if arg_kif.iter().all(|s| *s == KifSort::Individual)
            && ret_kif == KifSort::Individual
        {
            let ir_args: Vec<IrSort> =
                arg_kif.iter().map(|_| IrSort::default_sort()).collect();
            let f = IrFn::typed(fn_name, &ir_args, IrSort::default_sort());
            self.ensure_fn(&f);
            f
        } else {
            IrFn::new(fn_name, actual_arity as u32)
        }
    }

    /// TFF Predicate: typed when all argument sorts are `Individual`,
    /// untyped otherwise.  Typed predicates register a TFF declaration
    /// via `ensure_pred`.
    pub(super) fn tff_ir_pred(
        &mut self,
        id:           SymbolId,
        pred_name:    &str,
        actual_arity: usize,
    ) -> IrPd {
        let arg_kif = self.arg_sorts(id, actual_arity);
        if arg_kif.iter().all(|s| *s == KifSort::Individual) {
            let ir_args: Vec<IrSort> =
                arg_kif.iter().map(|_| IrSort::default_sort()).collect();
            let p = IrPd::typed(pred_name, &ir_args);
            self.ensure_pred(&p);
            p
        } else {
            IrPd::new(pred_name, actual_arity as u32)
        }
    }

    // -- Quantifier wrapping (typed) -----------------------------------------

    /// TFF universal: `![X<idx>:$i] : body`.
    ///
    /// We currently emit `$i` for every variable.  Per-variable sort
    /// inference (so an integer-typed variable gets `:$int`, etc.) is
    /// the next chunk of TODO.md §A — once that lands, this method
    /// learns to consult the per-variable sort assignment instead of
    /// always defaulting.  Until then, `:$i` is sound (every TFF type
    /// is a subtype of `$i` in our encoding) and matches Vampire's
    /// expectations for untyped variables in TFF mode.
    pub(super) fn tff_wrap_universal(&self, idx: u32, body: IrF) -> IrF {
        IrF::forall_typed(VarId(idx), IrSort::default_sort(), body)
    }

    /// TFF existential: `?[X<idx>:$i] : body`.  See `tff_wrap_universal`.
    pub(super) fn tff_wrap_existential(&self, idx: u32, body: IrF) -> IrF {
        IrF::exists_typed(VarId(idx), IrSort::default_sort(), body)
    }

    // -- Declaration registration (TFF-only) ---------------------------------
    //
    // FOF emits no declarations.  These helpers live here so the surface
    // area of declaration registration is contained in one place — when
    // §C lands and starts emitting sort-suffixed names, the registration
    // calls pick up the new names automatically.

    /// Register a sort declaration on the Problem if it hasn't been seen.
    pub(super) fn ensure_sort(&mut self, sort: &IrSort) {
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
    pub(super) fn ensure_fn(&mut self, f: &IrFn) {
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
    pub(super) fn ensure_pred(&mut self, p: &IrPd) {
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
}
