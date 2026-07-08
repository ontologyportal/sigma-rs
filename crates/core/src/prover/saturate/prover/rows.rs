// crates/core/src/prover/saturate/prover/rows.rs
//
// k-channel Vandermonde rows + erasure-decode retrieval (phase 2 of the
// subterm-index milestone, construction A).  Every stored subterm gets a
// 4-word GF(2^64) row; a non-ground demodulator lhs compiles once into a
// per-node linear-system plan, and each bucket candidate is then filtered
// by field arithmetic + hash probes BEFORE the structural verify — the
// verify (`match_one_way_off` at the posted occurrence) is NEVER removed,
// the chain is a pure necessary-condition prefilter.
//
// Row of a compound `f(c1..cn)` (children = argument seats 1..n-1; the
// head contributes the tag, not a seat):
//
//     R_j = tag_j(f, n)  XOR_i  alpha_{f,i}^j (x) K(c_i)      j = 0..3
//
//   * `alpha_{f,i}` is derived JOINTLY from the head's content identity
//     and the position index (one PRF image per (symbol, position) pair —
//     the phase-0 commutative-cancellation discipline documented at
//     `fingerprint::coin`: a symbol-independent per-position element
//     would make permuted/commuted carryless products collide as a
//     FAMILY, not at the generic 2^-64 rate).
//   * `K(c)` is the child's content key used DIRECTLY as the linear atom
//     — never re-hashed per channel.  Nonlinear per-channel hashing
//     would destroy the cross-channel coupling the decode inverts.
//   * Channel multipliers are the powers alpha^0..alpha^3 (transposed
//     Vandermonde): any v distinct seat elements give a nonsingular
//     v x v system for v <= 4, deterministically.
//   * Leaves (and shapeless-headed compounds) get per-channel tag rows —
//     presence markers for the registered-term probe, never decoded into.
//   * Variables in STORED terms enter as canonical-blank coins keyed by
//     the clause's first-occurrence slot (`blank_key`): blanks behave as
//     constants under one-way matching.  Decoded blank keys are only
//     ever compared WITHIN one candidate clause's evaluation (the
//     binding table is cleared per candidate) — never joined across
//     candidate clauses, where equal slots mean unrelated variables.
//
// Child keys `K(c)` (must agree byte-for-byte between registration and
// pattern compile — see `pattern_ground_key` / the postings walk):
//
//   * symbol leaf        -> `Symbol::id()` (the exact-postings keyspace);
//   * string / number    -> `xxh64(bytes, 'T' / 'N')` (the fingerprint
//     leaf-key streams);
//   * operator leaf      -> `u64::from(op_tag)` (the head-bucket stream);
//   * compound           -> the `ElementHasher` content key (ground: ==
//     `TermFactsTable::ground_key_facts` == `intern_atom`; open: the
//     slot-form content id, variables hashing as canonical blanks —
//     `slot_atom_content_id` semantics);
//   * variable           -> `blank_key(slot)`.
//
// Query plan (matching direction only): the demodulator lhs is walked
// top-down once per backward pass.  Ground child positions fold into a
// per-channel skeleton constant; open positions become unknowns.  With
// v unknowns at a node (v <= k-1 = 3 — ALWAYS at least one surplus check
// row; at v == k every swept candidate would pay a full decode before
// rejection and the economics invert), the compiler picks v pivot
// channels (greedy in channel order — the first v channels whenever they
// are independent, matching the rows-0..v-1 construction; a repeated
// variable collapses its occurrences into ONE unknown whose channel-0
// coefficient may vanish, in which case the greedy selection shifts down
// one row) and precomputes the inverse; the remaining channels are
// surplus checks.  v >= 4, a rank-deficient system, or an unindexable
// shape marks the node FALLBACK: no algebra there, the structural verify
// carries that subtree (phase-1 behavior).
//
// Repeated variables ACROSS nodes: the FIRST site (in evaluation order:
// a node binds its variable unknowns before descending into subpattern
// children, parents before children, siblings left to right) decodes and
// binds the key; every later site is compiled CLOSED — its seat powers
// multiply the bound key into the delta at query time, shrinking v and
// strengthening the checks.  This is what rejects f(a, g(b)) against the
// pattern f(X, g(X)) without a walk: the g-node becomes a pure check
// (v = 0) against X's binding.
//
// Nothing here allocates per candidate: the plan, binding table and
// trail live on the prover and are reused across queries.

use smallvec::SmallVec;
use xxhash_rust::xxh64::xxh64;

use crate::gf64;
use crate::types::Literal;

use super::super::clause::Term;
use super::super::hash64::Map64;
use super::super::kbo::KboOrdering;
use super::super::terms::TermFactsTable;
use super::super::units::op_tag;

/// One 4-channel row.
pub(crate) type Row = [u64; 4];

/// Seed for the seat elements `alpha_{f,i}` — hashed jointly over
/// (head content key, position index).  Own keyspace.
const ROW_SEAT_SEED: u64 = 0xA1FA_5EA7_A1FA_5EA7;
/// Seed for the per-channel compound tags `tag_j(f, n)`.
const ROW_TAG_SEED: u64 = 0x7A67_C0DE_7A67_C0DE;
/// Seed for per-channel leaf/presence tag rows.
const ROW_LEAF_SEED: u64 = 0x1EAF_7A65_1EAF_7A65;
/// Seed for canonical-blank keys (stored-variable slots).
const ROW_BLANK_SEED: u64 = 0xB1A2_1C01_B1A2_1C01;

/// The seat element `alpha_{f,i}` — a NONZERO field element derived
/// jointly from the head's content identity and the position index
/// (see the module docs on why jointly).
#[inline]
pub(crate) fn seat_elem(head: u64, pos: usize) -> u64 {
    let mut buf = [0u8; 12];
    buf[..8].copy_from_slice(&head.to_be_bytes());
    buf[8..].copy_from_slice(&(pos as u32).to_be_bytes());
    let a = xxh64(&buf, ROW_SEAT_SEED);
    if a == 0 { 1 } else { a }
}

/// `[alpha^0, alpha^1, alpha^2, alpha^3]` — the per-channel multipliers
/// of one seat.
#[inline]
pub(crate) fn seat_powers(a: u64) -> Row {
    let a2 = gf64::sq(a);
    [1, a, a2, gf64::mul(a2, a)]
}

/// Per-channel compound tag `tag_j(f, n)` — (head, len) hashed jointly.
#[inline]
pub(crate) fn node_tags(head: u64, len: usize) -> Row {
    let mut buf = [0u8; 13];
    buf[..8].copy_from_slice(&head.to_be_bytes());
    buf[8..12].copy_from_slice(&(len as u32).to_be_bytes());
    let mut r = [0u64; 4];
    for (j, w) in r.iter_mut().enumerate() {
        buf[12] = j as u8;
        *w = xxh64(&buf, ROW_TAG_SEED);
    }
    r
}

/// Per-channel presence row of a leaf (or shapeless-headed compound) —
/// a registered-term marker keyed by content, never decoded into.
#[inline]
pub(crate) fn leaf_row(key: u64) -> Row {
    let mut buf = [0u8; 9];
    buf[..8].copy_from_slice(&key.to_be_bytes());
    let mut r = [0u64; 4];
    for (j, w) in r.iter_mut().enumerate() {
        buf[8] = j as u8;
        *w = xxh64(&buf, ROW_LEAF_SEED);
    }
    r
}

/// Canonical-blank key of a stored variable slot (first-occurrence
/// renaming is already the stored form; blanks act as constants under
/// matching).  NEVER joined across candidate clauses — see module docs.
#[inline]
pub(crate) fn blank_key(slot: u64) -> u64 {
    xxh64(&slot.to_be_bytes(), ROW_BLANK_SEED)
}

/// The row-key of a string/number literal leaf — the fingerprint leaf
/// streams (`'T'` / `'N'`), disjoint from symbol ids and content hashes.
#[inline]
pub(crate) fn lit_key(l: &Literal) -> u64 {
    match l {
        Literal::Str(v) => xxh64(v.as_bytes(), u64::from(b'T')),
        Literal::Number(v) => xxh64(v.as_bytes(), u64::from(b'N')),
    }
}

/// XOR one child's contribution into a row under construction:
/// `row[j] ^= alpha^j (x) key` — the child key is the LINEAR atom, used
/// directly (channel 0 multiplier is 1).
#[inline]
pub(crate) fn accum_child(row: &mut Row, alpha: u64, key: u64) {
    let a2 = gf64::sq(alpha);
    row[0] ^= key;
    row[1] ^= gf64::mul(alpha, key);
    row[2] ^= gf64::mul(a2, key);
    row[3] ^= gf64::mul(gf64::mul(a2, alpha), key);
}

/// `row[j] ^= powers[j] (x) key` for a general (possibly summed) power
/// vector — the query-side twin of [`accum_child`], where a collapsed
/// or runtime-closed seat's channel-0 coefficient can be 0.
#[inline]
fn accum_powers(row: &mut Row, powers: &Row, key: u64) {
    for j in 0..4 {
        if powers[j] != 0 {
            row[j] ^= gf64::mul(powers[j], key);
        }
    }
}

/// What one decoded unknown IS in the pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PUnknown {
    /// A pattern variable slot — bind (check-and-set) after decode.
    Var(u32),
    /// An open subpattern with a concrete head — fetch the decoded
    /// child's row and descend into the referenced plan node.
    Sub(u32),
    /// An open subpattern without a concrete head (variable-/compound-
    /// headed): probe-only — its key is checked as a registered term,
    /// its interior is left to the structural verify.
    Opaque,
}

/// Compile-time unknown source (before subpattern nodes exist).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnkSrc {
    Var(u32),
    Sub(u32), // element index of the subpattern child
    Opaque,
}

/// One compiled pattern node: the linear system of one lhs level.
#[derive(Debug, Clone, Default)]
struct PNode {
    /// tag XOR ground-closed child contributions, per channel.
    skel: Row,
    /// Runtime-closed seats: (binding slot, summed seat powers) — the
    /// bound key is multiplied into the delta per candidate (the
    /// cross-level substitution step).
    closed: SmallVec<[(u32, Row); 2]>,
    /// The v unknowns, in seat order (v <= 3 unless `fallback`).
    unknowns: SmallVec<[PUnknown; 3]>,
    /// Full 4 x v coefficient matrix: `coef[j][u]` = summed alpha^j of
    /// unknown `u`'s occurrences.  Rows >= v of the pivot selection are
    /// the surplus checks.
    coef: [[u64; 3]; 4],
    /// The v pivot channels (greedy in channel order).
    pivot: [u8; 3],
    /// Inverse of the pivot v x v submatrix: `k[u] = XOR_i inv[u][i] (x)
    /// delta[pivot[i]]`.
    inv: [[u64; 3]; 3],
    /// Bit j set ⇔ channel j is a surplus check row (not a pivot).
    check_mask: u8,
    /// No algebra at this node (v >= 4 / rank-deficient / unindexable):
    /// the structural verify carries this subtree — phase-1 behavior.
    fallback: bool,
    /// Whether the delta folds in bound-variable keys (used to classify
    /// a check failure as a BINDING reject rather than a plain surplus
    /// reject in the counters).
    has_closed: bool,
}

/// Why a candidate was rejected by the decode chain (counter split).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reject {
    /// A surplus check row failed on purely structural content.
    Surplus,
    /// A decoded key is not a registered term (row-table probe miss).
    Probe,
    /// A check involving a bound variable's substituted key failed, or
    /// a decoded key contradicted an existing binding.
    Binding,
}

/// A compiled demodulator-lhs plan: nodes[0] is the root, children are
/// referenced by index.  Reused across queries (`compile_into` clears).
#[derive(Debug, Clone, Default)]
pub(crate) struct PatternPlan {
    nodes: Vec<PNode>,
    /// Some node fell back (v >= 4 / singular / unindexable shape).
    pub(crate) fallback_any: bool,
    /// Some node imposes an algebraic constraint the depth-1 seat
    /// prefilter cannot see (ground/bound closures, collapsed repeats,
    /// v == 0 checks, or subpattern descent).  A plan with none is
    /// TRIVIAL: the chain cannot reject anything the seat prefilter
    /// passed, so running it would be pure overhead.
    constraining: bool,
}

impl PatternPlan {
    /// Whether the decode chain should run at all for this pattern.
    #[inline]
    pub(crate) fn active(&self) -> bool {
        !self.nodes.is_empty() && !self.nodes[0].fallback && self.constraining
    }

    /// Whether the ROOT node is a fallback (chain unusable outright).
    #[inline]
    pub(crate) fn root_fallback(&self) -> bool {
        self.nodes.first().is_some_and(|n| n.fallback)
    }

    /// Trivial = compiled fine but constraint-free (see `constraining`).
    #[inline]
    pub(crate) fn trivial(&self) -> bool {
        !self.nodes.is_empty() && !self.root_fallback() && !self.constraining
    }

    /// Evaluate the plan against one candidate's root row.  `bind` must
    /// be sized to the pattern's slot space and all-`None` on entry;
    /// slots bound during evaluation are appended to `trail` (the
    /// caller rolls back per candidate — bindings NEVER survive across
    /// candidate clauses).  `Ok` means "may match — verify structurally";
    /// `Err` is a sound rejection.
    pub(crate) fn eval(
        &self,
        row: &Row,
        tab: &Map64<u64, Row>,
        bind: &mut [Option<u64>],
        trail: &mut Vec<u32>,
    ) -> Result<(), Reject> {
        self.eval_node(0, row, tab, bind, trail)
    }

    fn eval_node(
        &self,
        ni: usize,
        row: &Row,
        tab: &Map64<u64, Row>,
        bind: &mut [Option<u64>],
        trail: &mut Vec<u32>,
    ) -> Result<(), Reject> {
        let nd = &self.nodes[ni];
        if nd.fallback {
            // No algebra here: the structural verify carries the subtree.
            return Ok(());
        }
        // Delta: candidate row minus skeleton minus substituted bindings.
        let mut delta = [
            row[0] ^ nd.skel[0],
            row[1] ^ nd.skel[1],
            row[2] ^ nd.skel[2],
            row[3] ^ nd.skel[3],
        ];
        for (slot, powers) in &nd.closed {
            let Some(b) = bind[*slot as usize] else {
                debug_assert!(false, "compile order guarantees closed slots are bound");
                return Ok(());
            };
            accum_powers(&mut delta, powers, b);
        }
        let on_check_fail = if nd.has_closed { Reject::Binding } else { Reject::Surplus };
        let v = nd.unknowns.len();
        if v == 0 {
            // Fully closed node: all four channels are checks.
            if delta != [0u64; 4] {
                return Err(on_check_fail);
            }
            return Ok(());
        }
        // Solve the pivot v x v system for the unknown keys.
        let mut k = [0u64; 3];
        for (u, ku) in k.iter_mut().take(v).enumerate() {
            let mut acc = 0u64;
            for i in 0..v {
                let d = delta[nd.pivot[i] as usize];
                let c = nd.inv[u][i];
                if c != 0 && d != 0 {
                    acc ^= gf64::mul(c, d);
                }
            }
            *ku = acc;
        }
        // Surplus checks (>= 1 by construction: v <= 3 < k = 4).
        for j in 0..4 {
            if nd.check_mask & (1 << j) == 0 {
                continue;
            }
            let mut acc = 0u64;
            for u in 0..v {
                let c = nd.coef[j][u];
                if c != 0 && k[u] != 0 {
                    acc ^= gf64::mul(c, k[u]);
                }
            }
            if acc != delta[j] {
                return Err(on_check_fail);
            }
        }
        // Registered-term probes: every decoded key must be a stored
        // subterm; subpattern positions keep the fetched child row.
        let mut child_rows: [Option<&Row>; 3] = [None; 3];
        for u in 0..v {
            match tab.get(&k[u]) {
                Some(r) => child_rows[u] = Some(r),
                None => return Err(Reject::Probe),
            }
        }
        // Bind ALL variable unknowns first (check-and-set), then descend
        // subpatterns — matching the compile-time closure order, so a
        // sibling subpattern always sees this node's bindings.
        for (u, unk) in nd.unknowns.iter().enumerate() {
            if let PUnknown::Var(slot) = unk {
                match bind[*slot as usize] {
                    Some(b) if b != k[u] => return Err(Reject::Binding),
                    Some(_) => {}
                    None => {
                        bind[*slot as usize] = Some(k[u]);
                        trail.push(*slot);
                    }
                }
            }
        }
        for (u, unk) in nd.unknowns.iter().enumerate() {
            if let PUnknown::Sub(ci) = unk {
                let crow = child_rows[u].expect("probed above");
                self.eval_node(*ci as usize, crow, tab, bind, trail)?;
            }
        }
        Ok(())
    }
}

/// The head key of a compound's element list, in the (head, len) bucket
/// keyspace — `None` for variable-/compound-/literal-headed shapes.
/// Shared with the subsumption equality-join channel (`ej.rs`), whose
/// literal classification must agree with the compiler's.
#[inline]
pub(crate) fn head_of(elems: &[Term]) -> Option<u64> {
    match elems.first() {
        Some(Term::Sym(s)) => Some(s.id()),
        Some(Term::Op(op)) => Some(u64::from(op_tag(op))),
        _ => None,
    }
}

/// The row-key of a GROUND pattern child — byte-identical to the
/// registration walk's derivation for the same term (see module docs).
fn pattern_ground_key(t: &Term, facts: &TermFactsTable, kbo: &KboOrdering) -> Option<u64> {
    match t {
        Term::Sym(s) => Some(s.id()),
        Term::Lit(l) => Some(lit_key(l)),
        Term::Op(op) => Some(u64::from(op_tag(op))),
        Term::App(_) => facts.ground_key_facts(t, kbo).map(|(k, _)| k),
        Term::Var(_) => None,
    }
}

/// Compile the demodulator lhs `l` (an `App` with a concrete head,
/// non-ground — the head-bucket query surface) into `plan`, reusing its
/// storage.  Once per backward pass.
pub(crate) fn compile_into(
    plan: &mut PatternPlan,
    l: &Term,
    facts: &TermFactsTable,
    kbo: &KboOrdering,
) {
    plan.nodes.clear();
    plan.fallback_any = false;
    plan.constraining = false;
    let mut bound: SmallVec<[u32; 8]> = SmallVec::new();
    let elems = match l {
        Term::App(e) => e.as_slice(),
        _ => &[],
    };
    let Some(head) = head_of(elems) else {
        // Unindexable lhs shape — `DemodIndex::add` never produces one;
        // defensive root fallback.
        plan.nodes.push(PNode { fallback: true, ..PNode::default() });
        plan.fallback_any = true;
        return;
    };
    let root = comp_node(plan, elems, head, facts, kbo, &mut bound);
    debug_assert_eq!(root, 0, "root compiles first");
    debug_assert!(
        plan.nodes[0].fallback || !plan.nodes[0].unknowns.is_empty(),
        "a non-ground lhs root always has at least one open seat",
    );
}

/// Compile one pattern node (recursing into open subpattern children
/// AFTER this node's variable unknowns are marked bound — the exact
/// order `eval_node` replays).  Returns the node's index.
fn comp_node(
    plan: &mut PatternPlan,
    elems: &[Term],
    head: u64,
    facts: &TermFactsTable,
    kbo: &KboOrdering,
    bound: &mut SmallVec<[u32; 8]>,
) -> u32 {
    let n = elems.len();
    let mut skel = node_tags(head, n);
    let mut closed: SmallVec<[(u32, Row); 2]> = SmallVec::new();
    let mut srcs: SmallVec<[UnkSrc; 4]> = SmallVec::new();
    let mut cols: SmallVec<[Row; 4]> = SmallVec::new();
    let mut ground_closed = false;
    let mut collapsed = false;
    let mut has_sub = false;
    let mut fallback = false;
    for (i, e) in elems.iter().enumerate().skip(1) {
        let powers = seat_powers(seat_elem(head, i));
        if e.is_ground() {
            // Closed at compile time: fold the known key into the skeleton.
            let Some(key) = pattern_ground_key(e, facts, kbo) else {
                fallback = true; // defensive — ground children always key
                break;
            };
            accum_powers(&mut skel, &powers, key);
            ground_closed = true;
            continue;
        }
        match e {
            Term::Var(vslot) => {
                let slot = *vslot as u32;
                if bound.contains(&slot) {
                    // Runtime-closed: an earlier site binds this slot;
                    // substitute its key into this system per candidate.
                    match closed.iter_mut().find(|(s, _)| *s == slot) {
                        Some((_, pw)) => {
                            for j in 0..4 {
                                pw[j] ^= powers[j];
                            }
                        }
                        None => closed.push((slot, powers)),
                    }
                } else if let Some(u) =
                    srcs.iter().position(|s| *s == UnkSrc::Var(slot))
                {
                    // Same-node repeat: collapse linearly into ONE
                    // unknown with summed seat multipliers.
                    for j in 0..4 {
                        cols[u][j] ^= powers[j];
                    }
                    collapsed = true;
                } else {
                    srcs.push(UnkSrc::Var(slot));
                    cols.push(powers);
                }
            }
            Term::App(ce) => {
                if head_of(ce).is_some() {
                    srcs.push(UnkSrc::Sub(i as u32));
                    has_sub = true;
                } else {
                    srcs.push(UnkSrc::Opaque);
                }
                cols.push(powers);
            }
            // Non-ground non-Var non-App cannot exist (leaves are ground).
            _ => unreachable!("open leaf"),
        }
        if srcs.len() > 3 {
            // v >= 4: no surplus row would remain (k = 4) — the
            // economics invert; fall back to phase-1 behavior here.
            fallback = true;
            break;
        }
    }
    let v = srcs.len();
    let mut node = PNode {
        skel,
        has_closed: !closed.is_empty(),
        closed,
        check_mask: 0x0F,
        ..PNode::default()
    };
    if !fallback && v > 0 {
        for (u, col) in cols.iter().enumerate() {
            for j in 0..4 {
                node.coef[j][u] = col[j];
            }
        }
        match solve_plan(&node.coef, v) {
            Some((pivot, inv, check_mask)) => {
                node.pivot = pivot;
                node.inv = inv;
                node.check_mask = check_mask;
            }
            // Rank-deficient (e.g. a collapsed unknown whose summed
            // multipliers vanish): no decode at this node.
            None => fallback = true,
        }
    }
    node.fallback = fallback;
    if fallback {
        plan.fallback_any = true;
    } else {
        // A node constrains beyond the depth-1 seat prefilter when it
        // closes seats (ground or bound), collapses repeats, is a pure
        // check, or descends into subpatterns.
        plan.constraining |=
            ground_closed || node.has_closed || collapsed || v == 0 || has_sub;
    }
    let idx = plan.nodes.len() as u32;
    plan.nodes.push(node);
    if fallback {
        // Vars under a fallback node are NOT bound here; a later
        // non-fallback site becomes their binder.  Children are
        // unreachable (no decoded keys to descend on) — skip them.
        return idx;
    }
    // Bind all variable unknowns BEFORE compiling subpattern children,
    // in seat order — the order eval_node binds at runtime.
    for src in &srcs {
        if let UnkSrc::Var(slot) = src {
            if !bound.contains(slot) {
                bound.push(*slot);
            }
        }
    }
    for src in &srcs {
        let unk = match src {
            UnkSrc::Var(slot) => PUnknown::Var(*slot),
            UnkSrc::Opaque => PUnknown::Opaque,
            UnkSrc::Sub(pos) => {
                let Term::App(ce) = &elems[*pos as usize] else {
                    unreachable!("Sub sources are Apps")
                };
                let chead = head_of(ce).expect("Sub sources have concrete heads");
                PUnknown::Sub(comp_node(plan, ce, chead, facts, kbo, bound))
            }
        };
        plan.nodes[idx as usize].unknowns.push(unk);
    }
    idx
}

/// Pick v pivot channels (greedy in channel order — deterministic, and
/// exactly channels 0..v-1 whenever those are independent) and invert
/// the pivot submatrix.  Returns (pivot rows, inverse, check mask) or
/// `None` when the system is rank-deficient over the 4 channels.
fn solve_plan(coef: &[[u64; 3]; 4], v: usize) -> Option<([u8; 3], [[u64; 3]; 3], u8)> {
    debug_assert!((1..=3).contains(&v));
    // Greedy rank-building: keep channel j if it is independent of the
    // channels kept so far (Gaussian elimination over GF(2^64)).
    let mut pivot = [0u8; 3];
    let mut basis: [[u64; 3]; 3] = [[0; 3]; 3]; // reduced kept rows
    let mut basis_pc: [usize; 3] = [usize::MAX; 3]; // pivot column of each
    let mut cnt = 0usize;
    for j in 0..4 {
        if cnt == v {
            break;
        }
        let mut r = coef[j];
        // Reduce against the kept rows.
        for b in 0..cnt {
            let pc = basis_pc[b];
            if r[pc] != 0 {
                let f = r[pc];
                for c in 0..v {
                    r[c] ^= gf64::mul(f, basis[b][c]);
                }
            }
        }
        let Some(pc) = (0..v).find(|&c| r[c] != 0) else {
            continue; // dependent row — a surplus check, not a pivot
        };
        // Normalize so the pivot column is 1 (cheaper later reductions).
        let f = gf64::inv(r[pc]);
        for c in 0..v {
            r[c] = gf64::mul(r[c], f);
        }
        basis[cnt] = r;
        basis_pc[cnt] = pc;
        pivot[cnt] = j as u8;
        cnt += 1;
    }
    if cnt < v {
        return None;
    }
    // Invert the v x v pivot submatrix M (rows = chosen channels) by
    // Gauss-Jordan on [M | I] in GF(2^64).
    let mut a = [[0u64; 3]; 3];
    let mut inv = [[0u64; 3]; 3];
    for i in 0..v {
        a[i][..v].copy_from_slice(&coef[pivot[i] as usize][..v]);
        inv[i][i] = 1;
    }
    for col in 0..v {
        let p = (col..v).find(|&r| a[r][col] != 0)?; // full rank: found
        a.swap(col, p);
        inv.swap(col, p);
        let f = gf64::inv(a[col][col]);
        for c in 0..v {
            a[col][c] = gf64::mul(a[col][c], f);
            inv[col][c] = gf64::mul(inv[col][c], f);
        }
        for r in 0..v {
            if r != col && a[r][col] != 0 {
                let g = a[r][col];
                for c in 0..v {
                    a[r][c] ^= gf64::mul(g, a[col][c]);
                    inv[r][c] ^= gf64::mul(g, inv[col][c]);
                }
            }
        }
    }
    let mut check_mask = 0x0Fu8;
    for &p in &pivot[..v] {
        check_mask &= !(1 << p);
    }
    debug_assert!(check_mask != 0, "v <= 3 always leaves a surplus check row");
    Some((pivot, inv, check_mask))
}

#[cfg(test)]
mod tests {
    use super::super::postings::{head_lhs_key, SubtermPostings};
    use super::*;
    use crate::types::Symbol;

    fn sym(n: &str) -> Term {
        Term::Sym(Symbol::from(n))
    }
    fn app(v: Vec<Term>) -> Term {
        Term::App(v)
    }
    fn var(s: u64) -> Term {
        Term::Var(s)
    }

    /// Register `(p <cand>)` as clause `cid`, compile `pat`, and run the
    /// chain against the candidate's bucket row.  Returns the eval
    /// verdict plus the decoded bindings (by slot).
    fn run_chain(
        pat: &Term,
        cand: &Term,
        nslots: usize,
    ) -> (Result<(), Reject>, Vec<Option<u64>>, PatternPlan) {
        let facts = TermFactsTable::default();
        let kbo = KboOrdering::new();
        let mut po = SubtermPostings::default();
        po.register_clause(1, &[(true, app(vec![sym("p"), cand.clone()]))], &facts, &kbo, true);
        let (h, ar) = head_lhs_key(pat).expect("pattern is a concrete-headed App");
        let (posts, rows) = po.head_postings(h, ar);
        assert_eq!(posts.len(), 1, "candidate occurrence bucketed");
        let mut plan = PatternPlan::default();
        compile_into(&mut plan, pat, &facts, &kbo);
        let mut bind: Vec<Option<u64>> = vec![None; nslots];
        let mut trail: Vec<u32> = Vec::new();
        let r = plan.eval(&rows[0], po.row_table(), &mut bind, &mut trail);
        (r, bind, plan)
    }

    // Vandermonde solve roundtrip for v = 1..3: the decoded unknown keys
    // must be exactly the candidate children's registration keys, across
    // a spread of (pseudo-random) child symbols.
    #[test]
    fn vandermonde_roundtrip_v1_to_v3() {
        for v in 1..=3usize {
            for salt in 0..8u64 {
                // Pattern (f ?0 .. ?v-1 kX) with one ground anchor seat;
                // candidate instantiates each slot with a distinct symbol.
                let anchor = format!("k{salt}");
                let mut pat = vec![sym("f")];
                let mut cand = vec![sym("f")];
                let mut want = Vec::new();
                for u in 0..v {
                    pat.push(var(u as u64));
                    let name = format!("c{}_{}", u, salt.wrapping_mul(0x9E37_79B9));
                    want.push(Symbol::from(name.as_str()).id());
                    cand.push(sym(&name));
                }
                pat.push(sym(&anchor));
                cand.push(sym(&anchor));
                let (r, bind, plan) =
                    run_chain(&app(pat), &app(cand), v);
                assert!(plan.active(), "anchored pattern is constraining");
                assert_eq!(r, Ok(()), "true instance must survive (v={v})");
                for (u, w) in want.iter().enumerate() {
                    assert_eq!(bind[u], Some(*w), "decoded key at slot {u} (v={v})");
                }
            }
        }
    }

    // A corrupted candidate row must die on the surplus checks — the
    // whole point of keeping >= 1 check row at every v.
    #[test]
    fn surplus_check_rejects_corrupted_rows() {
        let facts = TermFactsTable::default();
        let kbo = KboOrdering::new();
        let mut po = SubtermPostings::default();
        let cand = app(vec![sym("f"), sym("a"), sym("b"), sym("k")]);
        po.register_clause(1, &[(true, app(vec![sym("p"), cand]))], &facts, &kbo, true);
        let pat = app(vec![sym("f"), var(0), var(1), sym("k")]);
        let (h, ar) = head_lhs_key(&pat).unwrap();
        let (_, rows) = po.head_postings(h, ar);
        let mut plan = PatternPlan::default();
        compile_into(&mut plan, &pat, &facts, &kbo);
        let mut bind: Vec<Option<u64>> = vec![None; 2];
        let mut trail: Vec<u32> = Vec::new();
        // Channel 3 is never a pivot for a fresh-vars system (greedy
        // picks 0..v-1): corrupting it must trip a SURPLUS reject.
        let mut bad = rows[0];
        bad[3] ^= 1;
        assert_eq!(
            plan.eval(&bad, po.row_table(), &mut bind, &mut trail),
            Err(Reject::Surplus),
        );
        // And the pristine row still passes (sanity).
        for &s in &trail {
            bind[s as usize] = None;
        }
        trail.clear();
        assert_eq!(plan.eval(&rows[0], po.row_table(), &mut bind, &mut trail), Ok(()));
    }

    // Same-node repeated variable: occurrences collapse into ONE unknown
    // with summed multipliers (channel 0 vanishes — the pivot selection
    // must shift down a row), accepting f(a,a) and rejecting f(a,b).
    #[test]
    fn repeated_variable_collapse_same_node() {
        let pat = app(vec![sym("f"), var(0), var(0)]);
        let good = app(vec![sym("f"), sym("a"), sym("a")]);
        let bad = app(vec![sym("f"), sym("a"), sym("b")]);
        let (r, bind, plan) = run_chain(&pat, &good, 1);
        assert!(plan.active(), "collapse is constraining");
        assert_eq!(r, Ok(()));
        assert_eq!(bind[0], Some(Symbol::from("a").id()));
        let (r, _, _) = run_chain(&pat, &bad, 1);
        assert!(r.is_err(), "f(a,b) must be rejected before any walk");
    }

    // Cross-level repeated variable: the root binds X, the subpattern
    // system closes over the binding — f(a, g(b)) dies WITHOUT a
    // structural walk (the documented trap-4 false-accept shape).
    #[test]
    fn repeated_variable_cross_level_binding() {
        let pat = app(vec![sym("f"), var(0), app(vec![sym("g"), var(0)])]);
        let good = app(vec![sym("f"), sym("a"), app(vec![sym("g"), sym("a")])]);
        let bad = app(vec![sym("f"), sym("a"), app(vec![sym("g"), sym("b")])]);
        let (r, bind, plan) = run_chain(&pat, &good, 1);
        assert!(plan.active());
        assert_eq!(r, Ok(()));
        assert_eq!(bind[0], Some(Symbol::from("a").id()));
        let (r, _, _) = run_chain(&pat, &bad, 1);
        assert_eq!(
            r,
            Err(Reject::Binding),
            "f(a, g(b)) vs f(X, g(X)): the closed g-system must refute X's binding",
        );
    }

    // v >= 4 at a node: no surplus row would remain — the node must fall
    // back (no algebra, no rejects) rather than decode without a check.
    #[test]
    fn four_open_seats_fall_back() {
        let pat = app(vec![sym("f"), var(0), var(1), var(2), var(3)]);
        let cand = app(vec![sym("f"), sym("a"), sym("b"), sym("c"), sym("d")]);
        let (r, bind, plan) = run_chain(&pat, &cand, 4);
        assert!(plan.root_fallback(), "v = 4 must mark the root fallback");
        assert!(plan.fallback_any);
        assert!(!plan.active());
        assert_eq!(r, Ok(()), "fallback never rejects");
        assert!(bind.iter().all(Option::is_none), "fallback never binds");
    }

    // A depth-1 all-fresh-distinct-vars pattern constrains nothing the
    // seat prefilter didn't already check: the plan must be TRIVIAL so
    // the caller can skip the chain.
    #[test]
    fn unconstraining_pattern_is_trivial() {
        let facts = TermFactsTable::default();
        let kbo = KboOrdering::new();
        let pat = app(vec![sym("f"), var(0), var(1)]);
        let mut plan = PatternPlan::default();
        compile_into(&mut plan, &pat, &facts, &kbo);
        assert!(plan.trivial());
        assert!(!plan.active());
        assert!(!plan.fallback_any);
    }

    // Nested ground anchors constrain through TWO levels: the pattern
    // f(X, g(X, k)) must reject a candidate matching everywhere except
    // the depth-2 ground anchor — invisible to the depth-1 prefilter.
    #[test]
    fn depth_two_ground_anchor_rejects() {
        let pat = app(vec![
            sym("f"),
            var(0),
            app(vec![sym("g"), var(0), sym("k")]),
        ]);
        let good = app(vec![
            sym("f"),
            sym("a"),
            app(vec![sym("g"), sym("a"), sym("k")]),
        ]);
        let bad = app(vec![
            sym("f"),
            sym("a"),
            app(vec![sym("g"), sym("a"), sym("m")]),
        ]);
        let (r, _, _) = run_chain(&pat, &good, 1);
        assert_eq!(r, Ok(()));
        let (r, _, _) = run_chain(&pat, &bad, 1);
        assert!(r.is_err(), "depth-2 anchor mismatch must reject");
    }
}
