// crates/core/src/prover/external/backends/vampire/lower_ho.rs
//
// [`HoProblem`] (THF IR) -> FFI [`SysProblem`] lowering — the higher-order
// sibling of `lower.rs`: native Kernel data structures, no TPTP text
// round-trip.  Bi-sorted THF maps onto four FFI primitives:
//
//   * arrow sorts        — `SysSort::arrow` (compound-sort registry);
//   * HOL constants      — 0-ary typed functions AT their arrow sort;
//   * application        — `Term::hol_app` (curried `f @ x`);
//   * the `$o` boundary  — `Formula::from_bool_term` (a Boolean term in
//     formula position) and `Formula::as_bool_term` (FOOL: a compound
//     formula in a `$o` argument position).
//
// Lambdas never reach here: the THF lowering lambda-lifts `KappaFn` into
// defined predicates upstream (`trans/lower_thf.rs`), so a `Lam` is an
// upstream invariant violation and errors out rather than lowering.

use std::collections::HashMap;

use vampire_prover::{Options, SysFormula, SysFunction, SysProblem, SysSort, SysTerm};

use crate::trans::ir::{HoProblem, HoSort, ThfExpr};

/// Lower a THF problem into the FFI solver's native structures.
pub(crate) fn lower_ho_problem(p: &HoProblem, opts: Options) -> Result<SysProblem, String> {
    let mut out = SysProblem::new_tff(opts);

    // Declared constant name -> its HoSort (every constant the THF lowering
    // emits carries a declaration, so this is total for well-formed input).
    let decl_sorts: HashMap<&str, &HoSort> = p
        .decls()
        .iter()
        .map(|d| (d.name.as_str(), &d.sort))
        .collect();
    let mut cx = LowerCx {
        decl_sorts,
        sorts: HashMap::new(),
        consts: HashMap::new(),
        var_sorts: HashMap::new(),
    };

    for ax in p.axioms() {
        let f = cx.formula(ax)?;
        out.with_axiom(f);
    }
    if let Some(c) = p.conjecture_ref() {
        let f = cx.formula(c)?;
        out.conjecture(f);
    }
    Ok(out)
}

fn restore_var(map: &mut HashMap<u32, HoSort>, v: u32, saved: Option<HoSort>) {
    match saved {
        Some(s) => {
            map.insert(v, s);
        }
        None => {
            map.remove(&v);
        }
    }
}

struct LowerCx<'a> {
    decl_sorts: HashMap<&'a str, &'a HoSort>,
    /// HoSort -> registered FFI sort (arrow registration is per-problem
    /// idempotent but not free; memoise).
    sorts: HashMap<HoSort, SysSort>,
    /// Constant name -> its registered 0-ary typed function.
    consts: HashMap<String, SysFunction>,
    /// In-scope binder sorts (variable index -> HoSort), maintained with
    /// save/restore around quantifier bodies: a VARIABLE-headed application
    /// (`P @ X`) needs its binder's arrow sort passed explicitly.
    var_sorts: HashMap<u32, HoSort>,
}

impl<'a> LowerCx<'a> {
    fn sort(&mut self, s: &HoSort) -> SysSort {
        if let Some(hit) = self.sorts.get(s) {
            return hit.clone();
        }
        let lowered = match s {
            HoSort::I => SysSort::default_sort(),
            HoSort::O => SysSort::bool_sort(),
            HoSort::Fn(a, b) => {
                let fa = self.sort(a);
                let fb = self.sort(b);
                SysSort::arrow(&fa, &fb)
            }
        };
        self.sorts.insert(s.clone(), lowered.clone());
        lowered
    }

    fn constant(&mut self, name: &str) -> Result<SysTerm, String> {
        if let Some(f) = self.consts.get(name) {
            return Ok(f.with(()));
        }
        let sort = self
            .decl_sorts
            .get(name)
            .map(|s| (*s).clone())
            .ok_or_else(|| format!("undeclared THF constant `{name}`"))?;
        let ffi_sort = self.sort(&sort);
        let f = SysFunction::typed(name, &[], ffi_sort);
        let t = f.with(());
        self.consts.insert(name.to_string(), f);
        Ok(t)
    }

    /// An expression in FORMULA position.
    fn formula(&mut self, e: &ThfExpr) -> Result<SysFormula, String> {
        Ok(match e {
            ThfExpr::True => SysFormula::new_true(),
            ThfExpr::False => SysFormula::new_false(),
            ThfExpr::Not(a) => SysFormula::new_not(self.formula(a)?),
            ThfExpr::And(es) => match es.len() {
                0 => SysFormula::new_true(),
                1 => self.formula(&es[0])?,
                _ => {
                    let parts = es
                        .iter()
                        .map(|x| self.formula(x))
                        .collect::<Result<Vec<_>, _>>()?;
                    SysFormula::new_and(&parts)
                }
            },
            ThfExpr::Or(es) => match es.len() {
                0 => SysFormula::new_false(),
                1 => self.formula(&es[0])?,
                _ => {
                    let parts = es
                        .iter()
                        .map(|x| self.formula(x))
                        .collect::<Result<Vec<_>, _>>()?;
                    SysFormula::new_or(&parts)
                }
            },
            ThfExpr::Imp(a, b) => self.formula(a)?.imp(self.formula(b)?),
            ThfExpr::Iff(a, b) => self.formula(a)?.iff(self.formula(b)?),
            // THF equalities from our lowering are between `$i` terms.
            ThfExpr::Eq(a, b) => SysFormula::new_eq_typed(
                self.term(a)?,
                self.term(b)?,
                SysSort::default_sort(),
            ),
            ThfExpr::Forall(v, sort, body) => {
                let s = self.sort(sort);
                let saved = self.var_sorts.insert(*v, sort.clone());
                let lowered = self.formula(body)?;
                restore_var(&mut self.var_sorts, *v, saved);
                SysFormula::new_forall_typed(*v, s, lowered)
            }
            ThfExpr::Exists(v, sort, body) => {
                let s = self.sort(sort);
                let saved = self.var_sorts.insert(*v, sort.clone());
                let lowered = self.formula(body)?;
                restore_var(&mut self.var_sorts, *v, saved);
                SysFormula::new_exists_typed(*v, s, lowered)
            }
            // A `$o` atom: an application spine, a Boolean variable, or a
            // Boolean constant — a term wrapped into formula position.
            ThfExpr::App(..) | ThfExpr::Const(_) | ThfExpr::Var(_) => {
                let t = self.term(e)?;
                SysFormula::from_bool_term(&t)
            }
            ThfExpr::Lam(..) => {
                return Err("lambda reached the native HOL lowering \
                            (KappaFn is lambda-lifted upstream)"
                    .to_string())
            }
        })
    }

    /// An expression in TERM position.
    fn term(&mut self, e: &ThfExpr) -> Result<SysTerm, String> {
        Ok(match e {
            ThfExpr::Const(name) => self.constant(name)?,
            ThfExpr::Var(i) => SysTerm::new_var(*i),
            ThfExpr::App(f, a) => {
                let arg = self.term(a)?;
                // A VARIABLE head has no inferable sort: pass its binder's
                // arrow sort explicitly.  Proper-term heads (constants,
                // nested applications) infer their own.
                if let ThfExpr::Var(i) = &**f {
                    let hs = self
                        .var_sorts
                        .get(i)
                        .cloned()
                        .ok_or_else(|| format!("unbound variable X{i} in head position"))?;
                    let ffi_hs = self.sort(&hs);
                    let head = SysTerm::new_var(*i);
                    head.hol_app_sorted(&ffi_hs, &arg)
                } else {
                    let head = self.term(f)?;
                    head.hol_app(&arg)
                }
            }
            ThfExpr::Lam(..) => {
                return Err("lambda reached the native HOL lowering \
                            (KappaFn is lambda-lifted upstream)"
                    .to_string())
            }
            // FOOL: a compound formula in a `$o` argument position
            // (e.g. `knows @ a @ (p & q)`).
            other => {
                let f = self.formula(other)?;
                f.as_bool_term()
            }
        })
    }
}
