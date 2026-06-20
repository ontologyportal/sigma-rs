// crates/core/src/trans/ir/ho.rs
//
// HIGHER-ORDER (THF / TH0) IR — the sibling of the first-order `ir` types.
//
// Deliberately a SEPARATE carve-out: nothing here extends `Sort` / `Term` /
// `Formula`, so every existing first-order consumer (emitters, FFI lowering,
// caches) is untouched by construction.  The FO pipeline can always be
// EMBEDDED into this one (a total, mechanical lift); nothing flows back.
//
// One unified expression type: in THF the term/formula distinction collapses
// (a formula is a term at `$o`), so `ThfExpr` mirrors the THF grammar
// 1-to-1 — each variant emits exactly one THF construct, and the emitter is
// a plain structural fold with no rewriting.  Typing discipline is the
// phase-1 bi-sorted scheme: `$i`, `$o`, and arrows over them (SUMO's class
// taxonomy stays as `instance` guards, exactly like the FOF encoding).

use std::collections::HashSet;
use std::fmt::Write as _;

use crate::types::SentenceId;

/// A TH0 sort: `$i`, `$o`, or a (right-associated, curried) arrow.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HoSort {
    /// The individual sort `$i`.
    I,
    /// The Boolean sort `$o`.
    O,
    /// `σ > τ`.
    Fn(Box<HoSort>, Box<HoSort>),
}

impl HoSort {
    /// `args₀ > args₁ > … > ret` (right-associated).
    pub fn curry(args: &[HoSort], ret: HoSort) -> HoSort {
        args.iter()
            .rev()
            .fold(ret, |acc, a| HoSort::Fn(Box::new(a.clone()), Box::new(acc)))
    }

    /// The THF rendering.  Arrows are parenthesized when they appear in
    /// argument position (`($i > $o) > $i`), bare otherwise.
    pub fn thf(&self) -> String {
        match self {
            HoSort::I => "$i".to_string(),
            HoSort::O => "$o".to_string(),
            HoSort::Fn(a, b) => {
                let left = match **a {
                    HoSort::Fn(..) => format!("({})", a.thf()),
                    _ => a.thf(),
                };
                format!("{} > {}", left, b.thf())
            }
        }
    }
}

/// A declared THF constant: `thf(<name>_tp, type, <name>: <sort>).`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThfConst {
    pub name: String,
    pub sort: HoSort,
}

/// The unified THF expression.  Formulas are expressions at `$o`; terms at
/// `$i` (or arrows).  Each variant is one THF construct — emission is 1-to-1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThfExpr {
    /// A named constant (`s__part`, `s__Bill`, `s__part__m`, `n__42`).
    Const(String),
    /// A variable, emitted `X<n>`.
    Var(u32),
    /// Application `f @ x` (curried; spines flatten on emission).
    App(Box<ThfExpr>, Box<ThfExpr>),
    /// Lambda `^[X<n>: σ]: body`.
    Lam(u32, HoSort, Box<ThfExpr>),
    Not(Box<ThfExpr>),
    And(Vec<ThfExpr>),
    Or(Vec<ThfExpr>),
    Imp(Box<ThfExpr>, Box<ThfExpr>),
    Iff(Box<ThfExpr>, Box<ThfExpr>),
    Eq(Box<ThfExpr>, Box<ThfExpr>),
    Forall(u32, HoSort, Box<ThfExpr>),
    Exists(u32, HoSort, Box<ThfExpr>),
    True,
    False,
}

impl ThfExpr {
    /// Apply `head` to `args` left-to-right (curried spine).
    pub fn apply(head: ThfExpr, args: Vec<ThfExpr>) -> ThfExpr {
        args.into_iter()
            .fold(head, |f, a| ThfExpr::App(Box::new(f), Box::new(a)))
    }

    /// Render as THF text.  Conservative parenthesization: every composite
    /// is wrapped, application spines flatten to `(f @ a @ b)`.
    pub fn thf(&self) -> String {
        match self {
            ThfExpr::Const(n) => n.clone(),
            ThfExpr::Var(i) => format!("X{i}"),
            ThfExpr::App(..) => {
                // Flatten the spine for `(f @ a @ b)` readability.
                let mut spine = Vec::new();
                let mut cur = self;
                while let ThfExpr::App(f, a) = cur {
                    spine.push(a);
                    cur = f;
                }
                let mut s = format!("({}", cur.thf());
                for a in spine.iter().rev() {
                    let _ = write!(s, " @ {}", a.thf());
                }
                s.push(')');
                s
            }
            ThfExpr::Lam(v, sort, body) => {
                format!("(^ [X{v}: {}] : {})", sort.thf(), body.thf())
            }
            ThfExpr::Not(e) => format!("(~ {})", e.thf()),
            ThfExpr::And(es) => Self::assoc("&", es),
            ThfExpr::Or(es) => Self::assoc("|", es),
            ThfExpr::Imp(a, b) => format!("({} => {})", a.thf(), b.thf()),
            ThfExpr::Iff(a, b) => format!("({} <=> {})", a.thf(), b.thf()),
            ThfExpr::Eq(a, b) => format!("({} = {})", a.thf(), b.thf()),
            ThfExpr::Forall(v, sort, body) => {
                format!("(! [X{v}: {}] : {})", sort.thf(), body.thf())
            }
            ThfExpr::Exists(v, sort, body) => {
                format!("(? [X{v}: {}] : {})", sort.thf(), body.thf())
            }
            ThfExpr::True => "$true".to_string(),
            ThfExpr::False => "$false".to_string(),
        }
    }

    fn assoc(op: &str, es: &[ThfExpr]) -> String {
        match es {
            [] => "$true".to_string(),
            [one] => one.thf(),
            _ => {
                let mut s = String::from("(");
                for (i, e) in es.iter().enumerate() {
                    if i > 0 {
                        let _ = write!(s, " {op} ");
                    }
                    s.push_str(&e.thf());
                }
                s.push(')');
                s
            }
        }
    }
}

/// A complete THF problem: constant declarations, axioms (paired with their
/// origin sentences via `sid_map`, exactly like the FO `Problem`), and an
/// optional conjecture.
#[derive(Debug, Clone, Default)]
pub struct HoProblem {
    decls: Vec<ThfConst>,
    decl_names: HashSet<String>,
    axioms: Vec<ThfExpr>,
    conjecture: Option<ThfExpr>,
}

impl HoProblem {
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare a constant; deduped by name (first declaration wins).
    pub fn declare(&mut self, c: ThfConst) {
        if self.decl_names.insert(c.name.clone()) {
            self.decls.push(c);
        }
    }

    pub fn with_axiom(&mut self, e: ThfExpr) {
        self.axioms.push(e);
    }

    pub fn conjecture(&mut self, e: ThfExpr) {
        self.conjecture = Some(e);
    }

    pub fn axioms(&self) -> &[ThfExpr] {
        &self.axioms
    }

    pub fn conjecture_ref(&self) -> Option<&ThfExpr> {
        self.conjecture.as_ref()
    }

    pub fn decls(&self) -> &[ThfConst] {
        &self.decls
    }

    /// Assemble the full THF text.  `sid_map[i]` names `axioms[i]` as
    /// `kb_<sid>` (repeats suffixed `_v<n>`, mirroring the FO assembler);
    /// unmapped axioms are `ax_<i>`.  The conjecture is named
    /// `conjecture_name`.
    pub fn to_thf(&self, sid_map: &[SentenceId], conjecture_name: &str) -> String {
        let mut out = String::new();
        for d in &self.decls {
            let _ = writeln!(out, "thf({}_tp, type, {}: {}).", d.name, d.name, d.sort.thf());
        }
        let mut seen: std::collections::HashMap<SentenceId, u32> = std::collections::HashMap::new();
        for (i, ax) in self.axioms.iter().enumerate() {
            let name = match sid_map.get(i) {
                Some(&sid) => {
                    let n = *seen.entry(sid).and_modify(|n| *n += 1).or_insert(0u32);
                    if n == 0 {
                        format!("kb_{sid}")
                    } else {
                        format!("kb_{sid}_v{n}")
                    }
                }
                None => format!("ax_{i}"),
            };
            let _ = writeln!(out, "thf({name}, axiom, {}).", ax.thf());
        }
        if let Some(c) = &self.conjecture {
            let _ = writeln!(out, "thf({conjecture_name}, conjecture, {}).", c.thf());
        }
        out
    }
}

// -- Tests ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_render_with_argument_parens() {
        let s = HoSort::curry(&[HoSort::I, HoSort::O], HoSort::O);
        assert_eq!(s.thf(), "$i > $o > $o");
        let k = HoSort::Fn(
            Box::new(HoSort::Fn(Box::new(HoSort::I), Box::new(HoSort::O))),
            Box::new(HoSort::I),
        );
        assert_eq!(k.thf(), "($i > $o) > $i");
    }

    #[test]
    fn application_spines_flatten() {
        let e = ThfExpr::apply(
            ThfExpr::Const("s__knows".into()),
            vec![ThfExpr::Const("s__Bill".into()), ThfExpr::Var(0)],
        );
        assert_eq!(e.thf(), "(s__knows @ s__Bill @ X0)");
    }

    #[test]
    fn lambda_and_quantifiers() {
        let body = ThfExpr::apply(
            ThfExpr::Const("s__p".into()),
            vec![ThfExpr::Var(1)],
        );
        let lam = ThfExpr::Lam(1, HoSort::I, Box::new(body.clone()));
        assert_eq!(lam.thf(), "(^ [X1: $i] : (s__p @ X1))");
        let q = ThfExpr::Exists(2, HoSort::O, Box::new(ThfExpr::Var(2)));
        assert_eq!(q.thf(), "(? [X2: $o] : X2)");
    }

    #[test]
    fn connectives_and_problem_assembly() {
        let mut p = HoProblem::new();
        p.declare(ThfConst { name: "s__q".into(), sort: HoSort::O });
        p.declare(ThfConst { name: "s__q".into(), sort: HoSort::I }); // deduped
        p.with_axiom(ThfExpr::And(vec![
            ThfExpr::Const("s__q".into()),
            ThfExpr::Not(Box::new(ThfExpr::False)),
        ]));
        p.conjecture(ThfExpr::Const("s__q".into()));
        let text = p.to_thf(&[SentenceId::from(7u64)], "query_0");
        assert!(text.contains("thf(s__q_tp, type, s__q: $o)."), "{text}");
        assert_eq!(text.matches("_tp, type,").count(), 1, "dedup failed: {text}");
        assert!(text.contains("thf(kb_7, axiom, (s__q & (~ $false)))."), "{text}");
        assert!(text.contains("thf(query_0, conjecture, s__q)."), "{text}");
    }
}
