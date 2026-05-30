// crates/core/src/saturate/clausify.rs
//
// Native clausifier: stored SUO-KIF sentence -> list of canonical clauses.
// A faithful port of the prototype pipeline (residue_prover.py §2):
//
//     lift -> elim(=> <=>) -> NNF -> implicit ∀-closure -> skolemize -> CNF
//
// operating on the self-contained [`Form`]/[`Term`] trees instead of
// Python tuples.  Differences from the Vampire-backed `cnf` module are
// deliberate (plan D5): no FFI, no sort annotations, and **deterministic
// skolems** — names are `sk_<root_sid_hex>_<n>` with `n` a per-root
// counter, so re-clausifying the same root yields byte-identical clauses
// (required for cache eviction + re-generation to be invisible).

use std::collections::BTreeSet;
use std::collections::HashMap;

use crate::parse::OpKind;
use crate::syntactic::SyntacticLayer;
use crate::types::{Element, Sentence, SentenceId, Symbol, SymbolId};

use super::canon::canonical_clause;
use super::clause::{AtomTable, PClause, Term};

/// Distribution guard: a single formula may not explode into more than
/// this many clauses (matches the prototype's MAX_CLAUSES guard — big
/// `<=>` nests are dropped rather than allowed to take the KB hostage).
pub(crate) const MAX_CLAUSES_PER_FORMULA: usize = 128;

/// A clause longer than this is dropped — such clauses are practically
/// unusable for resolution and only bloat the passive queue.
pub(crate) const MAX_LITS_PER_CLAUSE: usize = 32;

/// Lifted first-order formula.  Connectives mirror [`OpKind`]; quantifier
/// variables are scope-qualified symbol ids exactly as stored.
#[derive(Debug, Clone)]
pub(crate) enum Form {
    Atom(Term),
    Not(Box<Form>),
    And(Vec<Form>),
    Or(Vec<Form>),
    Implies(Box<Form>, Box<Form>),
    Iff(Box<Form>, Box<Form>),
    ForAll(Vec<SymbolId>, Box<Form>),
    Exists(Vec<SymbolId>, Box<Form>),
}

// -- Lift: stored Sentence -> Form/Term ---------------------------------------

/// Lift a stored sentence into a [`Form`], resolving nested sub-sentences
/// through the store.  Returns `None` for shapes the clausifier cannot
/// handle (malformed quantifiers, unresolvable subterms) — the caller
/// skips the formula rather than guessing.
pub(crate) fn lift_form(syn: &SyntacticLayer, atoms: &AtomTable, sent: &Sentence) -> Option<Form> {
    let op = match sent.elements.first() {
        Some(Element::Op(op)) if *op != OpKind::Equal => op.clone(),
        // Symbol- or variable-headed (incl. predicate variables) and
        // equality sentences are atoms.
        _ => return Some(Form::Atom(lift_term_of(syn, atoms, sent)?)),
    };
    let mut args = sent.elements.iter().skip(1);
    match op {
        OpKind::And | OpKind::Or => {
            let mut fs = Vec::with_capacity(sent.elements.len() - 1);
            for el in args {
                fs.push(lift_subform(syn, atoms, el)?);
            }
            Some(if op == OpKind::And { Form::And(fs) } else { Form::Or(fs) })
        }
        OpKind::Not => {
            let f = lift_subform(syn, atoms, args.next()?)?;
            Some(Form::Not(Box::new(f)))
        }
        OpKind::Implies => {
            let a = lift_subform(syn, atoms, args.next()?)?;
            let b = lift_subform(syn, atoms, args.next()?)?;
            Some(Form::Implies(Box::new(a), Box::new(b)))
        }
        OpKind::Iff => {
            let a = lift_subform(syn, atoms, args.next()?)?;
            let b = lift_subform(syn, atoms, args.next()?)?;
            Some(Form::Iff(Box::new(a), Box::new(b)))
        }
        OpKind::ForAll | OpKind::Exists => {
            // Shape: (forall (?X ?Y) body) — elements[1] is the varlist
            // sub-sentence (all Variables), elements[2] the body.
            let Some(Element::Sub(vl_sid)) = args.next() else { return None };
            let vl = atoms.resolve(*vl_sid, syn)?;
            let mut vars = Vec::with_capacity(vl.elements.len());
            for el in vl.elements.iter() {
                let Element::Variable { id, .. } = el else { return None };
                vars.push(*id);
            }
            let body = lift_subform(syn, atoms, args.next()?)?;
            Some(if op == OpKind::ForAll {
                Form::ForAll(vars, Box::new(body))
            } else {
                Form::Exists(vars, Box::new(body))
            })
        }
        OpKind::Equal => unreachable!("handled as an atom above"),
    }
}

fn lift_subform(syn: &SyntacticLayer, atoms: &AtomTable, el: &Element) -> Option<Form> {
    match el {
        Element::Sub(sid) => lift_form(syn, atoms, atoms.resolve(*sid, syn)?.as_ref()),
        // A bare symbol/variable in formula position is a propositional
        // atom (rare but legal).
        Element::Symbol(s) => Some(Form::Atom(Term::App(vec![Term::Sym(s.0.clone())]))),
        Element::Variable { id, .. } => Some(Form::Atom(Term::App(vec![Term::Var(*id)]))),
        _ => None,
    }
}

/// Lift a sentence in *term/atom* position into a [`Term::App`].
fn lift_term_of(syn: &SyntacticLayer, atoms: &AtomTable, sent: &Sentence) -> Option<Term> {
    let mut elems = Vec::with_capacity(sent.elements.len());
    for el in sent.elements.iter() {
        elems.push(lift_term_el(syn, atoms, el)?);
    }
    Some(Term::App(elems))
}

fn lift_term_el(syn: &SyntacticLayer, atoms: &AtomTable, el: &Element) -> Option<Term> {
    Some(match el {
        Element::Symbol(s)           => Term::Sym(s.0.clone()),
        Element::Variable { id, .. } => Term::Var(*id),
        Element::Literal(l)          => Term::Lit(l.clone()),
        Element::Op(op)              => Term::Op(op.clone()),
        Element::Sub(sid)            => lift_term_of(syn, atoms, atoms.resolve(*sid, syn)?.as_ref())?,
    })
}

// -- elim / NNF ----------------------------------------------------------------

/// Eliminate `=>` and `<=>` (prototype `elim`).
fn elim(f: Form) -> Form {
    match f {
        Form::Implies(a, b) => Form::Or(vec![
            Form::Not(Box::new(elim(*a))),
            elim(*b),
        ]),
        Form::Iff(a, b) => {
            let a = elim(*a);
            let b = elim(*b);
            Form::And(vec![
                Form::Or(vec![Form::Not(Box::new(a.clone())), b.clone()]),
                Form::Or(vec![Form::Not(Box::new(b)), a]),
            ])
        }
        Form::And(fs)        => Form::And(fs.into_iter().map(elim).collect()),
        Form::Or(fs)         => Form::Or(fs.into_iter().map(elim).collect()),
        Form::Not(g)         => Form::Not(Box::new(elim(*g))),
        Form::ForAll(vs, g)  => Form::ForAll(vs, Box::new(elim(*g))),
        Form::Exists(vs, g)  => Form::Exists(vs, Box::new(elim(*g))),
        atom @ Form::Atom(_) => atom,
    }
}

/// Push negation down to atoms — de Morgan + quantifier duality
/// (prototype `nnf`).
fn nnf(f: Form, neg: bool) -> Form {
    match f {
        Form::Not(g) => nnf(*g, !neg),
        Form::And(fs) => {
            let fs = fs.into_iter().map(|x| nnf(x, neg)).collect();
            if neg { Form::Or(fs) } else { Form::And(fs) }
        }
        Form::Or(fs) => {
            let fs = fs.into_iter().map(|x| nnf(x, neg)).collect();
            if neg { Form::And(fs) } else { Form::Or(fs) }
        }
        Form::ForAll(vs, g) => {
            let g = Box::new(nnf(*g, neg));
            if neg { Form::Exists(vs, g) } else { Form::ForAll(vs, g) }
        }
        Form::Exists(vs, g) => {
            let g = Box::new(nnf(*g, neg));
            if neg { Form::ForAll(vs, g) } else { Form::Exists(vs, g) }
        }
        atom @ Form::Atom(_) => {
            if neg { Form::Not(Box::new(atom)) } else { atom }
        }
        Form::Implies(..) | Form::Iff(..) => {
            unreachable!("elim runs before nnf")
        }
    }
}

// -- Free variables / substitution ----------------------------------------------

/// Free variables of an NNF form (prototype `freevars`).  `BTreeSet`
/// for the deterministic ordering the implicit ∀-closure needs.
fn freevars(f: &Form, bound: &BTreeSet<SymbolId>, out: &mut BTreeSet<SymbolId>) {
    match f {
        Form::Atom(t) => term_vars(t, bound, out),
        Form::Not(g) => freevars(g, bound, out),
        Form::And(fs) | Form::Or(fs) => {
            for g in fs { freevars(g, bound, out); }
        }
        Form::ForAll(vs, g) | Form::Exists(vs, g) => {
            let mut b2 = bound.clone();
            b2.extend(vs.iter().copied());
            freevars(g, &b2, out);
        }
        Form::Implies(a, b) | Form::Iff(a, b) => {
            freevars(a, bound, out);
            freevars(b, bound, out);
        }
    }
}

fn term_vars(t: &Term, bound: &BTreeSet<SymbolId>, out: &mut BTreeSet<SymbolId>) {
    match t {
        Term::Var(v) if !bound.contains(v) => { out.insert(*v); }
        Term::App(elems) => {
            for e in elems { term_vars(e, bound, out); }
        }
        _ => {}
    }
}

/// Apply a variable substitution to a term (prototype `subst`).
pub(crate) fn subst(t: &Term, m: &HashMap<SymbolId, Term>) -> Term {
    match t {
        Term::Var(v) => m.get(v).cloned().unwrap_or_else(|| t.clone()),
        Term::App(elems) => Term::App(elems.iter().map(|e| subst(e, m)).collect()),
        _ => t.clone(),
    }
}

// -- Skolemization ---------------------------------------------------------------

/// Deterministic per-root name state for skolemization.
struct SkolemCtx {
    /// The root sentence id — baked into every skolem/fresh name so
    /// re-clausifying the same root is idempotent and two roots can
    /// never share a skolem.
    root:    SentenceId,
    fresh_n: u64,
    sk_n:    u64,
}

impl SkolemCtx {
    /// A fresh universal variable id (standardize-apart).  Synthetic but
    /// deterministic: hashed from the root id + counter, in the same id
    /// space as interned scoped names (collision odds are the usual 2^-64).
    fn fresh_var(&mut self) -> SymbolId {
        let id = Symbol::hash_name(&format!("?fv{}__{:x}", self.fresh_n, self.root));
        self.fresh_n += 1;
        id
    }

    /// The next skolem head symbol: `sk_<root_hex>_<n>` — starts with the
    /// `sk_` prefix `is_skolem_name` recognises (cnf/skolem.rs).
    fn skolem_sym(&mut self) -> Symbol {
        let name = format!("sk_{:x}_{}", self.root, self.sk_n);
        self.sk_n += 1;
        Symbol::from(name)
    }
}

/// Walk an NNF form: rename universals fresh, replace existentials with
/// skolem terms over the enclosing universal scope, drop the quantifiers
/// (prototype `skolemize`).
fn skolemize(
    f:     Form,
    scope: &[Term],
    sub:   &HashMap<SymbolId, Term>,
    ctx:   &mut SkolemCtx,
) -> Form {
    match f {
        Form::ForAll(vs, g) => {
            let mut sub2 = sub.clone();
            let mut sc = scope.to_vec();
            for v in vs {
                let nv = ctx.fresh_var();
                sub2.insert(v, Term::Var(nv));
                sc.push(Term::Var(nv));
            }
            skolemize(*g, &sc, &sub2, ctx)
        }
        Form::Exists(vs, g) => {
            let mut sub2 = sub.clone();
            for v in vs {
                let head = Term::Sym(ctx.skolem_sym());
                let sk = if scope.is_empty() {
                    // Constant: a BARE symbol, exactly like any other
                    // constant — a 1-element App would slip past every
                    // symbol-shaped fast path (term_binary_ids, the
                    // oracle's learned units and FD congruence, the
                    // decode phone book), silently exempting skolem
                    // facts from theory reasoning.
                    head
                } else {
                    let mut elems = Vec::with_capacity(scope.len() + 1);
                    elems.push(head);
                    elems.extend(scope.iter().cloned());
                    Term::App(elems)
                };
                sub2.insert(v, sk);
            }
            skolemize(*g, scope, &sub2, ctx)
        }
        Form::And(fs) => Form::And(fs.into_iter().map(|x| skolemize(x, scope, sub, ctx)).collect()),
        Form::Or(fs)  => Form::Or(fs.into_iter().map(|x| skolemize(x, scope, sub, ctx)).collect()),
        // NNF: `not` wraps an atom.
        Form::Not(g) => match *g {
            Form::Atom(t) => Form::Not(Box::new(Form::Atom(subst(&t, sub)))),
            _ => unreachable!("NNF guarantees negation sits on atoms"),
        },
        Form::Atom(t) => Form::Atom(subst(&t, sub)),
        Form::Implies(..) | Form::Iff(..) => unreachable!("elim runs before skolemize"),
    }
}

// -- CNF distribution -------------------------------------------------------------

/// Distribute ∨ over ∧ (prototype `cnf`).  Leaves arrive as `Atom` /
/// `Not(Atom)` and come out as signed terms.  Returns `None` when the
/// product exceeds [`MAX_CLAUSES_PER_FORMULA`] — callers drop the
/// formula (and should say so).
fn distribute(f: &Form) -> Option<Vec<Vec<(bool, Term)>>> {
    match f {
        Form::And(fs) => {
            let mut out = Vec::new();
            for x in fs {
                out.extend(distribute(x)?);
                if out.len() > MAX_CLAUSES_PER_FORMULA { return None; }
            }
            Some(out)
        }
        Form::Or(fs) => {
            let mut prod: Vec<Vec<(bool, Term)>> = vec![vec![]];
            for x in fs {
                let rhs = distribute(x)?;
                let mut next = Vec::with_capacity(prod.len() * rhs.len());
                for a in &prod {
                    for b in &rhs {
                        let mut cl = a.clone();
                        cl.extend(b.iter().cloned());
                        next.push(cl);
                    }
                }
                if next.len() > MAX_CLAUSES_PER_FORMULA { return None; }
                prod = next;
            }
            Some(prod)
        }
        Form::Not(g) => match &**g {
            Form::Atom(t) => Some(vec![vec![(false, t.clone())]]),
            _ => unreachable!("NNF guarantees negation sits on atoms"),
        },
        Form::Atom(t) => Some(vec![vec![(true, t.clone())]]),
        _ => unreachable!("quantifiers eliminated by skolemize, => <=> by elim"),
    }
}

// -- Entry points ------------------------------------------------------------------

/// Clausify one stored root sentence (prototype `clausify`).
///
/// `negate` flips the formula first — conjecture clausification under
/// refutation.  Free variables get implicit universal closure *after*
/// the negation (which is exactly the ∃→∀ flip refutation needs).
///
/// Returns canonical, deduped, tautology-free clauses.  An empty result
/// means the formula produced nothing usable (or blew the distribution
/// guard); callers needing the distinction can check `lift_form` first.
pub(crate) fn clausify_sentence(
    syn:    &SyntacticLayer,
    atoms:  &AtomTable,
    sent:   &Sentence,
    root:   SentenceId,
    negate: bool,
) -> Vec<PClause> {
    let Some(lifted) = lift_form(syn, atoms, sent) else { return Vec::new() };
    let f = if negate { Form::Not(Box::new(lifted)) } else { lifted };
    clausify_form(f, atoms, root)
}

/// Clausify the NEGATED conjunction of several stored roots — the
/// refutation form of a conjunctive conjecture.
///
/// The KIF ingest pipeline splits a top-level `(and A B)` query into
/// separate store roots, so negating each root independently asserts
/// `¬A ∧ ¬B` — the negation of the DISJUNCTION — and a refutation of a
/// single conjunct would "prove" the whole conjunction.  The negation
/// must wrap the rebuilt conjunction: `¬(A ∧ B) = ¬A ∨ ¬B`.
pub(crate) fn clausify_negated_conjunction(
    syn:   &SyntacticLayer,
    atoms: &AtomTable,
    sents: &[(std::sync::Arc<Sentence>, SentenceId)],
) -> Vec<PClause> {
    let Some(root) = sents.first().map(|(_, sid)| *sid) else { return Vec::new() };
    let mut parts = Vec::with_capacity(sents.len());
    for (sent, _) in sents {
        let Some(l) = lift_form(syn, atoms, sent) else { return Vec::new() };
        parts.push(l);
    }
    let conj = if parts.len() == 1 { parts.pop().unwrap() } else { Form::And(parts) };
    clausify_form(Form::Not(Box::new(conj)), atoms, root)
}

/// The shared pipeline below the negation decision: NNF, universal
/// closure of free variables, skolemization, distribution, dedup.
fn clausify_form(f: Form, atoms: &AtomTable, root: SentenceId) -> Vec<PClause> {
    let f = nnf(elim(f), false);

    // Implicit universal closure over the (sorted) free variables.
    let mut fv = BTreeSet::new();
    freevars(&f, &BTreeSet::new(), &mut fv);
    let f = if fv.is_empty() {
        f
    } else {
        Form::ForAll(fv.into_iter().collect(), Box::new(f))
    };

    let mut ctx = SkolemCtx { root, fresh_n: 0, sk_n: 0 };
    let f = skolemize(f, &[], &HashMap::new(), &mut ctx);

    let Some(raw) = distribute(&f) else { return Vec::new() };

    let mut out: Vec<PClause> = Vec::with_capacity(raw.len());
    let mut seen_keys = std::collections::HashSet::new();
    'clauses: for cl in raw {
        if cl.len() > MAX_LITS_PER_CLAUSE { continue; }
        // In-clause literal dedup + tautology check on the raw terms
        // (same var names within one clause, so plain equality works —
        // mirrors the prototype's `seen` / `pos_atoms` passes).
        let mut lits: Vec<(bool, Term)> = Vec::with_capacity(cl.len());
        for (pos, t) in cl {
            if !lits.iter().any(|(p, u)| *p == pos && *u == t) {
                lits.push((pos, t));
            }
        }
        for (p, t) in &lits {
            if !*p && lits.iter().any(|(q, u)| *q && u == t) {
                continue 'clauses; // tautology: P ∨ ¬P
            }
        }
        let clause = canonical_clause(lits, atoms);
        if seen_keys.insert(clause.key) {
            out.push(clause);
        }
    }
    out
}
