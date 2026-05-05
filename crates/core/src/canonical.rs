// crates/core/src/canonical.rs
// #[cfg(feature = "cnf")]
//
// Canonical hashing of CNF clauses.
//
// A clause's canonical hash is invariant under three equivalences that
// ordinary fingerprinting of the element tree would miss:
//
// 1. Variable renaming -- `p(?X) | q(?X)` and `p(?Y) | q(?Y)` are the
//    same clause.  All variables are renamed by first-occurrence DFS to
//    `V0..Vn`, and only the normalised names participate in the hash.
// 2. Skolem renaming -- Vampire invents skolem names (`sK0`, `sK7_3`)
//    whose concrete indices depend on the clausifier's internal state
//    across a session.  We rename them by first-occurrence DFS to
//    `sk0..skn`, matching the variable scheme.
// 3. Literal ordering -- a clause is a *set* of literals; a serialisation
//    order of `[p, q]` vs `[q, p]` must hash equivalently.  We order
//    literals by a structural total (sub-)hash, then stringify for tie-
//    break stability.
// 4. Equality is unordered -- `l = r` and `r = l` are the same literal.
//    The two sides are sorted before hashing.
//
// Sort annotations are **ignored** -- the stored `CnfTerm`s from
// `cnf2.rs` are already sort-agnostic (a TFF and an FOF clausification
// of the same formula should dedup to a single stored clause).  If a
// sort-aware auxiliary hash becomes necessary for safety, see Risk 3
// in the design plan.
//
// Formula-level fingerprinting reduces to hashing a *sorted* `[ClauseId]`
// vector -- two formulas that produce the same clause set (per the rules
// above) share the same `formula_hash`.

use std::collections::HashMap;

use xxhash_rust::xxh64::Xxh64;

use crate::types::{Clause, CnfLiteral, CnfTerm, SymbolId};

// =========================================================================
//  Discriminant bytes
// =========================================================================
//
// Per-variant tag bytes so the hash stream is unambiguous across the
// `CnfTerm` shape.  Kept in sync with the legacy fingerprint.rs scheme
// but with a disjoint range so any cross-module mistakes surface as
// obvious hash-mismatch.

const D_CONST:      u8 = 0x11;
const D_VAR:        u8 = 0x12;
const D_FN:         u8 = 0x13;
const D_SKOLEM:     u8 = 0x14;
const D_NUM:        u8 = 0x15;
const D_STR:        u8 = 0x16;

const D_LIT_POS:    u8 = 0x20;
const D_LIT_NEG:    u8 = 0x21;
const D_LIT_EQ_POS: u8 = 0x22;
const D_LIT_EQ_NEG: u8 = 0x23;

const D_SEP:        u8 = 0x00;

const EQUALITY_PRED_ID: SymbolId = u64::MAX;

// =========================================================================
//  Rename context
// =========================================================================

/// State carried through canonical-hash computation.
///
/// Variables and skolems are each renamed by first-occurrence DFS index.
/// They live in separate namespaces so the same integer (e.g. 0) doesn't
/// collide between a variable and a skolem in the hash stream — the tag
/// byte already distinguishes the variant, but segregating the counters
/// keeps diagnostic dumps readable.
struct RenameCtx {
    vars:    HashMap<SymbolId, u32>,
    var_n:   u32,
    skolems: HashMap<SymbolId, u32>,
    skol_n:  u32,
}

impl RenameCtx {
    fn new() -> Self {
        Self {
            vars:    HashMap::new(),
            var_n:   0,
            skolems: HashMap::new(),
            skol_n:  0,
        }
    }

    fn var(&mut self, id: SymbolId) -> u32 {
        *self.vars.entry(id).or_insert_with(|| {
            let n = self.var_n;
            self.var_n += 1;
            n
        })
    }

    fn skolem(&mut self, id: SymbolId) -> u32 {
        *self.skolems.entry(id).or_insert_with(|| {
            let n = self.skol_n;
            self.skol_n += 1;
            n
        })
    }
}

// =========================================================================
//  Term / literal hashing
// =========================================================================

fn hash_term(h: &mut Xxh64, ctx: &mut RenameCtx, t: &CnfTerm) {
    match t {
        CnfTerm::Const(id) => {
            h.update(&[D_CONST]);
            h.update(&id.to_le_bytes());
            h.update(&[D_SEP]);
        }
        CnfTerm::Var(id) => {
            let idx = ctx.var(*id);
            h.update(&[D_VAR]);
            h.update(&idx.to_le_bytes());
            h.update(&[D_SEP]);
        }
        CnfTerm::Fn { id, args } => {
            h.update(&[D_FN]);
            h.update(&id.to_le_bytes());
            h.update(&[D_SEP]);
            h.update(&(args.len() as u32).to_le_bytes());
            for a in args {
                hash_term(h, ctx, a);
            }
        }
        CnfTerm::SkolemFn { id, args } => {
            let idx = ctx.skolem(*id);
            h.update(&[D_SKOLEM]);
            h.update(&idx.to_le_bytes());
            h.update(&[D_SEP]);
            h.update(&(args.len() as u32).to_le_bytes());
            for a in args {
                hash_term(h, ctx, a);
            }
        }
        CnfTerm::Num(s) => {
            h.update(&[D_NUM]);
            h.update(s.as_bytes());
            h.update(&[D_SEP]);
        }
        CnfTerm::Str(s) => {
            h.update(&[D_STR]);
            h.update(s.as_bytes());
            h.update(&[D_SEP]);
        }
    }
}

/// Compute a stable pre-hash for one literal.
///
/// Same `RenameCtx` is used across all literals of the containing clause
/// so variable identity is preserved *across* literals (`p(?X) & q(?X)`
/// must hash differently from `p(?X) & q(?Y)`).
///
/// Equality is handled specially: the `lhs` and `rhs` are sorted by their
/// own sub-hash before being fed to the main stream.  This is what makes
/// `(equal a b)` and `(equal b a)` dedup to the same canonical form.
fn literal_prehash(ctx: &mut RenameCtx, lit: &CnfLiteral) -> u64 {
    let mut h = Xxh64::new(0);
    let is_equality = matches!(&lit.pred,
        CnfTerm::Const(id) if *id == EQUALITY_PRED_ID);

    if is_equality && lit.args.len() == 2 {
        // Fork into two scratch ctxs so the symmetric orientation
        // doesn't pollute the live variable numbering -- we then replay
        // the winning order into the real ctx.
        let tag = if lit.positive { D_LIT_EQ_POS } else { D_LIT_EQ_NEG };
        h.update(&[tag]);

        let l_hash = sub_term_hash(ctx, &lit.args[0]);
        let r_hash = sub_term_hash(ctx, &lit.args[1]);

        let (first, second) = if l_hash <= r_hash {
            (&lit.args[0], &lit.args[1])
        } else {
            (&lit.args[1], &lit.args[0])
        };

        hash_term(&mut h, ctx, first);
        hash_term(&mut h, ctx, second);
    } else {
        let tag = if lit.positive { D_LIT_POS } else { D_LIT_NEG };
        h.update(&[tag]);
        hash_term(&mut h, ctx, &lit.pred);
        h.update(&(lit.args.len() as u32).to_le_bytes());
        for a in &lit.args {
            hash_term(&mut h, ctx, a);
        }
    }
    h.digest()
}

/// Compute a term's structural hash in isolation, using a scratch rename
/// context.  Used to total-order equality sides; the scratch ctx is
/// discarded so variable numbering seen through this call does not leak
/// into the main hash stream.
fn sub_term_hash(ctx: &RenameCtx, t: &CnfTerm) -> u64 {
    // Fork the rename state so we can preview a hash without committing.
    let mut scratch = RenameCtx {
        vars:    ctx.vars.clone(),
        var_n:   ctx.var_n,
        skolems: ctx.skolems.clone(),
        skol_n:  ctx.skol_n,
    };
    let mut h = Xxh64::new(0);
    hash_term(&mut h, &mut scratch, t);
    h.digest()
}

// =========================================================================
//  Public API
// =========================================================================

/// Canonical 64-bit hash for `clause`.
///
/// Two clauses that differ only by variable renaming, skolem renaming,
/// literal ordering, or equality-side orientation produce the same hash.
/// Sort annotations are not considered.
pub(crate) fn canonical_clause_hash(clause: &Clause) -> u64 {
    let mut ctx = RenameCtx::new();

    // Compute literal pre-hashes in a first pass so they sort the clause.
    // Because literals may bind new variables, we need a deterministic
    // order.  We sort by (pre-hash, stringified form) so ties break on a
    // total order.  The pre-hash pass uses a *detached* context so the
    // var-counter assigned during sort doesn't leak into the final hash.
    let pre: Vec<(u64, usize)> = clause
        .literals
        .iter()
        .enumerate()
        .map(|(i, lit)| {
            let mut scratch = RenameCtx::new();
            (literal_prehash(&mut scratch, lit), i)
        })
        .collect();

    let mut order: Vec<usize> = (0..clause.literals.len()).collect();
    order.sort_by_key(|&i| pre[i].0);

    // Now emit into the live hasher in sorted literal order, using the
    // live RenameCtx so cross-literal variable identity is preserved.
    let mut hasher = Xxh64::new(0);
    hasher.update(&(clause.literals.len() as u32).to_le_bytes());
    for i in order {
        let lit = &clause.literals[i];
        let h = literal_prehash(&mut ctx, lit);
        hasher.update(&h.to_le_bytes());
    }

    hasher.digest()
}

/// Formula-level fingerprint derived from a clause-id multiset.
///
/// The caller interns each clause against the canonical-hash table, yielding
/// a stable `ClauseId`.  Sorting those ids before hashing makes the
/// formula hash order-insensitive: `(and A B) == (and B A)` after
/// clausification because the resulting clause sets are equal.
pub(crate) fn formula_hash_from_clauses(clause_ids: &[u64]) -> u64 {
    let mut ids: Vec<u64> = clause_ids.to_vec();
    ids.sort_unstable();
    let mut h = Xxh64::new(0);
    h.update(&(ids.len() as u32).to_le_bytes());
    for id in ids {
        h.update(&id.to_le_bytes());
    }
    h.digest()
}

// =========================================================================
//  Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn lit_pos(pred_id: SymbolId, args: Vec<CnfTerm>) -> CnfLiteral {
        CnfLiteral {
            positive: true,
            pred:     CnfTerm::Const(pred_id),
            args,
        }
    }

    fn lit_neg(pred_id: SymbolId, args: Vec<CnfTerm>) -> CnfLiteral {
        CnfLiteral {
            positive: false,
            pred:     CnfTerm::Const(pred_id),
            args,
        }
    }

    fn eq_lit(positive: bool, lhs: CnfTerm, rhs: CnfTerm) -> CnfLiteral {
        CnfLiteral {
            positive,
            pred: CnfTerm::Const(EQUALITY_PRED_ID),
            args: vec![lhs, rhs],
        }
    }

    #[test]
    fn variable_rename_is_invariant() {
        // p(?X) | q(?X)  -- renaming ?X to ?Y must give same hash.
        let c1 = Clause {
            literals: vec![
                lit_pos(1, vec![CnfTerm::Var(100)]),
                lit_pos(2, vec![CnfTerm::Var(100)]),
            ],
        };
        let c2 = Clause {
            literals: vec![
                lit_pos(1, vec![CnfTerm::Var(200)]),
                lit_pos(2, vec![CnfTerm::Var(200)]),
            ],
        };
        assert_eq!(canonical_clause_hash(&c1), canonical_clause_hash(&c2));
    }

    #[test]
    fn variable_identity_preserved_across_literals() {
        // p(?X) | q(?X)  vs  p(?X) | q(?Y) -- different clauses.
        let same_var = Clause {
            literals: vec![
                lit_pos(1, vec![CnfTerm::Var(100)]),
                lit_pos(2, vec![CnfTerm::Var(100)]),
            ],
        };
        let diff_var = Clause {
            literals: vec![
                lit_pos(1, vec![CnfTerm::Var(100)]),
                lit_pos(2, vec![CnfTerm::Var(200)]),
            ],
        };
        assert_ne!(canonical_clause_hash(&same_var), canonical_clause_hash(&diff_var));
    }

    #[test]
    fn literal_order_is_invariant() {
        // Clause is a set: [p(a), q(a)] == [q(a), p(a)].
        let c1 = Clause {
            literals: vec![
                lit_pos(1, vec![CnfTerm::Const(10)]),
                lit_pos(2, vec![CnfTerm::Const(10)]),
            ],
        };
        let c2 = Clause {
            literals: vec![
                lit_pos(2, vec![CnfTerm::Const(10)]),
                lit_pos(1, vec![CnfTerm::Const(10)]),
            ],
        };
        assert_eq!(canonical_clause_hash(&c1), canonical_clause_hash(&c2));
    }

    #[test]
    fn polarity_matters() {
        // p(a)  vs  ~p(a)
        let c1 = Clause { literals: vec![lit_pos(1, vec![CnfTerm::Const(10)])] };
        let c2 = Clause { literals: vec![lit_neg(1, vec![CnfTerm::Const(10)])] };
        assert_ne!(canonical_clause_hash(&c1), canonical_clause_hash(&c2));
    }

    #[test]
    fn skolem_position_is_canonical() {
        // Two clauses with skolem ids swapped should hash the same as long
        // as they appear in the same *positions* after DFS renaming.
        let c1 = Clause {
            literals: vec![lit_pos(1, vec![
                CnfTerm::SkolemFn { id: 500, args: vec![] },
                CnfTerm::SkolemFn { id: 600, args: vec![] },
            ])],
        };
        let c2 = Clause {
            literals: vec![lit_pos(1, vec![
                CnfTerm::SkolemFn { id: 700, args: vec![] },
                CnfTerm::SkolemFn { id: 800, args: vec![] },
            ])],
        };
        assert_eq!(canonical_clause_hash(&c1), canonical_clause_hash(&c2));
    }

    #[test]
    fn skolem_identity_matters() {
        // sK(a, a)  vs  sK(a, b) -- latter has two different skolems.
        let c1 = Clause {
            literals: vec![lit_pos(1, vec![
                CnfTerm::SkolemFn { id: 500, args: vec![] },
                CnfTerm::SkolemFn { id: 500, args: vec![] },
            ])],
        };
        let c2 = Clause {
            literals: vec![lit_pos(1, vec![
                CnfTerm::SkolemFn { id: 500, args: vec![] },
                CnfTerm::SkolemFn { id: 600, args: vec![] },
            ])],
        };
        assert_ne!(canonical_clause_hash(&c1), canonical_clause_hash(&c2));
    }

    #[test]
    fn equality_is_unordered() {
        let a = CnfTerm::Const(10);
        let b = CnfTerm::Const(20);
        let c1 = Clause { literals: vec![eq_lit(true, a.clone(), b.clone())] };
        let c2 = Clause { literals: vec![eq_lit(true, b, a)] };
        assert_eq!(canonical_clause_hash(&c1), canonical_clause_hash(&c2));
    }

    #[test]
    fn equality_polarity_matters() {
        let a = CnfTerm::Const(10);
        let b = CnfTerm::Const(20);
        let c1 = Clause { literals: vec![eq_lit(true,  a.clone(), b.clone())] };
        let c2 = Clause { literals: vec![eq_lit(false, a, b)] };
        assert_ne!(canonical_clause_hash(&c1), canonical_clause_hash(&c2));
    }

    #[test]
    fn formula_hash_is_order_insensitive() {
        let ids1: Vec<u64> = vec![100, 200, 300];
        let ids2: Vec<u64> = vec![300, 100, 200];
        assert_eq!(
            formula_hash_from_clauses(&ids1),
            formula_hash_from_clauses(&ids2),
        );
    }

    #[test]
    fn formula_hash_distinguishes_cardinality() {
        let ids1: Vec<u64> = vec![100, 200];
        let ids2: Vec<u64> = vec![100, 200, 200]; // duplicate clause id
        assert_ne!(
            formula_hash_from_clauses(&ids1),
            formula_hash_from_clauses(&ids2),
        );
    }

    #[test]
    fn formula_hash_distinguishes_content() {
        let ids1: Vec<u64> = vec![100, 200];
        let ids2: Vec<u64> = vec![100, 300];
        assert_ne!(
            formula_hash_from_clauses(&ids1),
            formula_hash_from_clauses(&ids2),
        );
    }
}
