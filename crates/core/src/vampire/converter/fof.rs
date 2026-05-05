// crates/core/src/vampire/converter/fof.rs
//
// FOF-specific impl methods on NativeConverter.
//
// FOF mode uses *holds-reification*: every n-ary predicate atom
//
//     (P a b)  ->  s__holds(s__P__m, a, b)
//
// where `s__P__m` is the predicate name in "mention" form (a constant)
// and `s__holds` is a single (n+1)-ary predicate symbol.  Functions and
// individuals appear as ordinary symbols `s__name` (no `__m`) when in
// argument position; predicates used as terms reify through
// `s__holds_app_<n>(...)` (defined in `common::sid_to_term`).
//
// FOF emits no type / sort / function / predicate declarations: the IR
// constructs are all untyped.  Hence none of the `ensure_*` registration
// helpers from `tff.rs` are reachable from this module.
//
// Naming: every method here is prefixed `fof_*` so the dispatcher in
// `common.rs` can route to it explicitly.

use vampire_prover::ir::{Formula as IrF, Function as IrFn, Predicate as IrPd, Term as IrT, VarId};

use super::common::{mention_name, NativeConverter};
use crate::types::{Element, SymbolId};

impl<'a> NativeConverter<'a> {
    // -- atomic_sid_to_formula path ------------------------------------------

    /// FOF holds-reified predicate atom:
    ///   `(pred a b)`  ->  `s__holds(s__pred__m, a, b)`
    ///
    /// `head_id` and `_head_id` are accepted for symmetry with the TFF
    /// signature; FOF does not consult any signature data.
    pub(super) fn fof_atomic_predicate(
        &mut self,
        _head_id: SymbolId,
        head_name: &str,
        elems:    &[Element],
        n_args:   usize,
    ) -> Option<IrF> {
        let mention = IrT::constant(IrFn::new(&mention_name(head_name), 0));
        let mut args: Vec<IrT> = vec![mention];
        for e in elems {
            args.push(self.element_to_term(e)?);
        }
        let pred = IrPd::new("s__holds", (n_args + 1) as u32);
        Some(IrF::atom(pred, args))
    }

    // -- element_to_formula path ---------------------------------------------

    /// FOF: a bare symbol in formula position becomes
    ///   `s__holds(s__name__m)` — a 1-ary holds atom over the mention.
    pub(super) fn fof_symbol_to_formula(&mut self, name: &str) -> IrF {
        let mention = IrT::constant(IrFn::new(&mention_name(name), 0));
        let holds = IrPd::new("s__holds", 1);
        IrF::atom(holds, vec![mention])
    }

    /// FOF: a bare variable in formula position becomes
    ///   `s__holds(?V)` — a 1-ary holds atom over the variable term.
    pub(super) fn fof_variable_to_formula(&mut self, var_t: IrT) -> IrF {
        let holds = IrPd::new("s__holds", 1);
        IrF::atom(holds, vec![var_t])
    }

    // -- ir_fn_for / ir_pred_for -- FOF arms ---------------------------------

    /// FOF Function: always untyped.
    pub(super) fn fof_ir_fn(&self, fn_name: &str, actual_arity: usize) -> IrFn {
        IrFn::new(fn_name, actual_arity as u32)
    }

    /// FOF Predicate: always untyped.
    pub(super) fn fof_ir_pred(&self, pred_name: &str, actual_arity: usize) -> IrPd {
        IrPd::new(pred_name, actual_arity as u32)
    }

    // -- Quantifier wrapping (untyped) ---------------------------------------

    /// FOF universal: `![X<idx>] : body` — variables in FOF carry no
    /// sort, so this is just an untyped wrap.
    pub(super) fn fof_wrap_universal(&self, idx: u32, body: IrF) -> IrF {
        IrF::forall(VarId(idx), body)
    }

    /// FOF existential: `?[X<idx>] : body`.
    pub(super) fn fof_wrap_existential(&self, idx: u32, body: IrF) -> IrF {
        IrF::exists(VarId(idx), body)
    }
}
