// crates/core/src/saturate/canon.rs
//
// Canonical clause form (port of the prototype `canonical` + the
// cnf/canonical.rs conventions, over prover Terms):
//
// 1. Equality-side sort — `(= a b)` and `(= b a)` are the same literal;
//    sides are ordered by variable-blind structural key.
// 2. Literal ordering — a clause is a SET of literals; they are sorted
//    by (polarity, variable-blind structural key) so serialisation
//    order cannot split one clause into two identities.
// 3. First-occurrence variable rename — walking the literals in sorted
//    order, variables become `V0..Vn`; α-equivalent clauses collapse
//    onto identical atom Sentences and hence identical `ClauseKey`s.
//
// The blanked key deliberately ignores variable *identity* (every
// variable renders as `?`), matching the prototype's `blank` sort key:
// identity is then reintroduced consistently by the rename pass.

use std::collections::HashMap;
use std::fmt::Write;

use smallvec::SmallVec;

use xxhash_rust::xxh64::Xxh64;

use crate::parse::OpKind;
use crate::types::{Literal, Symbol, SymbolId};

use super::clause::{AtomTable, ClauseKey, PClause, PLit, Term};

/// Seed for [`ClauseKey`] hashing — distinct from the sentence content
/// hash and AST fingerprint seeds (separate keyspace, separate stream).
const CLAUSE_SEED: u64 = 0xC1A0_5EED_C1A0_5EED;

/// Canonicalize raw signed literals into a [`PClause`], interning each
/// atom into `atoms`.  See module docs for the three normalisations.
pub(crate) fn canonical_clause(mut lits: Vec<(bool, Term)>, atoms: &AtomTable) -> PClause {
    // 1. Orient equality literals.
    for (_, t) in lits.iter_mut() {
        orient_equality(t);
    }

    // 2. Sort literals by (polarity, blanked structural key).
    lits.sort_by_cached_key(|(pos, t)| (*pos, blank_key(t)));

    // 3. First-occurrence rename over the sorted order.
    let mut map: super::hash64::Map64<SymbolId, SymbolId> = Default::default();
    let mut plits: SmallVec<[PLit; 4]> = SmallVec::with_capacity(lits.len());
    let mut key_hash = Xxh64::new(CLAUSE_SEED);
    for (pos, t) in &lits {
        let renamed = rename(t, &mut map);
        let atom = atoms.intern_atom(&renamed);
        key_hash.update(&[*pos as u8]);
        key_hash.update(&atom.to_be_bytes());
        plits.push(PLit { pos: *pos, atom });
    }

    PClause {
        key:   ClauseKey(key_hash.digest()),
        lits:  plits,
        nvars: map.len() as u32,
    }
}

/// Canonical variable for rename slot `k`: name `V<k>`, id the name's
/// content hash — the same id every time, KB-independent, so canonical
/// atoms are content-addressed across clauses and sessions.
pub(crate) fn canonical_var(k: usize) -> SymbolId {
    Symbol::hash_name(&format!("V{}", k))
}

/// Highest canonical slot with a precomputed reverse mapping.  A clause
/// would need >256 distinct variables to exceed it — far past every
/// practical guard (`MAX_LITS_PER_CLAUSE` × realistic atom widths).
const MAX_CANON_SLOTS: usize = 256;

/// Inverse of [`canonical_var`]: the slot `k` for a canonical variable
/// id, or `None` for ids outside the canonical family.  Backed by a
/// process-wide table (the ids are pure name hashes, KB-independent).
pub(crate) fn canonical_slot(id: SymbolId) -> Option<u32> {
    use std::sync::OnceLock;
    static REVERSE: OnceLock<HashMap<SymbolId, u32>> = OnceLock::new();
    REVERSE
        .get_or_init(|| {
            (0..MAX_CANON_SLOTS)
                .map(|k| (canonical_var(k), k as u32))
                .collect()
        })
        .get(&id)
        .copied()
}

fn rename(t: &Term, map: &mut super::hash64::Map64<SymbolId, SymbolId>) -> Term {
    match t {
        Term::Var(v) => {
            let next = map.len();
            Term::Var(*map.entry(*v).or_insert_with(|| canonical_var(next)))
        }
        Term::App(elems) => Term::App(elems.iter().map(|e| rename(e, map)).collect()),
        _ => t.clone(),
    }
}

/// Sort the two sides of an equality atom by blanked key, in place.
fn orient_equality(t: &mut Term) {
    let Term::App(elems) = t else { return };
    if elems.len() == 3 && matches!(elems[0], Term::Op(OpKind::Equal)) {
        if blank_key(&elems[1]) > blank_key(&elems[2]) {
            elems.swap(1, 2);
        }
    }
}

/// Variable-blind structural rendering — the literal/equality sort key.
/// Total order via the string; every variable renders identically (`?`)
/// so α-variants of a literal sort identically (the prototype's
/// `str(blank(atom))`).  Also the argument order for symmetric-relation
/// orientation (prover.rs) — any consistent total order works; sharing
/// the equality-orientation key keeps the two normalizations aligned.
pub(crate) fn blank_key(t: &Term) -> String {
    let mut s = String::new();
    blank_into(t, &mut s);
    s
}

fn blank_into(t: &Term, out: &mut String) {
    match t {
        Term::Var(_) => out.push('?'),
        Term::Sym(sym) => {
            out.push('s');
            out.push_str(&sym.name());
        }
        Term::Lit(Literal::Str(v)) => { out.push('t'); out.push_str(v); }
        Term::Lit(Literal::Number(v)) => { out.push('n'); out.push_str(v); }
        Term::Op(op) => { let _ = write!(out, "o{:?}", op); }
        Term::App(elems) => {
            out.push('(');
            for e in elems {
                blank_into(e, out);
                out.push(' ');
            }
            out.push(')');
        }
    }
}
