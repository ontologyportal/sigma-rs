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
use std::sync::atomic::{AtomicU64, Ordering};

use crate::parse::OpKind;
use crate::syntactic::SyntacticLayer;
use crate::types::{Element, Sentence, SentenceId, Symbol, SymbolId};

use super::canon::canonical_clause;
use super::clause::{AtomTable, PClause, Term};

/// Distribution guard: a single formula may not explode into more than
/// this many clauses (matches the prototype's MAX_CLAUSES guard — big
/// `<=>` nests are dropped rather than allowed to take the KB hostage).
/// A formula that trips this guard no longer simply records a loss: it is
/// retried through the definitional-CNF rescue path (see `defcnf_rescue`),
/// which introduces fresh predicate definitions until the distribution
/// estimate fits, and only falls back to the lossy result when even that
/// fails.
pub(crate) const MAX_CLAUSES_PER_FORMULA: usize = 128;

/// A clause longer than this is dropped — such clauses are practically
/// unusable for resolution and only bloat the passive queue.
pub(crate) const MAX_LITS_PER_CLAUSE: usize = 32;

/// Absolute insanity guard for the definitional-CNF rescue path.  After
/// definitions are introduced the clause count is LINEAR in the input
/// formula size (no distribution products survive), so the multiplicative
/// `MAX_CLAUSES_PER_FORMULA` guard no longer applies — a big conjunction
/// legitimately yields one clause per conjunct.  This cap only bounds the
/// pathological end of "linear" (a multi-megabyte single formula); tripping
/// it abandons the rescue and keeps the original lossy result.
pub(crate) const DEFCNF_MAX_CLAUSES_PER_FORMULA: usize = 65_536;

/// The KappaFn class former, KIF `(KappaFn ?V φ)` — recognized by NAME,
/// mirroring `trans/caches/ho_signatures.rs`'s `KAPPA_FN` (kept as a
/// separate const: the trans module is a different feature axis).
pub(crate) const KAPPA_FN: &str = "KappaFn";

/// The reserved quote constructor a lifted KappaFn class term is built
/// from: `(kappa_q qb<n> <quoted body>)` — binder discipline exactly as
/// `forall_q`, except the binder is a single bound-constant (KappaFn
/// binds exactly one variable), not a varlist.
pub(crate) const KAPPA_CTOR: &str = "kappa_q";

/// Comprehension-emission cap per root, mirroring defCNF's per-root cap
/// discipline: beyond it further KappaFn terms still LIFT structurally
/// (the class term loads) but carry no comprehension units — recorded as
/// input LOSS so the completeness gate withholds countermodel verdicts.
pub(crate) const MAX_KAPPA_COMPREHENSIONS_PER_ROOT: u32 = 8;

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

/// Per-root lift context (KappaFn comprehension, HO-parity part B):
/// collects the per-instance comprehension units minted while lifting,
/// numbered deterministically per root.
pub(crate) struct LiftCtx {
    root:    SentenceId,
    /// Comprehension units in traversal (mint) order — lowered alongside
    /// the root's main formula by `clausify_form` under the SAME per-root
    /// skolem context (the defCNF auxiliary-clause pattern: deterministic
    /// order ⇒ deterministic names, byte-identical re-clausification).
    units:   Vec<Form>,
    /// Next kappa sequence number within this root (names the fresh
    /// instance/binder variables deterministically).
    kappa_n: u32,
    /// Comprehension was SKIPPED for a kappa term because the per-root
    /// cap was hit — input loss the completeness gate must see.
    capped:  bool,
}

impl LiftCtx {
    fn new(root: SentenceId) -> Self {
        LiftCtx { root, units: Vec::new(), kappa_n: 0, capped: false }
    }
}

/// Lift a stored sentence into a [`Form`], resolving nested sub-sentences
/// through the store.  Returns `None` for shapes the clausifier cannot
/// handle (malformed quantifiers, malformed KappaFn, unresolvable
/// subterms) — the caller skips the formula rather than guessing.
pub(crate) fn lift_form(
    syn:   &SyntacticLayer,
    atoms: &AtomTable,
    sent:  &Sentence,
    lctx:  &mut LiftCtx,
) -> Option<Form> {
    let op = match sent.elements.first() {
        Some(Element::Op(op)) if *op != OpKind::Equal => op.clone(),
        // Symbol- or variable-headed (incl. predicate variables) and
        // equality sentences are atoms.
        _ => return Some(Form::Atom(lift_term_of(syn, atoms, sent, lctx)?)),
    };
    let mut args = sent.elements.iter().skip(1);
    match op {
        OpKind::And | OpKind::Or => {
            let mut fs = Vec::with_capacity(sent.elements.len() - 1);
            for el in args {
                fs.push(lift_subform(syn, atoms, el, lctx)?);
            }
            Some(if op == OpKind::And { Form::And(fs) } else { Form::Or(fs) })
        }
        OpKind::Not => {
            let f = lift_subform(syn, atoms, args.next()?, lctx)?;
            Some(Form::Not(Box::new(f)))
        }
        OpKind::Implies => {
            let a = lift_subform(syn, atoms, args.next()?, lctx)?;
            let b = lift_subform(syn, atoms, args.next()?, lctx)?;
            Some(Form::Implies(Box::new(a), Box::new(b)))
        }
        OpKind::Iff => {
            let a = lift_subform(syn, atoms, args.next()?, lctx)?;
            let b = lift_subform(syn, atoms, args.next()?, lctx)?;
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
            let body = lift_subform(syn, atoms, args.next()?, lctx)?;
            Some(if op == OpKind::ForAll {
                Form::ForAll(vars, Box::new(body))
            } else {
                Form::Exists(vars, Box::new(body))
            })
        }
        OpKind::Equal => unreachable!("handled as an atom above"),
    }
}

fn lift_subform(
    syn:   &SyntacticLayer,
    atoms: &AtomTable,
    el:    &Element,
    lctx:  &mut LiftCtx,
) -> Option<Form> {
    match el {
        Element::Sub(sid) => lift_form(syn, atoms, atoms.resolve(*sid, syn)?.as_ref(), lctx),
        // A bare symbol/variable in formula position is a propositional
        // atom (rare but legal).
        Element::Symbol(s) => Some(Form::Atom(Term::App(vec![Term::Sym(s.0.clone())]))),
        Element::Variable { id, .. } => Some(Form::Atom(Term::App(vec![Term::Var(*id)]))),
        _ => None,
    }
}

/// Lift a sentence in *term/atom* position into a [`Term::App`].
fn lift_term_of(
    syn:   &SyntacticLayer,
    atoms: &AtomTable,
    sent:  &Sentence,
    lctx:  &mut LiftCtx,
) -> Option<Term> {
    let mut elems = Vec::with_capacity(sent.elements.len());
    for el in sent.elements.iter() {
        elems.push(lift_term_el(syn, atoms, el, lctx)?);
    }
    Some(Term::App(elems))
}

fn lift_term_el(
    syn:   &SyntacticLayer,
    atoms: &AtomTable,
    el:    &Element,
    lctx:  &mut LiftCtx,
) -> Option<Term> {
    Some(match el {
        Element::Symbol(s)           => Term::Sym(s.0.clone()),
        Element::Variable { id, .. } => Term::Var(*id),
        Element::Literal(l)          => Term::Lit(l.clone()),
        Element::Op(op)              => Term::Op(op.clone()),
        Element::Sub(sid)            => {
            let sent = atoms.resolve(*sid, syn)?;
            match kappa_shape(sent.as_ref()) {
                KappaShape::Kappa { binder, body } => {
                    // `(KappaFn ?V φ)` in plain term position: opens its
                    // own quote scope (a maximal quote, like any other
                    // formula-shaped argument) AND emits its per-instance
                    // comprehension units (`emit = true`).
                    lift_kappa_term(
                        syn, atoms, binder, body, &mut QuoteEnv::default(), lctx, true)?
                }
                KappaShape::Malformed => {
                    // Documented bail: a malformed KappaFn (arity ≠ 3 or
                    // non-variable binder) drops the whole root through
                    // the existing LOSSY path — counted — rather than
                    // loading a wrong encoding.
                    KAPPA_MALFORMED_BAILS.fetch_add(1, Ordering::Relaxed);
                    return None;
                }
                KappaShape::NotKappa => {
                    if is_formula_shaped(sent.as_ref()) {
                        // An embedded connective/quantifier in ARGUMENT
                        // position (e.g. `(believes John (and P Q))`):
                        // lift as a structured QUOTE — reserved
                        // constructor symbols, binders alpha-normalized
                        // to quote-scoped bound constants.  Each maximal
                        // formula-shaped argument opens its own quote
                        // scope (`QuoteEnv::default`).
                        lift_quote_sentence(
                            syn, atoms, sent.as_ref(), &mut QuoteEnv::default(), lctx)?
                    } else {
                        lift_term_of(syn, atoms, sent.as_ref(), lctx)?
                    }
                }
            }
        }
    })
}

// -- Structured quoting of embedded formulas (argument position) ---------------
//
// A formula appearing in ARGUMENT position under a predicate — e.g.
// `(believes John (and P Q))` or `(holdsDuring T (forall (?X) ...))` —
// is QUOTED: the prover sees an ordinary first-order term built from
// reserved constructor symbols, never a connective the calculus would
// interpret.  Design (docs/plans/doxastic-contexts.md, step 0):
//
//   * Connectives map to reserved function symbols `and_q` / `or_q` /
//     `not_q` / `impl_q` / `iff_q`; quantifiers to `forall_q` /
//     `exists_q`.  Like skolem (`sk_…`) and defCNF (`df_…`) names these
//     are minted as plain `Symbol`s, so their ids are the deterministic
//     content hashes of the names — stable across runs and KBs.
//   * Binder alpha-normalization: variables bound INSIDE the quote are
//     replaced by canonical bound-CONSTANTS `qb0`, `qb1`, … numbered by
//     first occurrence within the quote (the varlist is traversed first,
//     so binder order is the occurrence order).  Alpha-variant quotes
//     therefore lift to byte-identical terms (one interned atom), and
//     unification can never capture into a quoted scope — bound
//     occurrences are constants, not variables.
//   * FREE variables of the embedding context stay real variables, so
//     rules like `(=> (believes J ?P) …)` bind whole quotes through `?P`
//     and `(believes J (loves ?X Mary))` keeps `?X` linked to the
//     enclosing scope.
//   * Equality is NOT a connective here: an embedded `(= a b)` keeps its
//     atom shape (`Op(Equal)`-headed app), exactly as before.
//
// This applies ONLY in term/argument position — top-level formula
// structure clausifies exactly as always (`lift_form` above).  The
// saturation kernel is untouched: quotes are ordinary FO terms.

/// `true` when a stored sentence is formula-shaped for quoting purposes:
/// headed by a logical connective or quantifier (equality excluded — an
/// equality in argument position keeps its atom shape).
fn is_formula_shaped(sent: &Sentence) -> bool {
    matches!(sent.elements.first(), Some(Element::Op(op)) if *op != OpKind::Equal)
}

/// The reserved quote-constructor symbol for a connective/quantifier.
pub(crate) fn quote_ctor_name(op: &OpKind) -> Option<&'static str> {
    Some(match op {
        OpKind::And     => "and_q",
        OpKind::Or      => "or_q",
        OpKind::Not     => "not_q",
        OpKind::Implies => "impl_q",
        OpKind::Iff     => "iff_q",
        OpKind::ForAll  => "forall_q",
        OpKind::Exists  => "exists_q",
        OpKind::Equal   => return None,
    })
}

/// The canonical bound-constant for quote binder slot `n` (`qb0`, `qb1`, …).
fn quote_bound_sym(n: u32) -> Symbol {
    Symbol::from(format!("qb{n}"))
}

/// Per-quote binder environment: a stack of binder frames (shadowing) and
/// the first-occurrence counter for bound-constant numbering.  Scoped to
/// one maximal quote — a fresh quote (a different formula-shaped
/// argument) numbers from `qb0` again, which is exactly what makes
/// alpha-variants intern identically.
#[derive(Default)]
struct QuoteEnv {
    frames: Vec<Vec<(SymbolId, u32)>>,
    next:   u32,
}

impl QuoteEnv {
    fn lookup(&self, id: SymbolId) -> Option<u32> {
        self.frames.iter().rev().find_map(|f| {
            f.iter().find(|(v, _)| *v == id).map(|(_, n)| *n)
        })
    }
}

/// Lift a formula-shaped stored sentence in argument position into a
/// quoted [`Term`].  Returns `None` for malformed quantifier shapes
/// (non-variable varlist entries, missing body) — a documented bail: the
/// axiom then drops as lossy rather than loading a wrong encoding.
fn lift_quote_sentence(
    syn:   &SyntacticLayer,
    atoms: &AtomTable,
    sent:  &Sentence,
    env:   &mut QuoteEnv,
    lctx:  &mut LiftCtx,
) -> Option<Term> {
    let op = match sent.elements.first() {
        Some(Element::Op(op)) if *op != OpKind::Equal => op.clone(),
        // Atomic (symbol/variable/equality-headed) sentence inside the
        // quote: an ordinary application, with binder substitution.
        _ => {
            let mut elems = Vec::with_capacity(sent.elements.len());
            for el in sent.elements.iter() {
                elems.push(lift_quote_el(syn, atoms, el, env, lctx)?);
            }
            return Some(Term::App(elems));
        }
    };
    let ctor = Term::Sym(Symbol::from(quote_ctor_name(&op)?));
    match op {
        OpKind::ForAll | OpKind::Exists => {
            // Shape: (forall (?X ?Y) body) — mirror `lift_form`'s
            // recognizer, bail on anything else.
            if sent.elements.len() != 3 { return None; }
            let Some(Element::Sub(vl_sid)) = sent.elements.get(1) else { return None };
            let vl = atoms.resolve(*vl_sid, syn)?;
            let mut frame = Vec::with_capacity(vl.elements.len());
            let mut vl_elems = Vec::with_capacity(vl.elements.len());
            for el in vl.elements.iter() {
                let Element::Variable { id, .. } = el else { return None };
                // Rebinding the same name within ONE varlist keeps the
                // first slot (degenerate source; lookup finds it either
                // way).  Shadowing across NESTED quantifiers works via
                // the frame stack.
                let n = env.next;
                env.next += 1;
                frame.push((*id, n));
                vl_elems.push(Term::Sym(quote_bound_sym(n)));
            }
            env.frames.push(frame);
            let body = lift_quote_el(syn, atoms, &sent.elements[2], env, lctx);
            env.frames.pop();
            Some(Term::App(vec![ctor, Term::App(vl_elems), body?]))
        }
        _ => {
            let mut elems = Vec::with_capacity(sent.elements.len());
            elems.push(ctor);
            for el in sent.elements.iter().skip(1) {
                elems.push(lift_quote_el(syn, atoms, el, env, lctx)?);
            }
            Some(Term::App(elems))
        }
    }
}

/// One element inside a quote: bound variables become bound constants,
/// free variables stay variables, nested sub-sentences recurse under the
/// same quote environment.
fn lift_quote_el(
    syn:   &SyntacticLayer,
    atoms: &AtomTable,
    el:    &Element,
    env:   &mut QuoteEnv,
    lctx:  &mut LiftCtx,
) -> Option<Term> {
    Some(match el {
        Element::Symbol(s)  => Term::Sym(s.0.clone()),
        Element::Variable { id, .. } => match env.lookup(*id) {
            Some(n) => Term::Sym(quote_bound_sym(n)),
            None    => Term::Var(*id),
        },
        Element::Literal(l) => Term::Lit(l.clone()),
        Element::Op(op)     => Term::Op(op.clone()),
        Element::Sub(sid)   => {
            let sent = atoms.resolve(*sid, syn)?;
            match kappa_shape(sent.as_ref()) {
                KappaShape::Kappa { binder, body } => {
                    // A KappaFn NESTED inside an enclosing quote: same
                    // env (binder numbering continues — exactly the
                    // shared-scope rule nested quantifiers follow), and
                    // NO comprehension (`emit = false`): quoted content
                    // is mentioned, not asserted — emitting here would
                    // bridge through an unasserted context.
                    lift_kappa_term(syn, atoms, binder, body, env, lctx, false)?
                }
                KappaShape::Malformed => {
                    KAPPA_MALFORMED_BAILS.fetch_add(1, Ordering::Relaxed);
                    return None;
                }
                KappaShape::NotKappa => {
                    lift_quote_sentence(syn, atoms, sent.as_ref(), env, lctx)?
                }
            }
        }
    })
}

// -- KappaFn per-instance comprehension (native HO-parity, part B) --------------
//
// KIF's `(KappaFn ?V φ)` denotes "the class of ?V such that φ" — a term.
// It lifts as a quoted class term `(kappa_q qb<n> <quoted body>)`, same
// binder discipline as `forall_q` (alpha-normalized bound-constants, so
// alpha-variant kappa terms intern identically and the SAME KappaFn
// expression asked as a query denotes the SAME term — quote dedup).
//
// The MEANING of the class term is supplied by PER-INSTANCE comprehension,
// emitted as auxiliary clauses attached to the same root (the defCNF
// auxiliary-clause pattern):
//
//   (=> (instance ?X <kterm>) body[?V := ?X])     — elimination
//   (=> body[?V := ?X] (instance ?X <kterm>))     — introduction
//
// where `body[?V := ?X]` comes from `unquote_form`, the
// unquote-with-substitution primitive: the INVERSE of the quote walk,
// one level deep.
//
// GUARDRAIL (Montague's paradox — non-negotiable): every quote-to-
// assertion bridge here is PER-INSTANCE — comprehension for one SPECIFIC
// KappaFn term met at clausification time, whose unquoted body is a
// FIXED formula skeleton.  There is deliberately NO general rule of the
// shape "for all quoted ?P: holds(?P) implies <?P unquoted>" — that
// schema reproduces Montague's paradox, and this module must never grow
// one.

/// Process-cumulative part-B counters, surfaced by `SIGMA_STATS`
/// (prove.rs) when nonzero — same regime as `defcnf_counters`.
static KAPPA_COMPREHENSIONS:  AtomicU64 = AtomicU64::new(0);
static KAPPA_MALFORMED_BAILS: AtomicU64 = AtomicU64::new(0);

/// (comprehensions_emitted, malformed_bails) — process-cumulative.
pub(crate) fn kappa_counters() -> (u64, u64) {
    (KAPPA_COMPREHENSIONS.load(Ordering::Relaxed),
     KAPPA_MALFORMED_BAILS.load(Ordering::Relaxed))
}

/// Recognizer for `(KappaFn ?V φ)` sub-sentences.
enum KappaShape<'a> {
    /// Not KappaFn-headed at all.
    NotKappa,
    /// KappaFn-headed but not the binder shape — arity ≠ 3 or a
    /// non-variable binder.  Documented bail: the root drops as LOSSY
    /// (counted), never loads a wrong encoding.
    Malformed,
    /// Well-formed: the binder variable and the body element.
    Kappa { binder: SymbolId, body: &'a Element },
}

fn kappa_shape(sent: &Sentence) -> KappaShape<'_> {
    let Some(Element::Symbol(h)) = sent.elements.first() else { return KappaShape::NotKappa };
    if &*h.name() != KAPPA_FN {
        return KappaShape::NotKappa;
    }
    if sent.elements.len() != 3 {
        return KappaShape::Malformed;
    }
    let Element::Variable { id, .. } = &sent.elements[1] else { return KappaShape::Malformed };
    KappaShape::Kappa { binder: *id, body: &sent.elements[2] }
}

/// Lift a well-formed `(KappaFn ?V body)` into `(kappa_q qb<n> <quoted
/// body>)` under `env` (the binder gets the next bound-constant slot,
/// exactly like a `forall_q` binder), and — when `emit` (plain term
/// position only) — mint the per-instance comprehension units into
/// `lctx`, capped per root.
fn lift_kappa_term(
    syn:    &SyntacticLayer,
    atoms:  &AtomTable,
    binder: SymbolId,
    body:   &Element,
    env:    &mut QuoteEnv,
    lctx:   &mut LiftCtx,
    emit:   bool,
) -> Option<Term> {
    let n = env.next;
    env.next += 1;
    env.frames.push(vec![(binder, n)]);
    let body_t = lift_quote_el(syn, atoms, body, env, lctx);
    env.frames.pop();
    let body_t = body_t?;
    let kterm = Term::App(vec![
        Term::Sym(Symbol::from(KAPPA_CTOR)),
        Term::Sym(quote_bound_sym(n)),
        body_t.clone(),
    ]);
    if !emit {
        return Some(kterm);
    }
    if lctx.kappa_n >= MAX_KAPPA_COMPREHENSIONS_PER_ROOT {
        // Cap discipline (mirrors defCNF's per-root caps): the class
        // term still loads, its comprehension does not — input LOSS.
        lctx.capped = true;
        return Some(kterm);
    }
    let k = lctx.kappa_n;
    lctx.kappa_n += 1;

    // The fresh instance variable ?X of `body[?V := ?X]`: deterministic
    // per (root, kappa sequence) — same reserved-name id space (and the
    // same 2^-64 collision argument) as `SkolemCtx::fresh_var`.
    let iv = Symbol::hash_name(&format!("?ki{k}__{:x}", lctx.root));
    let mut sub: HashMap<SymbolId, Term> = HashMap::new();
    sub.insert(quote_bound_sym(n).id(), Term::Var(iv));
    // Fresh real variables for quantifier binders unquoted along the way,
    // numbered in traversal order — deterministic.
    let root = lctx.root;
    let mut fresh_n: u32 = 0;
    let mut fresh = move || {
        let id = Symbol::hash_name(&format!("?kq{fresh_n}_{k}__{root:x}"));
        fresh_n += 1;
        id
    };
    // Unquote ONE level: the kappa's own quote scope becomes assertion-
    // level formula structure; quotes nested inside atoms stay quoted.
    // `None` (a term shape the walker never produces) → treat the root
    // like a malformed kappa: documented lossy bail, counted.
    let Some(body_form) = unquote_form(&body_t, &mut sub, &mut fresh) else {
        KAPPA_MALFORMED_BAILS.fetch_add(1, Ordering::Relaxed);
        return None;
    };

    // PER-INSTANCE comprehension — the guardrail-compliant bridge shape:
    // both directions for THIS kappa term only; `body_form` is a fixed
    // formula skeleton fixed here at clausification time, never a
    // quantified quote variable, so no general truth/unquote bridge
    // (Montague) can arise from these units.
    let inst_atom = Form::Atom(Term::App(vec![
        Term::Sym(Symbol::from("instance")),
        Term::Var(iv),
        kterm.clone(),
    ]));
    lctx.units.push(Form::Implies(
        Box::new(inst_atom.clone()), Box::new(body_form.clone())));
    lctx.units.push(Form::Implies(
        Box::new(body_form), Box::new(inst_atom)));
    KAPPA_COMPREHENSIONS.fetch_add(1, Ordering::Relaxed);
    Some(kterm)
}

/// The reverse of [`quote_ctor_name`]: which connective a reserved quote
/// constructor symbol encodes.
fn unquote_op(h: &Symbol) -> Option<OpKind> {
    Some(match &*h.name() {
        "and_q"    => OpKind::And,
        "or_q"     => OpKind::Or,
        "not_q"    => OpKind::Not,
        "impl_q"   => OpKind::Implies,
        "iff_q"    => OpKind::Iff,
        "forall_q" => OpKind::ForAll,
        "exists_q" => OpKind::Exists,
        _ => return None,
    })
}

/// THE unquote-with-substitution primitive (part B): rebuild assertion-
/// level [`Form`] structure from a quoted term — the inverse of the quote
/// walk (`lift_quote_sentence`), ONE quote level deep:
///
///   * Quote constructors at FORMULA level invert to their connectives;
///     `forall_q`/`exists_q` rebind their `qb` bound-constants to fresh
///     REAL variables (`fresh`, traversal order — "up to binder renaming").
///   * Anything else is an ATOM: kept verbatim as a term with `sub`
///     applied throughout — free-variable threading (a real variable
///     captured at quote time threads back as itself) and the kappa-binder
///     instantiation both happen here.  Quote constructors nested INSIDE
///     atom arguments (a `believes`-style nested quote) therefore stay
///     quoted — their scopes are re-canonicalized to number from `qb0`
///     again (`renumber_nested_scopes`), which is exactly what a direct
///     assertion of the unquoted formula would produce (quote dedup).
///
/// `sub` may gain entries as quantifiers are unquoted; `qb` slot numbers
/// are unique within one quote scope (the walker's counter never reuses a
/// slot), so entries never need scoping/restoring.  `None` = a shape the
/// quote walker never produces (defensive; callers bail the root as
/// lossy).
///
/// GUARDRAIL (Montague): this primitive is only ever invoked on the
/// per-instance comprehension path for a SPECIFIC kappa term met at
/// clausification time — never as a rule over quantified quote variables.
fn unquote_form<F: FnMut() -> SymbolId>(
    t:     &Term,
    sub:   &mut HashMap<SymbolId, Term>,
    fresh: &mut F,
) -> Option<Form> {
    if let Term::App(elems) = t {
        if let Some(Term::Sym(h)) = elems.first() {
            match unquote_op(h) {
                Some(op @ (OpKind::And | OpKind::Or)) => {
                    if elems.len() < 2 { return None; }
                    let mut fs = Vec::with_capacity(elems.len() - 1);
                    for e in &elems[1..] {
                        fs.push(unquote_form(e, sub, fresh)?);
                    }
                    return Some(if op == OpKind::And { Form::And(fs) } else { Form::Or(fs) });
                }
                Some(OpKind::Not) => {
                    if elems.len() != 2 { return None; }
                    return Some(Form::Not(Box::new(unquote_form(&elems[1], sub, fresh)?)));
                }
                Some(op @ (OpKind::Implies | OpKind::Iff)) => {
                    if elems.len() != 3 { return None; }
                    let a = Box::new(unquote_form(&elems[1], sub, fresh)?);
                    let b = Box::new(unquote_form(&elems[2], sub, fresh)?);
                    return Some(if op == OpKind::Implies {
                        Form::Implies(a, b)
                    } else {
                        Form::Iff(a, b)
                    });
                }
                Some(op @ (OpKind::ForAll | OpKind::Exists)) => {
                    if elems.len() != 3 { return None; }
                    let Term::App(vl) = &elems[1] else { return None };
                    let mut vars = Vec::with_capacity(vl.len());
                    for b in vl {
                        let Term::Sym(qb) = b else { return None };
                        let v = fresh();
                        sub.insert(qb.id(), Term::Var(v));
                        vars.push(v);
                    }
                    let body = Box::new(unquote_form(&elems[2], sub, fresh)?);
                    return Some(if op == OpKind::ForAll {
                        Form::ForAll(vars, body)
                    } else {
                        Form::Exists(vars, body)
                    });
                }
                Some(OpKind::Equal) | None => {} // an atom — fall through
            }
        }
    }
    // ATOM level: substitute throughout (nested quotes included — the
    // free-variable rule), re-canonicalize nested quote scopes, and wrap
    // bare symbols/variables exactly like `lift_subform` does.
    let t2 = renumber_nested_scopes(&subst_qb(t, sub));
    Some(Form::Atom(match t2 {
        t2 @ Term::App(_) => t2,
        other => Term::App(vec![other]),
    }))
}

/// Replace quote bound-constants per `sub` throughout a term (nested
/// quotes included).  The inverse-direction sibling of `subst` (which
/// maps variables); everything else copies verbatim.
fn subst_qb(t: &Term, sub: &HashMap<SymbolId, Term>) -> Term {
    match t {
        Term::Sym(s) => sub.get(&s.id()).cloned().unwrap_or_else(|| t.clone()),
        Term::App(elems) => Term::App(elems.iter().map(|e| subst_qb(e, sub)).collect()),
        _ => t.clone(),
    }
}

/// `true` when the term is a quote-scope head: a quote-constructor (or
/// `kappa_q`) application — i.e. a maximal nested quote when found in
/// atom-argument position.
fn is_scope_head(t: &Term) -> bool {
    matches!(t, Term::App(es) if matches!(es.first(),
        Some(Term::Sym(h)) if unquote_op(h).is_some() || &*h.name() == KAPPA_CTOR))
}

/// `Some(n)` when `name` is a quote bound-constant `qb<n>`.
fn qb_slot(name: &str) -> Option<u32> {
    let digits = name.strip_prefix("qb")?;
    if digits.is_empty() { return None; }
    digits.parse::<u32>().ok()
}

/// Re-canonicalize the MAXIMAL nested quote scopes inside an unquoted
/// atom: their binders were numbered by the ENCLOSING scope's shared
/// counter at quote time; once the enclosing level is unquoted they are
/// standalone scopes again and must number from `qb0` by first occurrence
/// — byte-identical to what a direct assertion of the unquoted formula
/// would lift to, preserving quote dedup.  (Binders of the UNQUOTED
/// levels were already substituted away to real variables, so every `qb`
/// remaining inside a nested scope belongs to that scope.)
fn renumber_nested_scopes(t: &Term) -> Term {
    match t {
        Term::App(es) => Term::App(es.iter().map(|e| {
            if is_scope_head(e) { renumber_scope(e) } else { renumber_nested_scopes(e) }
        }).collect()),
        _ => t.clone(),
    }
}

fn renumber_scope(t: &Term) -> Term {
    let mut order: Vec<(SymbolId, u32)> = Vec::new();
    collect_qb_syms(t, &mut order);
    // First-occurrence order over the scope's term equals the walker's
    // numbering order (each quantifier's varlist precedes its body), so
    // position i renumbers to `qb<i>`; skip already-canonical slots.
    let map: HashMap<SymbolId, Term> = order.iter().enumerate()
        .filter(|(i, (_, slot))| *slot != *i as u32)
        .map(|(i, (id, _))| (*id, Term::Sym(quote_bound_sym(i as u32))))
        .collect();
    if map.is_empty() { t.clone() } else { subst_qb(t, &map) }
}

/// Quote bound-constants of `t` in first-occurrence order, with their
/// current slot numbers.
fn collect_qb_syms(t: &Term, out: &mut Vec<(SymbolId, u32)>) {
    match t {
        Term::Sym(s) => {
            if let Some(slot) = qb_slot(&s.name()) {
                if !out.iter().any(|(id, _)| *id == s.id()) {
                    out.push((s.id(), slot));
                }
            }
        }
        Term::App(es) => {
            for e in es {
                collect_qb_syms(e, out);
            }
        }
        _ => {}
    }
}

// -- Modal K-distribution schemata (native HO-parity, part A) -------------------
//
// SUMO's `knows`/`believes` are idealized (consequence-closed) attitudes;
// over QUOTED contents an uninterpreted attitude relation cannot even
// distribute over conjunction — `believes(a, and_q(p, q)) ⊢ believes(a, p)`
// is invalid without an axiom, because `and_q(p, q)` is just a term.  The
// K-distribution schema, stated over the quote constructor,
//
//   (=> (rel ?A (and_q ?P ?Q)) (and (rel ?A ?P) (rel ?A ?Q)))
//
// restores exactly the fragment the THF translation lane injects
// (`trans/lower_thf.rs`'s K-axiom chip) — parity scope only: conjunction
// distribution for `knows`/`believes`, nothing else (no D, no deductive
// closure, no iff).
//
// GUARDRAIL (Montague's paradox — non-negotiable): these schemata only
// REARRANGE quoted structure.  `?P`/`?Q` bind whole quotes and land back
// in argument position of the SAME attitude relation; nothing is ever
// unquoted to assertion level, so no general truth/unquote bridge over
// quantified quote variables exists here.
//
// Whether to inject is decided per problem at assembly time
// (`prove.rs`'s `modal_k_qualifying`): only when the KB's taxonomy
// actually DECLARES the relation's argument-2 domain as `Formula` (or a
// Formula descendant), and only when the relation occurs in the problem.

/// The two K-distribution clauses for attitude relation `rel`:
///   `¬rel(?A, and_q(?P, ?Q)) ∨ rel(?A, ?P)`
///   `¬rel(?A, and_q(?P, ?Q)) ∨ rel(?A, ?Q)`
/// Deterministic: fixed synthetic variable ids (hashed reserved names,
/// same id space as `SkolemCtx::fresh_var`), canonicalized through the
/// ordinary clause pipeline.
pub(crate) fn modal_k_clauses(rel: &str, atoms: &AtomTable) -> Vec<PClause> {
    let a = Term::Var(Symbol::hash_name("?A__modal_k"));
    let p = Term::Var(Symbol::hash_name("?P__modal_k"));
    let q = Term::Var(Symbol::hash_name("?Q__modal_k"));
    let att = |x: Term| Term::App(vec![Term::Sym(Symbol::from(rel)), a.clone(), x]);
    let and_q = quote_ctor_name(&OpKind::And).expect("And always has a quote ctor");
    let conj = att(Term::App(vec![Term::Sym(Symbol::from(and_q)), p.clone(), q.clone()]));
    let raw = vec![
        vec![(false, conj.clone()), (true, att(p))],
        vec![(false, conj), (true, att(q))],
    ];
    filter_canonicalize(raw, atoms).0
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
    /// Goal-mode (negated conjecture) clausification.  SentenceIds are
    /// content hashes, so a conjecture CONTAINING a KB axiom (a
    /// conjunction whose first conjunct is content-identical to it)
    /// clausifies under the SAME root as the axiom — without a namespace
    /// split, the axiom's existential witness and the goal's
    /// counterexample witness both mint `sk_<root>_0` and a resolution
    /// between them fabricates a refutation (a false Proved).
    goal:    bool,
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

    /// The next skolem head symbol: `sk_<root_hex>_<n>` (axiom mode) or
    /// `sk_g<root_hex>_<n>` (goal mode — see the `goal` field; both start
    /// with the `sk_` prefix `is_skolem_name` recognises, cnf/skolem.rs).
    fn skolem_sym(&mut self) -> Symbol {
        let name = if self.goal {
            format!("sk_g{:x}_{}", self.root, self.sk_n)
        } else {
            format!("sk_{:x}_{}", self.root, self.sk_n)
        };
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
/// product exceeds `cap` ([`MAX_CLAUSES_PER_FORMULA`] on the primary
/// path, [`DEFCNF_MAX_CLAUSES_PER_FORMULA`] on the rescue path) —
/// callers drop the formula (and should say so).
fn distribute(f: &Form, cap: usize) -> Option<Vec<Vec<(bool, Term)>>> {
    match f {
        Form::And(fs) => {
            let mut out = Vec::new();
            for x in fs {
                out.extend(distribute(x, cap)?);
                if out.len() > cap { return None; }
            }
            Some(out)
        }
        Form::Or(fs) => {
            let mut prod: Vec<Vec<(bool, Term)>> = vec![vec![]];
            for x in fs {
                let rhs = distribute(x, cap)?;
                let mut next = Vec::with_capacity(prod.len() * rhs.len());
                for a in &prod {
                    for b in &rhs {
                        let mut cl = a.clone();
                        cl.extend(b.iter().cloned());
                        next.push(cl);
                    }
                }
                if next.len() > cap { return None; }
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
    clausify_sentence_lossy(syn, atoms, sent, root, negate).0
}

/// [`clausify_sentence`] plus a LOSS flag: `true` when clauses were
/// (partially or wholly) dropped for shape/capacity reasons — an
/// unsupported shape (`lift_form` failure), a CNF distribution blow-up
/// (`MAX_CLAUSES_PER_FORMULA`), or a clause over [`MAX_LITS_PER_CLAUSE`].
/// Tautology deletion and dedup are NOT loss (dropping a valid clause never
/// changes satisfiability).  The input-completeness gate uses this to
/// withhold confident Disproved/Satisfiable verdicts when an input formula
/// failed to load.
pub(crate) fn clausify_sentence_lossy(
    syn:    &SyntacticLayer,
    atoms:  &AtomTable,
    sent:   &Sentence,
    root:   SentenceId,
    negate: bool,
) -> (Vec<PClause>, bool) {
    let mut lctx = LiftCtx::new(root);
    let Some(lifted) = lift_form(syn, atoms, sent, &mut lctx) else { return (Vec::new(), true) };
    let f = if negate { Form::Not(Box::new(lifted)) } else { lifted };
    let units = std::mem::take(&mut lctx.units);
    // `negate` doubles as the skolem-namespace mode: goal skolems must
    // never collide with the axiom skolems of a content-identical root.
    let (out, lossy) = clausify_form(f, units, atoms, root, negate);
    if !lossy {
        // STRICTLY ADDITIVE trigger: a losslessly-clausified root takes
        // exactly the path it always took — the rescue below runs only
        // where the primary path just recorded a capacity loss.  (A
        // capped kappa comprehension is loss the rescue cannot repair —
        // the clauses themselves loaded fine, so no rescue either.)
        return (out, lctx.capped);
    }
    // Definitional-CNF rescue.  Re-lift (the store is immutable under
    // this call, so this reproduces the same tree — and the same kappa
    // units — the primary attempt consumed) and retry with
    // Plaisted–Greenbaum definitions; on any rescue bail, the primary
    // path's lossy result stands unchanged.
    let mut lctx = LiftCtx::new(root);
    let Some(lifted) = lift_form(syn, atoms, sent, &mut lctx) else { return (out, true) };
    let f = if negate { Form::Not(Box::new(lifted)) } else { lifted };
    defcnf_rescue(f, &lctx.units, atoms, root, negate)
        .map(|(o, l)| (o, l || lctx.capped))
        .unwrap_or((out, true))
}

/// Clausify the NEGATED conjunction of several stored roots — the
/// refutation form of a conjunctive conjecture.
///
/// The KIF ingest pipeline splits a top-level `(and A B)` query into
/// separate store roots, so negating each root independently asserts
/// `¬A ∧ ¬B` — the negation of the DISJUNCTION — and a refutation of a
/// single conjunct would "prove" the whole conjunction.  The negation
/// must wrap the rebuilt conjunction: `¬(A ∧ B) = ¬A ∨ ¬B`.
/// The same LOSS flag as [`clausify_sentence_lossy`] rides along: `true`
/// when any conjecture root failed to lift or any resulting clause was
/// dropped for capacity reasons — the refutation set is then missing goal
/// clauses, so a saturation over it certifies nothing.
pub(crate) fn clausify_negated_conjunction_lossy(
    syn:   &SyntacticLayer,
    atoms: &AtomTable,
    sents: &[(std::sync::Arc<Sentence>, SentenceId)],
) -> (Vec<PClause>, bool) {
    let Some(root) = sents.first().map(|(_, sid)| *sid) else { return (Vec::new(), false) };
    // Kappa comprehension units mint across ALL conjuncts into one
    // context (they attach to the shared skolem root), and are NOT
    // negated below: they are definitional axioms of the class terms the
    // conjecture mentions, not part of the goal.
    let lift_conj = |lctx: &mut LiftCtx| -> Option<Form> {
        let mut parts = Vec::with_capacity(sents.len());
        for (sent, _) in sents {
            parts.push(lift_form(syn, atoms, sent, lctx)?);
        }
        let conj = if parts.len() == 1 { parts.pop().unwrap() } else { Form::And(parts) };
        Some(Form::Not(Box::new(conj)))
    };
    let mut lctx = LiftCtx::new(root);
    let Some(f) = lift_conj(&mut lctx) else { return (Vec::new(), true) };
    let units = std::mem::take(&mut lctx.units);
    // Always goal mode: the root is the FIRST conjunct's content sid,
    // which a loaded KB axiom can share — the namespaces must not.
    let (out, lossy) = clausify_form(f, units, atoms, root, true);
    if !lossy {
        return (out, lctx.capped);
    }
    // Same rescue as `clausify_sentence_lossy` — a lift failure was
    // already returned above, so a lossy result here is a capacity loss
    // the definitional path may be able to repair.
    let mut lctx = LiftCtx::new(root);
    let Some(f) = lift_conj(&mut lctx) else { return (out, true) };
    defcnf_rescue(f, &lctx.units, atoms, root, true)
        .map(|(o, l)| (o, l || lctx.capped))
        .unwrap_or((out, true))
}

/// The shared pipeline below the negation decision: NNF, universal
/// closure of free variables, skolemization, distribution, dedup.
/// The second return is the LOSS flag (see [`clausify_sentence_lossy`]).
///
/// `kunits` are the KappaFn comprehension units minted while lifting
/// this root: lowered under the SAME per-root skolem context, in mint
/// order after the main formula (the defCNF auxiliary-clause pattern —
/// deterministic order ⇒ deterministic names).  A unit that blows the
/// distribution cap drops ALONE, recorded as loss; the main formula's
/// blow-up still fails the whole root (rescue path).
fn clausify_form(
    f:      Form,
    kunits: Vec<Form>,
    atoms:  &AtomTable,
    root:   SentenceId,
    goal:   bool,
) -> (Vec<PClause>, bool) {
    let mut ctx = SkolemCtx { root, goal, fresh_n: 0, sk_n: 0 };
    let Some(mut raw) = lower_form(f, &mut ctx, MAX_CLAUSES_PER_FORMULA) else {
        return (Vec::new(), true);
    };
    let mut lossy = false;
    for u in kunits {
        match lower_form(u, &mut ctx, MAX_CLAUSES_PER_FORMULA) {
            Some(r) => raw.extend(r),
            None    => lossy = true,
        }
    }
    let (out, l2) = filter_canonicalize(raw, atoms);
    (out, lossy || l2)
}

/// NNF, implicit universal closure of free variables, skolemization
/// (names drawn from the shared per-root `ctx`), and CNF distribution
/// under `cap`.  `None` = distribution blew the cap.
fn lower_form(
    f:   Form,
    ctx: &mut SkolemCtx,
    cap: usize,
) -> Option<Vec<Vec<(bool, Term)>>> {
    let f = nnf(elim(f), false);

    // Implicit universal closure over the (sorted) free variables.
    let mut fv = BTreeSet::new();
    freevars(&f, &BTreeSet::new(), &mut fv);
    let f = if fv.is_empty() {
        f
    } else {
        Form::ForAll(fv.into_iter().collect(), Box::new(f))
    };

    let f = skolemize(f, &[], &HashMap::new(), ctx);
    distribute(&f, cap)
}

/// The shared filtering tail: over-cap clause drop (the LOSS flag),
/// in-clause literal dedup, tautology deletion, canonicalization, and
/// clause-key dedup.
fn filter_canonicalize(
    raw:   Vec<Vec<(bool, Term)>>,
    atoms: &AtomTable,
) -> (Vec<PClause>, bool) {
    let mut lossy = false;
    let mut out: Vec<PClause> = Vec::with_capacity(raw.len());
    let mut seen_keys = std::collections::HashSet::new();
    'clauses: for cl in raw {
        if cl.len() > MAX_LITS_PER_CLAUSE { lossy = true; continue; }
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
    (out, lossy)
}

// -- Definitional CNF (Plaisted–Greenbaum) rescue path -------------------------
//
// The blow-up escape hatch.  The primary pipeline distributes ∨ over ∧
// naively; equivalence-rich / wide-DNF formulas exceed the caps, clauses
// drop, and the input-completeness gate (correctly) withholds
// Satisfiable/countermodel verdicts.  This path re-clausifies exactly
// those formulas with polarity-aware definitional CNF: subformulas whose
// distribution estimate exceeds the caps are replaced by fresh predicates
// `d(x̄)` over their free variables, with one-sided (polarity-aware)
// definition units added alongside.
//
// SOUNDNESS.  The definitional extension is equisatisfiability-preserving
// AND conservative in both directions:
//   * any model of the ORIGINAL extends to a model of the extension by
//     interpreting each `d(x̄)` as the subformula φ(x̄) it names (that
//     interpretation satisfies both definition directions, hence also
//     either one-sided unit); and
//   * any model of the EXTENSION restricts (drop the `d` symbols) to a
//     model of the original: at a positive occurrence the unit d→φ gives
//     d ≤ φ pointwise and the context is monotone in that position, so
//     replacing d back by φ preserves truth (dually for negative
//     occurrences with φ→d and anti-monotone contexts; both-polarity
//     occurrences carry both units).
// Refutations over the extension therefore remain sound, AND the strict
// Satisfiable/countermodel side remains sound — which is why the
// input-completeness gate counts a definitionally-clausified root as
// FULLY LOADED: the loss reasons this path replaces (CNF blow-up,
// over-cap clause) must not and do not fire on a successful rescue.
//
// DETERMINISM.  Definition predicate names are derived from
// (root sentence id, subformula path) — `df_<root_hex>_<path>` — not
// from any global counter, so re-clausifying the same root yields
// byte-identical clauses (the cache-eviction invariant skolem names
// already obey), and no cross-root ordering can perturb them.  The
// fresh symbols are interned through the ordinary `Symbol`/`AtomTable`
// machinery at clausification time (pre-loop), exactly like skolem
// symbols.

/// Process-wide rescue counters, surfaced by `SIGMA_STATS` (prove.rs)
/// when nonzero.  Cumulative over the process lifetime: clausification
/// runs inside cache generation, which has no per-run stats handle.
static DEFCNF_DEFINITIONS_INTRODUCED: AtomicU64 = AtomicU64::new(0);
static DEFCNF_ROOTS_RESCUED:          AtomicU64 = AtomicU64::new(0);

/// (definitions_introduced, roots_rescued) — process-cumulative.
pub(crate) fn defcnf_counters() -> (u64, u64) {
    (DEFCNF_DEFINITIONS_INTRODUCED.load(Ordering::Relaxed),
     DEFCNF_ROOTS_RESCUED.load(Ordering::Relaxed))
}

/// Occurrence polarity, tracked through the un-eliminated connectives
/// (`Not`/`Implies` antecedent flip, `Iff` makes both sides both-polar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pol { Pos, Neg, Both }

impl Pol {
    fn flip(self) -> Pol {
        match self {
            Pol::Pos  => Pol::Neg,
            Pol::Neg  => Pol::Pos,
            Pol::Both => Pol::Both,
        }
    }
}

/// Exact clause-count / max-clause-width estimate of what
/// `distribute(nnf(elim(f)))` would produce (pre-dedup, matching the
/// primary path's cap checks literal-for-literal), for one polarity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cost {
    /// Number of clauses (saturating).
    n: u64,
    /// Maximum clause width in literals (saturating).
    w: u64,
}

impl Cost {
    const ONE: Cost = Cost { n: 1, w: 1 };
    fn fits(self, b: Budget) -> bool { self.n <= b.n && self.w <= b.w }
}

/// A subformula's [`Cost`] at both polarities, computed in one walk
/// (`Iff` needs both sides of each child, so single-polarity recursion
/// would be exponential).
#[derive(Debug, Clone, Copy)]
struct CostPair {
    pos: Cost,
    neg: Cost,
}

/// Conjunctive composition: clause sets concatenate.
fn and2(a: Cost, b: Cost) -> Cost {
    Cost { n: a.n.saturating_add(b.n), w: a.w.max(b.w) }
}

/// Disjunctive composition: clause sets cross-product, widths add.
fn or2(a: Cost, b: Cost) -> Cost {
    Cost { n: a.n.saturating_mul(b.n), w: a.w.saturating_add(b.w) }
}

/// Estimate a formula's distribution cost at both polarities.  Mirrors
/// `elim`/`nnf`/`distribute` exactly:
///   * `Implies(a,b)`⁺ = `¬a ∨ b`;   `Implies(a,b)`⁻ = `a ∧ ¬b`
///   * `Iff(a,b)`⁺ = `(¬a∨b) ∧ (¬b∨a)`;  `Iff(a,b)`⁻ = `(a∧¬b) ∨ (b∧¬a)`
fn est(f: &Form) -> CostPair {
    match f {
        Form::Atom(_) => CostPair { pos: Cost::ONE, neg: Cost::ONE },
        Form::Not(g) => {
            let c = est(g);
            CostPair { pos: c.neg, neg: c.pos }
        }
        Form::And(fs) => fold_children(fs, /*and_is_pos=*/true),
        Form::Or(fs)  => fold_children(fs, /*and_is_pos=*/false),
        Form::Implies(a, b) => {
            let (ca, cb) = (est(a), est(b));
            CostPair {
                pos: or2(ca.neg, cb.pos),
                neg: and2(ca.pos, cb.neg),
            }
        }
        Form::Iff(a, b) => {
            let (ca, cb) = (est(a), est(b));
            CostPair {
                pos: and2(or2(ca.neg, cb.pos), or2(cb.neg, ca.pos)),
                neg: or2(and2(ca.pos, cb.neg), and2(cb.pos, ca.neg)),
            }
        }
        Form::ForAll(_, g) | Form::Exists(_, g) => est(g),
    }
}

/// `And` composes conjunctively at Pos and disjunctively at Neg
/// (de Morgan); `Or` mirrors.
fn fold_children(fs: &[Form], and_is_pos: bool) -> CostPair {
    let mut pos = if and_is_pos { Cost { n: 0, w: 0 } } else { Cost { n: 1, w: 0 } };
    let mut neg = if and_is_pos { Cost { n: 1, w: 0 } } else { Cost { n: 0, w: 0 } };
    for f in fs {
        let c = est(f);
        if and_is_pos {
            pos = and2(pos, c.pos);
            neg = or2(neg, c.neg);
        } else {
            pos = or2(pos, c.pos);
            neg = and2(neg, c.neg);
        }
    }
    // An empty conjunction/disjunction cannot arrive here (`lift_form`
    // never builds one), but keep the identities honest anyway.
    CostPair { pos, neg }
}

fn fits_at(pair: CostPair, pol: Pol, b: Budget) -> bool {
    match pol {
        Pol::Pos  => pair.pos.fits(b),
        Pol::Neg  => pair.neg.fits(b),
        Pol::Both => pair.pos.fits(b) && pair.neg.fits(b),
    }
}

/// The victim-selection size of a subformula at its occurrence polarity:
/// the componentwise-max over the polarities in play.
fn occ_size(pair: CostPair, pol: Pol) -> (u64, u64) {
    match pol {
        Pol::Pos  => (pair.pos.n, pair.pos.w),
        Pol::Neg  => (pair.neg.n, pair.neg.w),
        Pol::Both => (pair.pos.n.max(pair.neg.n), pair.pos.w.max(pair.neg.w)),
    }
}

#[derive(Debug, Clone, Copy)]
struct Budget {
    n: u64,
    w: u64,
}

const RESCUE_BUDGET: Budget = Budget {
    n: MAX_CLAUSES_PER_FORMULA as u64,
    w: MAX_LITS_PER_CLAUSE as u64,
};

/// One step of a definition's (root-relative) subformula path.
#[derive(Debug, Clone, Copy)]
enum Seg {
    /// Ordinary descent: the n-th argument of the connective.
    Child(u32),
    /// A synthesized chunk group (wide literal-only disjunctions are
    /// split into balanced groups; groups have no source position, so
    /// they get a per-node sequence number instead).
    Chunk(u32),
}

/// Rescue-shared state: the definition units introduced so far (in
/// deterministic introduction order) plus the naming context.
struct DefCtx {
    root:   SentenceId,
    /// Goal-mode namespace split — same rule as [`SkolemCtx::goal`]: an
    /// axiom and a conjecture sharing a content root must never share a
    /// definitional predicate.
    goal:   bool,
    units:  Vec<Form>,
    n_defs: u64,
    /// Defensive path-collision net — paths are unique by construction
    /// (each tree position is defined at most once; chunk groups carry
    /// per-node sequence numbers), but a collision must never silently
    /// CONFLATE two definitions, so names are checked and deterministically
    /// disambiguated anyway.
    used:   std::collections::HashSet<String>,
}

impl DefCtx {
    fn new(root: SentenceId, goal: bool) -> Self {
        DefCtx { root, goal, units: Vec::new(), n_defs: 0, used: std::collections::HashSet::new() }
    }

    /// `df_<root_hex>_<path>` (axiom) / `df_g<root_hex>_<path>` (goal) —
    /// deterministic, root-scoped, path-derived.
    fn fresh_name(&mut self, path: &[Seg]) -> String {
        use std::fmt::Write;
        let mut name = if self.goal {
            format!("df_g{:x}_", self.root)
        } else {
            format!("df_{:x}_", self.root)
        };
        for (i, seg) in path.iter().enumerate() {
            if i > 0 { name.push('_'); }
            match seg {
                Seg::Child(k) => { let _ = write!(name, "{k}"); }
                Seg::Chunk(k) => { let _ = write!(name, "c{k}"); }
            }
        }
        if !self.used.insert(name.clone()) {
            // Deterministic (traversal-ordered) disambiguator; expected
            // unreachable.
            let _ = write!(name, "_x{}", self.n_defs);
            self.used.insert(name.clone());
        }
        name
    }
}

/// Introduce a definition `d(x̄)` for `body` occurring at polarity `pol`,
/// returning the replacement atom.  `x̄` = the (sorted) free variables of
/// the body.  The definition unit(s) pushed onto `dc.units`:
///   * Pos:  `¬d(x̄) ∨ body`   (d → φ)
///   * Neg:  `¬body ∨ d(x̄)`   (φ → d)
///   * Both: both units.
/// The body is first (recursively) squeezed to fit one literal less than
/// the parent budget, so each unit itself distributes within the caps.
fn define(
    body: Form,
    pol:  Pol,
    b:    Budget,
    path: &mut Vec<Seg>,
    dc:   &mut DefCtx,
) -> Option<Form> {
    if b.w < 2 { return None; }
    let body = pg(body, pol, Budget { n: b.n, w: b.w - 1 }, path, dc)?;

    let mut fv = BTreeSet::new();
    freevars(&body, &BTreeSet::new(), &mut fv);
    let name = dc.fresh_name(path);
    let mut elems = Vec::with_capacity(1 + fv.len());
    elems.push(Term::Sym(Symbol::from(name)));
    elems.extend(fv.into_iter().map(Term::Var));
    let d = Term::App(elems);

    let datom = |d: &Term| Form::Atom(d.clone());
    match pol {
        Pol::Pos => dc.units.push(Form::Or(vec![
            Form::Not(Box::new(datom(&d))), body,
        ])),
        Pol::Neg => dc.units.push(Form::Or(vec![
            Form::Not(Box::new(body)), datom(&d),
        ])),
        Pol::Both => {
            dc.units.push(Form::Or(vec![
                Form::Not(Box::new(datom(&d))), body.clone(),
            ]));
            dc.units.push(Form::Or(vec![
                Form::Not(Box::new(body)), datom(&d),
            ]));
        }
    }
    dc.n_defs += 1;
    Some(Form::Atom(d))
}

/// Polarity-aware Plaisted–Greenbaum transformation: return `f` with
/// definitions introduced at the smallest subformulas that bring the
/// distribution estimate within `b` (children are repaired first, then
/// the node renames its cheapest-fixing children).  Guarantees on the
/// result at `pol`:
///   * max clause width ≤ `b.w` (always), and
///   * clause count ≤ `b.n` for every MULTIPLICATIVE composition
///     (∨-products).  Purely additive counts (a big conjunction's one
///     clause per conjunct) may exceed `b.n` — definitions cannot reduce
///     them, the output stays linear in the input, and the rescue-path
///     distribution cap (`DEFCNF_MAX_CLAUSES_PER_FORMULA`) bounds the
///     pathological end.
/// `None` = unfixable within the width floor; the caller abandons the
/// rescue (the primary path's lossy result stands).
fn pg(
    f:    Form,
    pol:  Pol,
    b:    Budget,
    path: &mut Vec<Seg>,
    dc:   &mut DefCtx,
) -> Option<Form> {
    if fits_at(est(&f), pol, b) {
        return Some(f);
    }
    match f {
        // An atom always fits any width ≥ 1; only reachable with a
        // degenerate budget.
        Form::Atom(_) => Some(f),
        Form::Not(g) => {
            path.push(Seg::Child(0));
            let g = pg(*g, pol.flip(), b, path, dc);
            path.pop();
            Some(Form::Not(Box::new(g?)))
        }
        Form::ForAll(vs, g) => {
            path.push(Seg::Child(0));
            let g = pg(*g, pol, b, path, dc);
            path.pop();
            Some(Form::ForAll(vs, Box::new(g?)))
        }
        Form::Exists(vs, g) => {
            path.push(Seg::Child(0));
            let g = pg(*g, pol, b, path, dc);
            path.pop();
            Some(Form::Exists(vs, Box::new(g?)))
        }
        Form::And(fs) => {
            let kids = pg_children(fs, pol, b, path, dc)?;
            // `And` composes multiplicatively at NEG (`¬(a∧b) = ¬a∨¬b`).
            let kids = if matches!(pol, Pol::Neg | Pol::Both) {
                fix_multiplicative(false, kids, pol, b, path, dc)?
            } else {
                kids
            };
            Some(Form::And(kids))
        }
        Form::Or(fs) => {
            let kids = pg_children(fs, pol, b, path, dc)?;
            let kids = if matches!(pol, Pol::Pos | Pol::Both) {
                fix_multiplicative(true, kids, pol, b, path, dc)?
            } else {
                kids
            };
            Some(Form::Or(kids))
        }
        Form::Implies(a, c) => {
            path.push(Seg::Child(0));
            let a2 = pg(*a, pol.flip(), b, path, dc);
            path.pop();
            let mut a2 = a2?;
            path.push(Seg::Child(1));
            let c2 = pg(*c, pol, b, path, dc);
            path.pop();
            let mut c2 = c2?;
            if matches!(pol, Pol::Pos | Pol::Both) {
                // Positive `=>` is a two-child ∨-product.
                loop {
                    let (ca, cc) = (est(&a2), est(&c2));
                    let node = CostPair {
                        pos: or2(ca.neg, cc.pos),
                        neg: and2(ca.pos, cc.neg),
                    };
                    if fits_at(node, pol, b) { break; }
                    let sa = occ_size(ca, pol.flip());
                    let sc = occ_size(cc, pol);
                    if sa >= sc && sa > (1, 1) {
                        path.push(Seg::Child(0));
                        let d = define(a2, pol.flip(), b, path, dc);
                        path.pop();
                        a2 = d?;
                    } else if sc > (1, 1) {
                        path.push(Seg::Child(1));
                        let d = define(c2, pol, b, path, dc);
                        path.pop();
                        c2 = d?;
                    } else {
                        return None; // two atoms yet over budget: width floor
                    }
                }
            }
            Some(Form::Implies(Box::new(a2), Box::new(c2)))
        }
        Form::Iff(a, c) => {
            // Both sides of `<=>` occur at BOTH polarities regardless of
            // the node's own polarity.
            path.push(Seg::Child(0));
            let a2 = pg(*a, Pol::Both, b, path, dc);
            path.pop();
            let mut a2 = a2?;
            path.push(Seg::Child(1));
            let c2 = pg(*c, Pol::Both, b, path, dc);
            path.pop();
            let mut c2 = c2?;
            loop {
                let (ca, cc) = (est(&a2), est(&c2));
                let node = CostPair {
                    pos: and2(or2(ca.neg, cc.pos), or2(cc.neg, ca.pos)),
                    neg: or2(and2(ca.pos, cc.neg), and2(cc.pos, ca.neg)),
                };
                if fits_at(node, pol, b) { break; }
                let sa = occ_size(ca, Pol::Both);
                let sc = occ_size(cc, Pol::Both);
                if sa >= sc && sa > (1, 1) {
                    path.push(Seg::Child(0));
                    let d = define(a2, Pol::Both, b, path, dc);
                    path.pop();
                    a2 = d?;
                } else if sc > (1, 1) {
                    path.push(Seg::Child(1));
                    let d = define(c2, Pol::Both, b, path, dc);
                    path.pop();
                    c2 = d?;
                } else {
                    return None;
                }
            }
            Some(Form::Iff(Box::new(a2), Box::new(c2)))
        }
    }
}

/// Repair every child at its (unchanged) occurrence polarity.
fn pg_children(
    fs:   Vec<Form>,
    pol:  Pol,
    b:    Budget,
    path: &mut Vec<Seg>,
    dc:   &mut DefCtx,
) -> Option<Vec<Form>> {
    let mut out = Vec::with_capacity(fs.len());
    for (i, f) in fs.into_iter().enumerate() {
        path.push(Seg::Child(i as u32));
        let g = pg(f, pol, b, path, dc);
        path.pop();
        out.push(g?);
    }
    Some(out)
}

/// Bring an n-ary ∨-product node (an `Or` seen positively, or an `And`
/// seen negatively — `or_node` distinguishes them for chunk-body
/// construction) within budget: repeatedly rename the child contributing
/// most to the product; when every child is literal-sized and only the
/// WIDTH still overflows, split the children into balanced defined
/// groups.  Children arrive already individually within budget.
fn fix_multiplicative(
    or_node:  bool,
    mut kids: Vec<Form>,
    pol:      Pol,
    b:        Budget,
    path:     &mut Vec<Seg>,
    dc:       &mut DefCtx,
) -> Option<Vec<Form>> {
    // The multiplicative side of each child's cost: `Or`⁺ multiplies the
    // POS costs, `And`⁻ multiplies the NEG costs.  (For a Both-polarity
    // node the same side is the constraining one — the additive side's
    // width is already bounded by the children's own budgets.)
    let side = |pair: CostPair| if or_node { pair.pos } else { pair.neg };
    let mut chunk_seq: u32 = 0;
    loop {
        let costs: Vec<Cost> = kids.iter().map(|k| side(est(k))).collect();
        let n = costs.iter().fold(1u64, |acc, c| acc.saturating_mul(c.n));
        let w = costs.iter().fold(0u64, |acc, c| acc.saturating_add(c.w));
        if n <= b.n && w <= b.w {
            return Some(kids);
        }
        // Victim: the non-literal child with the largest (n, w) — the
        // smallest set of definitions that fixes the estimate is reached
        // by collapsing the biggest product contributor first.  Strict
        // `>` keeps the FIRST of equals (deterministic).
        let mut victim: Option<usize> = None;
        for (i, c) in costs.iter().enumerate() {
            if (c.n > 1 || c.w > 1)
                && victim.is_none_or(|j| (c.n, c.w) > (costs[j].n, costs[j].w))
            {
                victim = Some(i);
            }
        }
        if let Some(i) = victim {
            let body = std::mem::replace(&mut kids[i], Form::And(Vec::new()));
            path.push(Seg::Child(i as u32));
            let d = define(body, pol, b, path, dc);
            path.pop();
            kids[i] = d?;
        } else {
            // Every child is literal-sized: pure width overflow.  Split
            // into balanced groups of (b.w - 1) literals, each behind a
            // definition — group bodies fit (b.n, b.w - 1) outright, so
            // no definition chains form.
            let group = b.w.saturating_sub(1) as usize;
            if group < 2 { return None; }
            let old = std::mem::take(&mut kids);
            let mut it = old.into_iter().peekable();
            while it.peek().is_some() {
                let chunk: Vec<Form> = it.by_ref().take(group).collect();
                if chunk.len() == 1 {
                    kids.extend(chunk);
                } else {
                    let body = if or_node { Form::Or(chunk) } else { Form::And(chunk) };
                    path.push(Seg::Chunk(chunk_seq));
                    chunk_seq += 1;
                    let d = define(body, pol, b, path, dc);
                    path.pop();
                    kids.push(d?);
                }
            }
        }
    }
}

/// Definitional-CNF rescue for a root whose primary clausification
/// recorded a capacity loss.  `None` = rescue abandoned (width floor or
/// the rescue-path distribution cap) — the caller keeps the primary
/// path's lossy result, so this path is strictly additive: it can only
/// turn a lossy load into a lossless one, never perturb a lossless load.
///
/// `kunits` (KappaFn comprehension units, re-minted by the caller's
/// re-lift) are lowered with the PRIMARY-path caps and filtered
/// separately at the end: their capacity losses are honest input loss —
/// they would have failed the primary path identically — not estimator
/// defects, so they ride the returned LOSS flag instead of tripping the
/// rescue-width assert.
fn defcnf_rescue(
    f:      Form,
    kunits: &[Form],
    atoms:  &AtomTable,
    root:   SentenceId,
    goal:   bool,
) -> Option<(Vec<PClause>, bool)> {
    let mut dc = DefCtx::new(root, goal);
    let mut path = Vec::new();
    let main = pg(f, Pol::Pos, RESCUE_BUDGET, &mut path, &mut dc)?;
    debug_assert!(path.is_empty());

    // One skolem context across every unit: distinct existentials in
    // distinct units must never share a skolem symbol.  Unit order is
    // deterministic (main first, then definitions in introduction
    // order, then kappa units in mint order), so the names are too.
    let mut ctx = SkolemCtx { root, goal, fresh_n: 0, sk_n: 0 };
    let mut raw = lower_form(main, &mut ctx, DEFCNF_MAX_CLAUSES_PER_FORMULA)?;
    for unit in dc.units {
        raw.extend(lower_form(unit, &mut ctx, DEFCNF_MAX_CLAUSES_PER_FORMULA)?);
    }

    let (mut out, lossy) = filter_canonicalize(raw, atoms);
    if lossy {
        // The estimator guarantees width ≤ MAX_LITS_PER_CLAUSE; reaching
        // this means an estimator defect — fall back to the honest lossy
        // result rather than report a clean load.
        debug_assert!(false, "defcnf rescue produced an over-cap clause");
        return None;
    }

    let mut klossy = false;
    let mut kraw = Vec::new();
    for u in kunits {
        match lower_form(u.clone(), &mut ctx, MAX_CLAUSES_PER_FORMULA) {
            Some(r) => kraw.extend(r),
            None    => klossy = true,
        }
    }
    let (kout, kl2) = filter_canonicalize(kraw, atoms);
    out.extend(kout);

    DEFCNF_ROOTS_RESCUED.fetch_add(1, Ordering::Relaxed);
    DEFCNF_DEFINITIONS_INTRODUCED.fetch_add(dc.n_defs, Ordering::Relaxed);
    Some((out, klossy || kl2))
}
