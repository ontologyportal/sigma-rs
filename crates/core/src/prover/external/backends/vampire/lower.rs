// crates/core/src/prover/lower.rs
//
// Lower the pure-Rust `trans::ir` representation into the FFI types used
// by the linked Vampire C++ library.
//
// The lowering is mechanical: each IR node is rebuilt as the corresponding
// FFI node in a single walk of the tree.  The FFI constructors take the
// global lock (`synced`) per call and perform symbol interning on the
// Vampire side, so repeated references to the same IR symbol resolve to
// the same C++ signature entry.
//
// A problem lowered here is independent of the IR it came from — the
// returned `SysProblem` can be mutated (extra axioms appended, options
// changed) before calling `solve_and_prove`.
//
// Gated on `integrated-prover` because the FFI types only exist when the
// embedded Vampire backend is compiled in.

#![cfg(feature = "integrated-prover")]

use vampire_prover::ffi::{
    Formula   as SysFormula,
    Function  as SysFunction,
    Interp    as SysInterp,
    Predicate as SysPredicate,
    Problem   as SysProblem,
    Sort      as SysSort,
    Term      as SysTerm,
};
use vampire_prover::Options;

use crate::trans::ir::{self, LogicMode};

/// Rebuild a [`crate::trans::ir::Problem`] as an FFI [`SysProblem`] ready for solving.
///
/// `opts` is set on the returned problem.
pub(crate) fn lower_problem(p: &ir::Problem, opts: Options) -> SysProblem {
    let mode_tff = matches!(p.mode(), LogicMode::Tff);
    let mut out = if mode_tff {
        SysProblem::new_tff(opts)
    } else {
        SysProblem::new(opts)
    };

    for s in p.sort_decls() {
        out.declare_sort(lower_sort(s));
    }
    for f in p.fn_decls() {
        out.declare_function(lower_function(f));
    }
    for pd in p.pred_decls() {
        out.declare_predicate(lower_predicate(pd));
    }

    for ax in p.axioms() {
        out.with_axiom(lower_formula(ax));
    }
    if let Some(c) = p.conjecture_ref() {
        out.conjecture(lower_formula(c));
    }

    out
}

fn lower_sort(s: &ir::Sort) -> SysSort {
    match s.tptp_name() {
        "$i"    => SysSort::default_sort(),
        "$int"  => SysSort::int(),
        "$real" => SysSort::real(),
        "$rat"  => SysSort::rational(),
        name    => SysSort::new(name),
    }
}

fn lower_function(f: &ir::Function) -> SysFunction {
    if let Some(i) = f.interp() {
        return SysFunction::interpreted(f.name(), lower_interp(i));
    }
    if f.is_typed() {
        let arg_sorts: Vec<SysSort> = f.arg_sorts().iter().map(lower_sort).collect();
        let ret_sort = f
            .ret_sort()
            .map(lower_sort)
            .expect("typed ir::Function missing return sort");
        return SysFunction::typed(f.name(), &arg_sorts, ret_sort);
    }
    SysFunction::new(f.name(), f.arity())
}

fn lower_predicate(p: &ir::Predicate) -> SysPredicate {
    if let Some(i) = p.interp() {
        return SysPredicate::interpreted(p.name(), lower_interp(i));
    }
    if p.is_typed() {
        let arg_sorts: Vec<SysSort> = p.arg_sorts().iter().map(lower_sort).collect();
        return SysPredicate::typed(p.name(), &arg_sorts);
    }
    SysPredicate::new(p.name(), p.arity())
}

fn lower_interp(i: ir::Interp) -> SysInterp {
    match i {
        ir::Interp::Equal            => SysInterp::Equal,
        ir::Interp::IntGreater       => SysInterp::IntGreater,
        ir::Interp::IntGreaterEqual  => SysInterp::IntGreaterEqual,
        ir::Interp::IntLess          => SysInterp::IntLess,
        ir::Interp::IntLessEqual     => SysInterp::IntLessEqual,
        ir::Interp::IntDivides       => SysInterp::IntDivides,
        ir::Interp::IntSuccessor     => SysInterp::IntSuccessor,
        ir::Interp::IntUnaryMinus    => SysInterp::IntUnaryMinus,
        ir::Interp::IntPlus          => SysInterp::IntPlus,
        ir::Interp::IntMinus         => SysInterp::IntMinus,
        ir::Interp::IntMultiply      => SysInterp::IntMultiply,
        ir::Interp::IntAbs           => SysInterp::IntAbs,
        ir::Interp::RatGreater       => SysInterp::RatGreater,
        ir::Interp::RatGreaterEqual  => SysInterp::RatGreaterEqual,
        ir::Interp::RatLess          => SysInterp::RatLess,
        ir::Interp::RatLessEqual     => SysInterp::RatLessEqual,
        ir::Interp::RatPlus          => SysInterp::RatPlus,
        ir::Interp::RatMinus         => SysInterp::RatMinus,
        ir::Interp::RatMultiply      => SysInterp::RatMultiply,
        ir::Interp::RatQuotient      => SysInterp::RatQuotient,
        ir::Interp::RealGreater      => SysInterp::RealGreater,
        ir::Interp::RealGreaterEqual => SysInterp::RealGreaterEqual,
        ir::Interp::RealLess         => SysInterp::RealLess,
        ir::Interp::RealLessEqual    => SysInterp::RealLessEqual,
        ir::Interp::RealPlus         => SysInterp::RealPlus,
        ir::Interp::RealMinus        => SysInterp::RealMinus,
        ir::Interp::RealMultiply     => SysInterp::RealMultiply,
        ir::Interp::RealQuotient     => SysInterp::RealQuotient,
        ir::Interp::IntQuotientE  => SysInterp::IntQuotientE,
        ir::Interp::IntRemainderT => SysInterp::IntRemainderT,
        ir::Interp::IntFloor      => SysInterp::IntFloor,
        ir::Interp::IntCeiling    => SysInterp::IntCeiling,
        ir::Interp::IntTruncate   => SysInterp::IntTruncate,
        ir::Interp::IntRound      => SysInterp::IntRound,
        ir::Interp::RatFloor      => SysInterp::RatFloor,
        ir::Interp::RatCeiling    => SysInterp::RatCeiling,
        ir::Interp::RatTruncate   => SysInterp::RatTruncate,
        ir::Interp::RatRound      => SysInterp::RatRound,
        ir::Interp::RealFloor     => SysInterp::RealFloor,
        ir::Interp::RealCeiling   => SysInterp::RealCeiling,
        ir::Interp::RealTruncate  => SysInterp::RealTruncate,
        ir::Interp::RealRound     => SysInterp::RealRound,
        ir::Interp::IntToInt      => SysInterp::IntToInt,
        ir::Interp::IntToRat      => SysInterp::IntToRat,
        ir::Interp::IntToReal     => SysInterp::IntToReal,
        ir::Interp::RatToInt      => SysInterp::RatToInt,
        ir::Interp::RatToRat      => SysInterp::RatToRat,
        ir::Interp::RatToReal     => SysInterp::RatToReal,
        ir::Interp::RealToInt     => SysInterp::RealToInt,
        ir::Interp::RealToRat     => SysInterp::RealToRat,
        ir::Interp::RealToReal    => SysInterp::RealToReal,
    }
}

fn lower_term(t: &ir::Term) -> SysTerm {
    match t {
        ir::Term::Var(v)       => SysTerm::new_var(v.index()),
        ir::Term::Int(s)       => SysTerm::int(s),
        ir::Term::Real(s)      => SysTerm::real(s),
        ir::Term::Rational(s)  => SysTerm::rational(s),
        ir::Term::Apply(func, args) => {
            let ffi_func = lower_function(func);
            if args.is_empty() {
                ffi_func.with(())
            } else {
                let lowered: Vec<SysTerm> = args.iter().map(lower_term).collect();
                ffi_func.with(lowered.as_slice())
            }
        }
    }
}

fn lower_formula(f: &ir::Formula) -> SysFormula {
    match f {
        ir::Formula::True  => SysFormula::new_true(),
        ir::Formula::False => SysFormula::new_false(),

        ir::Formula::Atom { pred, args } => {
            let ffi_pred = lower_predicate(pred);
            let lowered: Vec<SysTerm> = args.iter().map(lower_term).collect();
            ffi_pred.with(lowered.as_slice())
        }

        ir::Formula::Eq(lhs, rhs) => {
            SysFormula::new_eq(lower_term(lhs), lower_term(rhs))
        }
        ir::Formula::EqTyped { lhs, rhs, sort } => {
            SysFormula::new_eq_typed(lower_term(lhs), lower_term(rhs), lower_sort(sort))
        }

        ir::Formula::Not(inner) => SysFormula::new_not(lower_formula(inner)),

        ir::Formula::And(parts) => match parts.len() {
            0 => SysFormula::new_true(),
            1 => lower_formula(&parts[0]),
            _ => {
                let lowered: Vec<SysFormula> = parts.iter().map(lower_formula).collect();
                SysFormula::new_and(&lowered)
            }
        },
        ir::Formula::Or(parts) => match parts.len() {
            0 => SysFormula::new_false(),
            1 => lower_formula(&parts[0]),
            _ => {
                let lowered: Vec<SysFormula> = parts.iter().map(lower_formula).collect();
                SysFormula::new_or(&lowered)
            }
        },

        ir::Formula::Imp(a, b) => lower_formula(a).imp(lower_formula(b)),
        ir::Formula::Iff(a, b) => lower_formula(a).iff(lower_formula(b)),

        ir::Formula::Forall(v, body) => SysFormula::new_forall(v.index(), lower_formula(body)),
        ir::Formula::Exists(v, body) => SysFormula::new_exists(v.index(), lower_formula(body)),

        ir::Formula::ForallTyped(v, sort, body) => {
            SysFormula::new_forall_typed(v.index(), lower_sort(sort), lower_formula(body))
        }
        ir::Formula::ExistsTyped(v, sort, body) => {
            SysFormula::new_exists_typed(v.index(), lower_sort(sort), lower_formula(body))
        }
    }
}
