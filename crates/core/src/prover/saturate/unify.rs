// crates/core/src/saturate/unify.rs
//
// Unification — the *verify* step behind every index probe (prototype
// §4) — plus one-way matching for unit subsumption.
//
// Inference works over slot-variable [`Term`]s: a clause's canonical
// variables (`V0..Vn`, ids = name hashes) are renumbered to dense slot
// ints `offset..offset+nvars` when the clause's atoms are lifted out of
// the `AtomTable` ([`slot_atom`]).  Rename-apart between two clauses is
// then just a slot offset, and a substitution is a flat `Vec` indexed
// by slot — no string tags, no hash-map walk on the hot path.

use crate::syntactic::SyntacticLayer;
use crate::types::SymbolId;

use super::canon::canonical_slot;
use super::clause::{AtomId, AtomTable, Term};

/// A substitution over slot variables: `s[slot]` is the bound term
/// (itself in slot-variable form), or `None` while unbound.
pub(crate) type Subst = Vec<Option<Term>>;

/// Lift `atom` into a slot-variable [`Term`], mapping each canonical
/// variable `Vk` to slot `offset + k`.  Returns `None` if the atom is
/// unresolvable or carries a non-canonical variable (every atom that
/// goes through `canonical_clause` is canonical by construction).
pub(crate) fn slot_atom(
    atoms:  &AtomTable,
    syn:    &SyntacticLayer,
    atom:   AtomId,
    offset: u32,
) -> Option<Term> {
    let t = atoms.term_of(atom, syn)?;
    reslot(&t, offset)
}

fn reslot(t: &Term, offset: u32) -> Option<Term> {
    Some(match t {
        Term::Var(id) => Term::Var(u64::from(canonical_slot(*id)? + offset)),
        Term::App(elems) => {
            let mut out = Vec::with_capacity(elems.len());
            for e in elems {
                out.push(reslot(e, offset)?);
            }
            Term::App(out)
        }
        other => other.clone(),
    })
}

/// Chase a variable through the substitution to its representative.
pub(crate) fn walk<'a>(mut t: &'a Term, s: &'a Subst) -> &'a Term {
    while let Term::Var(v) = t {
        match s.get(*v as usize).and_then(Option::as_ref) {
            Some(next) => t = next,
            None => break,
        }
    }
    t
}

/// Rename-apart by slot offset, materialized.  Bindings in a `Subst`
/// live in ABSOLUTE slot space, so a term viewed under an offset must
/// be shifted before it can be stored — but only the bound fragment,
/// at bind time, never the whole clause up front (see [`unify_off`]).
pub(crate) fn shift_slots(t: &Term, off: u64) -> Term {
    match t {
        Term::Var(v) => Term::Var(v + off),
        Term::App(elems) => Term::App(elems.iter().map(|e| shift_slots(e, off)).collect()),
        _ => t.clone(),
    }
}

/// Offset-aware walk: chase `(term, offset)` views through `s`.
/// Stored bindings are absolute, so walking into one resets the
/// offset to zero.
fn walk_off<'a>(mut t: &'a Term, mut off: u64, s: &'a Subst) -> (&'a Term, u64) {
    while let Term::Var(v) = t {
        match s.get((*v + off) as usize).and_then(Option::as_ref) {
            Some(next) => {
                t = next;
                off = 0;
            }
            None => break,
        }
    }
    (t, off)
}

fn occurs_off(slot: u64, t: &Term, off: u64, s: &Subst) -> bool {
    let (t, off) = walk_off(t, off, s);
    match t {
        Term::Var(u) => *u + off == slot,
        Term::App(elems) => elems.iter().any(|e| occurs_off(slot, e, off, s)),
        _ => false,
    }
}

/// Unify `a` and `b` under `s`, extending it in place.  On failure `s`
/// is rolled back to its entry state (the `trail` records bindings).
/// Slots must be pre-allocated (`s.len()` covers both clauses' vars).
pub(crate) fn unify(a: &Term, b: &Term, s: &mut Subst) -> bool {
    unify_off(a, 0, b, 0, s)
}

/// Unify with VIRTUAL rename-apart: `a`'s and `b`'s slot variables are
/// interpreted at their respective offsets, so neither term needs a
/// shifted copy before the attempt (the old path materialized the
/// partner literal per attempt — hundreds of thousands of tree clones
/// per run, wasted on every mismatch).  Only fragments actually BOUND
/// get shifted, at bind time.
pub(crate) fn unify_off(a: &Term, ao: u64, b: &Term, bo: u64, s: &mut Subst) -> bool {
    let mut trail: Vec<usize> = Vec::new();
    if unify_off_inner(a, ao, b, bo, s, &mut trail) {
        true
    } else {
        for slot in trail {
            s[slot] = None;
        }
        false
    }
}

fn unify_off_inner(
    a: &Term, ao: u64,
    b: &Term, bo: u64,
    s: &mut Subst,
    trail: &mut Vec<usize>,
) -> bool {
    // Bound variables resolve by cloning the bound FRAGMENT (usually a
    // constant or small term) — a borrow into `s` cannot live across
    // the recursion's `&mut s`.  The structural spine of both input
    // terms is never cloned; the old implementation cloned both walked
    // sides at every recursion level.
    if let Term::Var(v) = a {
        let slot = (*v + ao) as usize;
        if let Some(bound) = s[slot].clone() {
            return unify_off_inner(&bound, 0, b, bo, s, trail);
        }
        // `a` is an unbound variable; resolve `b` far enough to bind.
        if let Term::Var(u) = b {
            let bslot = (*u + bo) as usize;
            if let Some(bound) = s[bslot].clone() {
                return unify_off_inner(a, ao, &bound, 0, s, trail);
            }
            if slot == bslot {
                return true; // same absolute variable
            }
        }
        return bind_off(slot as u64, b, bo, s, trail);
    }
    if let Term::Var(u) = b {
        let bslot = (*u + bo) as usize;
        if let Some(bound) = s[bslot].clone() {
            return unify_off_inner(a, ao, &bound, 0, s, trail);
        }
        return bind_off(bslot as u64, a, ao, s, trail);
    }
    match (a, b) {
        (Term::App(xs), Term::App(ys)) if xs.len() == ys.len() => {
            xs.iter().zip(ys).all(|(x, y)| unify_off_inner(x, ao, y, bo, s, trail))
        }
        // Ground leaves (Sym/Lit/Op) and shape mismatches.
        _ => a == b,
    }
}

fn bind_off(slot: u64, t: &Term, toff: u64, s: &mut Subst, trail: &mut Vec<usize>) -> bool {
    if occurs_off(slot, t, toff, s) {
        return false;
    }
    let i = slot as usize;
    debug_assert!(s[i].is_none(), "walk left an unbound representative");
    // Bindings are stored ABSOLUTE: shift the bound fragment (and only
    // the fragment) when it comes from the offset side.
    s[i] = Some(if toff == 0 { t.clone() } else { shift_slots(t, toff) });
    trail.push(i);
    true
}

/// Deep-apply `s` to `t` — the resolvent constructor.
pub(crate) fn apply(t: &Term, s: &Subst) -> Term {
    apply_off(t, 0, s)
}

/// Deep-apply with a virtual slot offset on `t` (see [`unify_off`]):
/// unbound variables surface at their absolute slot, bound ones expand
/// through `s`.  Replaces `apply(&shift_slots(t, off), s)` without the
/// intermediate shifted tree.
pub(crate) fn apply_off(t: &Term, off: u64, s: &Subst) -> Term {
    let (t, off) = walk_off(t, off, s);
    match t {
        Term::Var(v) => Term::Var(v + off),
        Term::App(elems) => Term::App(elems.iter().map(|e| apply_off(e, off, s)).collect()),
        other => other.clone(),
    }
}

/// One-way match: bind only the *pattern's* variables; the target is
/// treated as fixed (its variables match nothing but themselves).
/// Caller must rename the two apart (disjoint slot ranges).  Rolls `s`
/// back on failure, like [`unify`].
pub(crate) fn match_one_way(p: &Term, t: &Term, s: &mut Subst) -> bool {
    let mut trail: Vec<usize> = Vec::new();
    if match_inner(p, t, s, &mut trail) {
        true
    } else {
        for slot in trail {
            s[slot] = None;
        }
        false
    }
}

fn match_inner(p: &Term, t: &Term, s: &mut Subst, trail: &mut Vec<usize>) -> bool {
    if let Term::Var(v) = p {
        let slot = *v as usize;
        return match &s[slot] {
            Some(bound) => bound == t,
            None => {
                s[slot] = Some(t.clone());
                trail.push(slot);
                true
            }
        };
    }
    match (p, t) {
        (Term::App(xs), Term::App(ys)) if xs.len() == ys.len() => {
            xs.iter().zip(ys).all(|(x, y)| match_inner(x, y, s, trail))
        }
        _ => p == t,
    }
}

/// Distinct slot variables in `t` — `nvars` recovery for derived terms.
pub(crate) fn term_slots(t: &Term, out: &mut std::collections::BTreeSet<u64>) {
    match t {
        Term::Var(v) => { out.insert(*v); }
        Term::App(elems) => {
            for e in elems { term_slots(e, out); }
        }
        _ => {}
    }
}

/// Convenience: the canonical-variable id for slot `k` *relative to a
/// zero offset* — the inverse of [`slot_atom`]'s mapping, used when a
/// derived (slot-form) term is fed back through `canonical_clause`.
#[allow(dead_code)] // exercised by the prover loop in the next phase
pub(crate) fn slot_var_id(k: u32) -> SymbolId {
    super::canon::canonical_var(k as usize)
}
