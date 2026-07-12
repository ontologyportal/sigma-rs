//! Stage B′ term arena — the dense hash-consed store for every clause
//! accepted into the search state.  Nodes are fixed-stride records with
//! facts inline (content key, ground flag, KBO weight, var mask); an
//! `App` node's key is bit-identical to the `AtomId` the atom table
//! assigns (shared `ElementHasher` keyspace), so `TermId` and `AtomId`
//! are two coordinates for one object.  Interning happens once per
//! accepted clause at `make`'s accept point; the backward-demodulation
//! postings walk reads node fields instead of re-hashing (B′-1).
//! `SIGMA_NO_ARENA=1` switches every reader back to the owned-tree
//! paths (the A/B off-leg).  Default ON.  Reports through SIGMA_STATS.
//! See docs/plans/term-arena-stage4.md, REVISED DESIGN.

use smallvec::SmallVec;

use crate::syntactic::sentence::ElementHasher;
use crate::types::SymbolId;

use super::super::canon::canonical_var_cached;
use super::super::clause::{PLit, Term};
use super::super::hash64::Map64;

pub(crate) const TAG_VAR: u8 = 0;
pub(crate) const TAG_SYM: u8 = 1;
pub(crate) const TAG_LIT: u8 = 2;
pub(crate) const TAG_OP: u8 = 3;
pub(crate) const TAG_APP: u8 = 4;

pub(crate) const F_HAS_VAR: u16 = 1;
pub(crate) const F_SPILL: u16 = 2;
const MAX_INLINE: usize = 4;

/// Fixed-stride arena node: facts inline, children one hop away.
/// `App` layout is uniform `[head, args...]` (head = kids[0]).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Node {
    /// content key; for `App` nodes this IS the `AtomId` the atom table
    /// would assign (shared `ElementHasher` keyspace)
    pub key: u64,
    /// OR of `1 << (canonical_var_id & 63)` below (KboInfo mask basis)
    pub vmask: u64,
    pub kids: [u32; 4],
    /// dense symbol id / var slot / dense literal id / op byte
    pub sym: u32,
    /// leaf count (== KBO weight under the default unit weight table)
    pub weight: u32,
    pub tag: u8,
    pub nargs: u8,
    pub flags: u16,
    _pad: u32,
}

const _: () = assert!(std::mem::size_of::<Node>() == 48);

#[inline]
fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[derive(Default)]
pub(crate) struct TermArena {
    pub(crate) nodes: Vec<Node>,
    spill: Vec<u32>,
    /// content key -> candidate ids (structurally verified on probe)
    intern: Map64<u64, SmallVec<[u32; 1]>>,
    /// per-run dense symbol remap (content id -> dense)
    sym_dense: Map64<SymbolId, u32>,
    /// dense literal remap (literal content hash -> dense)
    lit_dense: Map64<u64, u32>,
    /// dense symbol index -> production content id (Symbol::id)
    sym_content_ids: Vec<u64>,
    // -- instrument counters ------------------------------------------
    pub(crate) hits: u64,
    pub(crate) misses: u64,
    pub(crate) roots: u64,
    pub(crate) skipped: u64,
    pub(crate) key_mismatches: u64,
    pub(crate) nanos: u64,
}

impl TermArena {
    /// Default ON; `SIGMA_NO_ARENA=1` is the A/B off-switch.
    pub(crate) fn from_env() -> Option<Box<TermArena>> {
        if std::env::var_os("SIGMA_NO_ARENA").is_some() {
            None
        } else {
            Some(Box::default())
        }
    }

    /// Intern one accepted clause's slot-form literal terms; verify each
    /// literal root's content key against its `AtomId`.  Returns the
    /// per-literal root ids, or `None` when any literal cannot intern
    /// (arity above 255) — the caller then leaves that clause on the
    /// owned-tree fallback paths.
    pub(crate) fn intern_clause(
        &mut self,
        terms: &[(bool, Term)],
        lits: &[PLit],
    ) -> Option<SmallVec<[u32; 4]>> {
        let t0 = std::time::Instant::now();
        let mut out: SmallVec<[u32; 4]> = SmallVec::with_capacity(terms.len());
        let mut ok = true;
        for ((_, t), l) in terms.iter().zip(lits.iter()) {
            self.roots += 1;
            match self.intern_term(t) {
                Some(id) => {
                    if self.nodes[id as usize].key != l.atom {
                        self.key_mismatches += 1;
                    }
                    out.push(id);
                }
                None => {
                    self.skipped += 1;
                    ok = false;
                }
            }
        }
        self.nanos += t0.elapsed().as_nanos() as u64;
        ok.then_some(out)
    }

    #[inline]
    pub(crate) fn node(&self, id: u32) -> &Node {
        &self.nodes[id as usize]
    }

    /// The i-th child of a node (spill-aware, one hop).
    #[inline]
    pub(crate) fn kid(&self, n: &Node, i: usize) -> u32 {
        if n.flags & F_SPILL != 0 {
            self.spill[n.kids[0] as usize + i]
        } else {
            n.kids[i]
        }
    }

    /// Production symbol content id of a dense symbol index.
    #[inline]
    pub(crate) fn sym_content(&self, dense: u32) -> u64 {
        self.sym_content_ids[dense as usize]
    }

    pub(crate) fn intern_term(&mut self, t: &Term) -> Option<u32> {
        match t {
            Term::Var(slot) => {
                let canon = canonical_var_cached(*slot as usize);
                let key = mix64((u64::from(TAG_VAR) << 56) ^ canon);
                Some(self.intern_node(
                    key,
                    Node {
                        key,
                        vmask: 1u64 << (canon & 63),
                        kids: [0; 4],
                        sym: *slot as u32,
                        weight: 1,
                        tag: TAG_VAR,
                        nargs: 0,
                        flags: F_HAS_VAR,
                        _pad: 0,
                    },
                    &[],
                ))
            }
            Term::Sym(s) => {
                let id = s.id();
                let next = self.sym_dense.len() as u32;
                let dense = *self.sym_dense.entry(id).or_insert(next);
                if dense as usize == self.sym_content_ids.len() {
                    self.sym_content_ids.push(id);
                }
                let key = mix64((u64::from(TAG_SYM) << 56) ^ id);
                Some(self.intern_node(
                    key,
                    Node {
                        key,
                        vmask: 0,
                        kids: [0; 4],
                        sym: dense,
                        weight: 1,
                        tag: TAG_SYM,
                        nargs: 0,
                        flags: 0,
                        _pad: 0,
                    },
                    &[],
                ))
            }
            Term::Lit(l) => {
                let mut h = ElementHasher::new(1);
                h.literal(l);
                let lh = h.finish();
                let next = self.lit_dense.len() as u32;
                let dense = *self.lit_dense.entry(lh).or_insert(next);
                let key = mix64((u64::from(TAG_LIT) << 56) ^ lh);
                Some(self.intern_node(
                    key,
                    Node {
                        key,
                        vmask: 0,
                        kids: [0; 4],
                        sym: dense,
                        weight: 1,
                        tag: TAG_LIT,
                        nargs: 0,
                        flags: 0,
                        _pad: 0,
                    },
                    &[],
                ))
            }
            Term::Op(op) => {
                use crate::parse::OpKind::*;
                let b: u32 = match op {
                    And => u32::from(b'a'),
                    Or => u32::from(b'o'),
                    Not => u32::from(b'n'),
                    Implies => u32::from(b'i'),
                    Iff => u32::from(b'f'),
                    Equal => u32::from(b'e'),
                    ForAll => u32::from(b'A'),
                    Exists => u32::from(b'E'),
                };
                let key = mix64((u64::from(TAG_OP) << 56) ^ u64::from(b));
                Some(self.intern_node(
                    key,
                    Node {
                        key,
                        vmask: 0,
                        kids: [0; 4],
                        sym: b,
                        weight: 1,
                        tag: TAG_OP,
                        nargs: 0,
                        flags: 0,
                        _pad: 0,
                    },
                    &[],
                ))
            }
            Term::App(elems) => {
                if elems.len() > 255 {
                    return None;
                }
                // children first (post-order), then the parent key in the
                // shared ElementHasher scheme: this App's key == the
                // AtomId/SentenceId intern_atom would assign.
                let mut kid_ids: SmallVec<[u32; 8]> = SmallVec::with_capacity(elems.len());
                for e in elems {
                    kid_ids.push(self.intern_term(e)?);
                }
                let mut h = ElementHasher::new(elems.len());
                let mut vmask = 0u64;
                let mut weight = 0u64;
                let mut flags = 0u16;
                for (e, &kid) in elems.iter().zip(kid_ids.iter()) {
                    let kn = &self.nodes[kid as usize];
                    vmask |= kn.vmask;
                    weight += u64::from(kn.weight);
                    if kn.flags & F_HAS_VAR != 0 {
                        flags |= F_HAS_VAR;
                    }
                    match e {
                        Term::Var(slot) => {
                            h.variable(canonical_var_cached(*slot as usize), false)
                        }
                        Term::Sym(s) => h.symbol(s.id()),
                        Term::Lit(l) => h.literal(l),
                        Term::Op(op) => h.op(op),
                        Term::App(_) => h.sub(kn.key),
                    }
                }
                let key = h.finish();
                Some(self.intern_node(
                    key,
                    Node {
                        key,
                        vmask,
                        kids: [0; 4],
                        sym: 0,
                        weight: weight.min(u64::from(u32::MAX)) as u32,
                        tag: TAG_APP,
                        nargs: elems.len() as u8,
                        flags,
                        _pad: 0,
                    },
                    &kid_ids,
                ))
            }
        }
    }

    fn intern_node(&mut self, key: u64, mut n: Node, kid_ids: &[u32]) -> u32 {
        if let Some(cands) = self.intern.get(&key) {
            for &c in cands {
                let cn = &self.nodes[c as usize];
                if cn.tag == n.tag && cn.sym == n.sym && cn.nargs == n.nargs {
                    let ca: &[u32] = if cn.flags & F_SPILL != 0 {
                        let s = cn.kids[0] as usize;
                        &self.spill[s..s + cn.nargs as usize]
                    } else {
                        &cn.kids[..cn.nargs as usize]
                    };
                    if ca == kid_ids {
                        self.hits += 1;
                        return c;
                    }
                }
            }
        }
        self.misses += 1;
        if kid_ids.len() > MAX_INLINE {
            n.flags |= F_SPILL;
            n.kids[0] = self.spill.len() as u32;
            self.spill.extend_from_slice(kid_ids);
        } else {
            n.kids[..kid_ids.len()].copy_from_slice(kid_ids);
        }
        let id = self.nodes.len() as u32;
        self.nodes.push(n);
        self.intern.entry(key).or_default().push(id);
        id
    }

    /// Subterm at a postings byte path (one argument index per step).
    /// Mirrors `make::subterm_at_bytes` over node kids — the uniform
    /// `[head, args...]` layout makes the path byte the kid index.
    pub(crate) fn subterm_at_path(&self, mut t: u32, path: &[u8]) -> Option<u32> {
        for &b in path {
            let n = self.node(t);
            if n.tag != TAG_APP || usize::from(b) >= usize::from(n.nargs) {
                return None;
            }
            t = self.kid(n, usize::from(b));
        }
        Some(t)
    }

    /// Ground-lhs exact-probe key of an interned demodulator pattern —
    /// the value `postings::ground_lhs_key` computes by hashing, read
    /// off the node instead.  `App` keys ARE the shared content ids;
    /// `Sym` leaves key on the production symbol id.
    pub(crate) fn ground_probe_key(&self, t: u32) -> Option<u64> {
        let n = self.node(t);
        if n.flags & F_HAS_VAR != 0 {
            return None;
        }
        match n.tag {
            TAG_APP => Some(n.key),
            TAG_SYM => Some(self.sym_content(n.sym)),
            _ => None,
        }
    }

    /// `postings::seat_prefilter_match`, exact mirror over node fields:
    /// same accept/reject per candidate, no tree walk, no hashing.
    pub(crate) fn seat_prefilter_match(&self, l: u32, t: u32) -> bool {
        let ln = self.node(l);
        let tn = self.node(t);
        if ln.tag != TAG_APP || tn.tag != TAG_APP {
            return true; // defensive parity with the tree version
        }
        if ln.nargs != tn.nargs {
            return false;
        }
        for i in 0..usize::from(ln.nargs) {
            let ls = self.node(self.kid(ln, i));
            let ts = self.node(self.kid(tn, i));
            let ok = match ls.tag {
                TAG_VAR => true,
                TAG_SYM => ts.tag == TAG_SYM && ls.sym == ts.sym,
                TAG_LIT => ts.tag == TAG_LIT && ls.sym == ts.sym,
                TAG_OP => ts.tag == TAG_OP && ls.sym == ts.sym,
                _ => {
                    if ts.tag != TAG_APP || ls.nargs != ts.nargs {
                        false
                    } else {
                        let lh = self.node(self.kid(ls, 0));
                        let th = self.node(self.kid(ts, 0));
                        match (lh.tag, th.tag) {
                            (TAG_SYM, TAG_SYM) | (TAG_OP, TAG_OP) => lh.sym == th.sym,
                            (TAG_SYM | TAG_OP, _) => false,
                            _ => true,
                        }
                    }
                }
            };
            if !ok {
                return false;
            }
        }
        true
    }

    /// One-way match in id space: binds only pattern variables (at
    /// virtual offset `poff`); occurrence variables match only
    /// themselves.  Bindings are occurrence-side node ids, so the
    /// bound-variable consistency check is ONE integer comparison
    /// (hash-consing: same id ⟺ structurally equal).  Rollback contract
    /// mirrors `unify::match_one_way_off`: failure rolls back to the
    /// entry mark, success leaves bindings for the caller to reset.
    pub(crate) fn match_one_way(
        &self,
        p: u32,
        poff: u32,
        t: u32,
        s: &mut Vec<Option<u32>>,
        trail: &mut Vec<usize>,
    ) -> bool {
        let mark = trail.len();
        if self.match_inner(p, poff, t, s, trail) {
            true
        } else {
            for &slot in &trail[mark..] {
                s[slot] = None;
            }
            trail.truncate(mark);
            false
        }
    }

    fn match_inner(
        &self,
        p: u32,
        poff: u32,
        t: u32,
        s: &mut Vec<Option<u32>>,
        trail: &mut Vec<usize>,
    ) -> bool {
        let np = self.node(p);
        if np.tag == TAG_VAR {
            let slot = (np.sym + poff) as usize;
            return match s[slot] {
                Some(bound) => bound == t,
                None => {
                    s[slot] = Some(t);
                    trail.push(slot);
                    true
                }
            };
        }
        if np.flags & F_HAS_VAR == 0 {
            return p == t; // ground pattern: identity or fail
        }
        let nt = self.node(t);
        if np.tag != nt.tag || np.sym != nt.sym || np.nargs != nt.nargs {
            return false;
        }
        for i in 0..usize::from(np.nargs) {
            if !self.match_inner(self.kid(np, i), poff, self.kid(nt, i), s, trail) {
                return false;
            }
        }
        true
    }

    /// One-line SIGMA_STATS report.
    pub(crate) fn report(&self) -> String {
        let total = self.hits + self.misses;
        format!(
            "arena: {} nodes ({} spilled), {} roots ({} skipped), \
             {}/{} hit/miss (novelty {:.1}%), {} key-mismatches, \
             {:.2}ms accept-intern",
            self.nodes.len(),
            self.spill.len(),
            self.roots,
            self.skipped,
            self.hits,
            self.misses,
            self.misses as f64 * 100.0 / total.max(1) as f64,
            self.key_mismatches,
            self.nanos as f64 / 1e6,
        )
    }
}
