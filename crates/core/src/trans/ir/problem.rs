//! A TPTP problem: declarations plus axioms plus an optional conjecture.

use super::formula::Formula;
use super::symbol::{Function, Predicate, Sort};

/// TPTP logic dialect used when this problem is serialised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[derive(serde::Serialize, serde::Deserialize)]
pub enum LogicMode {
    /// First-order form (`fof(...)`).
    Fof,
    /// Typed first-order form (`tff(...)`).
    Tff,
}

impl Default for LogicMode {
    fn default() -> Self { LogicMode::Fof }
}

/// A pure-Rust TPTP problem.
///
/// [`Problem::with_axiom`] and [`Problem::conjecture`] append their arguments;
/// callers are responsible for the order and uniqueness they want to see in
/// the output. Declarations (sorts, functions, predicates) are emitted in
/// insertion order before the axioms.
///
/// # Examples
///
/// ```
/// use crate::trans::ir::{Formula, Function, Predicate, Problem, Term};
///
/// let p        = Predicate::new("P", 1);
/// let socrates = Term::constant(Function::new("socrates", 0));
///
/// let mut problem = Problem::new();
/// problem.with_axiom(Formula::atom(p.clone(), vec![socrates.clone()]));
/// problem.conjecture(Formula::atom(p,         vec![socrates]));
///
/// let tptp = problem.to_tptp();
/// assert!(tptp.contains("fof(axiom_0, axiom, P(socrates))."));
/// assert!(tptp.contains("fof(conjecture, conjecture, P(socrates))."));
/// ```
#[derive(Debug, Clone, Default)]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Problem {
    mode: LogicMode,
    sort_decls: Vec<Sort>,
    fn_decls:   Vec<Function>,
    pred_decls: Vec<Predicate>,
    axioms:     Vec<Formula>,
    conjecture: Option<Formula>,
}

impl Problem {
    /// Creates a new FOF problem.
    pub fn new() -> Self {
        Self { mode: LogicMode::Fof, ..Self::default() }
    }

    /// Creates a new TFF problem. Type declarations added via
    /// [`Problem::declare_sort`], [`Problem::declare_function`], and
    /// [`Problem::declare_predicate`] are emitted before axioms.
    pub fn new_tff() -> Self {
        Self { mode: LogicMode::Tff, ..Self::default() }
    }

    /// The logic mode this problem is emitted in.
    pub fn mode(&self) -> LogicMode {
        self.mode
    }

    /// Appends an axiom.
    pub fn with_axiom(&mut self, f: Formula) -> &mut Self {
        self.axioms.push(f);
        self
    }

    /// Sets (or overwrites) the conjecture.
    pub fn conjecture(&mut self, f: Formula) -> &mut Self {
        self.conjecture = Some(f);
        self
    }

    /// Records a sort declaration to emit in the TPTP preamble.
    pub fn declare_sort(&mut self, s: Sort) -> &mut Self {
        self.sort_decls.push(s);
        self
    }

    /// Records a typed function declaration to emit in the TPTP preamble.
    pub fn declare_function(&mut self, f: Function) -> &mut Self {
        self.fn_decls.push(f);
        self
    }

    /// Records a typed predicate declaration to emit in the TPTP preamble.
    pub fn declare_predicate(&mut self, p: Predicate) -> &mut Self {
        self.pred_decls.push(p);
        self
    }

    /// Read-only access to the axiom list.
    pub fn axioms(&self) -> &[Formula] {
        &self.axioms
    }

    /// Read-only access to the conjecture, if one has been set.
    pub fn conjecture_ref(&self) -> Option<&Formula> {
        self.conjecture.as_ref()
    }

    /// Read-only access to the registered sort declarations.
    pub fn sort_decls(&self) -> &[Sort] {
        &self.sort_decls
    }

    /// Read-only access to the registered typed-function declarations.
    pub fn fn_decls(&self) -> &[Function] {
        &self.fn_decls
    }

    /// Read-only access to the registered typed-predicate declarations.
    pub fn pred_decls(&self) -> &[Predicate] {
        &self.pred_decls
    }

    /// Serialises the problem to TPTP. Uses `tff(...)` in TFF mode, `fof(...)`
    /// otherwise. Type declarations for sorts and typed
    /// functions/predicates are emitted first, each on its own line, in
    /// insertion order.
    ///
    /// Axioms are emitted as `<kw>(axiom_<i>, axiom, <body>).` with the
    /// default naming; for custom axiom identifiers or leading comments
    /// (e.g. the original KIF sentence), iterate [`Problem::axioms`]
    /// yourself and call [`Formula::to_tptp`] per entry.
    ///
    /// # Examples
    ///
    /// ```
    /// use crate::trans::ir::{Formula, Function, Predicate, Problem, Sort, Term};
    ///
    /// let mut problem = Problem::new_tff();
    /// let person = Sort::new("person");
    /// let alice  = Function::typed("alice",  &[],                    person.clone());
    /// let mortal = Predicate::typed("mortal", &[person.clone()]);
    ///
    /// problem.declare_sort(person);
    /// problem.declare_function(alice.clone());
    /// problem.declare_predicate(mortal.clone());
    /// problem.with_axiom(Formula::atom(mortal, vec![Term::apply(alice, vec![])]));
    ///
    /// let t = problem.to_tptp();
    /// assert!(t.contains("tff(person_type, type, person: $tType)."));
    /// assert!(t.contains("tff(fn_alice, type, alice: person)."));
    /// assert!(t.contains("tff(pred_mortal_1, type, mortal: person > $o)."));
    /// assert!(t.contains("tff(axiom_0, axiom, mortal(alice))."));
    /// ```
    #[allow(dead_code)]
    pub fn to_tptp(&self) -> String {
        let kw = match self.mode {
            LogicMode::Tff => "tff",
            LogicMode::Fof => "fof",
        };
        let mut out = String::new();

        for s in &self.sort_decls {
            if let Some(d) = s.tptp_decl() {
                out.push_str(&d);
                out.push('\n');
            }
        }
        for f in &self.fn_decls {
            if let Some(d) = f.tptp_decl() {
                out.push_str(&d);
                out.push('\n');
            }
        }
        for p in &self.pred_decls {
            if let Some(d) = p.tptp_decl() {
                out.push_str(&d);
                out.push('\n');
            }
        }
        for (i, ax) in self.axioms.iter().enumerate() {
            out.push_str(&format!("{kw}(axiom_{i}, axiom, {}).\n", ax.to_tptp()));
        }
        if let Some(c) = &self.conjecture {
            out.push_str(&format!("{kw}(conjecture, conjecture, {}).\n", c.to_tptp()));
        }
        out
    }
}
