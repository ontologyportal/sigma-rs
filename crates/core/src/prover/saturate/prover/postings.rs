// crates/core/src/prover/saturate/prover/postings.rs
//
// Subterm-occurrence postings (phase 1 of the subterm-index milestone):
// which (clause, literal, position) holds a given subterm.  The
// backward-demodulation retrieval substrate — a newly oriented unit
// equation `l → r` finds the occurrences of `l`-instances in
// output-sensitive time instead of scanning clauses.
//
// Two structures, both prover-owned (arena-lockstep, NOT syntactic-layer
// caches — clause ids are per-run):
//
//   * `exact` — GROUND subterm content key → postings.  Keys live in the
//     shared milestone-27 content-hash keyspace (`TermFactsTable::
//     ground_key_facts` == `intern_atom` ids) for compounds; bare symbol
//     leaves key on their symbol id (own stream — a cross-stream clash is
//     a 2⁻⁶⁴ accident that only adds a false candidate, and the rewrite
//     pass verifies).  A GROUND demodulator lhs `l` matches exactly the
//     occurrences content-equal to `l`, so retrieval is one hash probe.
//   * `heads` — (head key, len) → postings of every concrete-headed
//     compound occurrence, ground AND open.  The non-ground-lhs query
//     surface; key derivation is byte-identical to `DemodIndex`'s `app`
//     buckets, so a demodulator's bucket and its target bucket agree.
//
// Registered nodes are EXACTLY the nodes the demod walk can rewrite:
// every proper subterm in argument position (heads skipped, the
// top-level literal atom excluded), minus shapes no indexable
// demodulator lhs can take (variables, literals, bare operators,
// variable-headed compounds — `DemodIndex::add` drops those lhs shapes,
// so nothing ever queries for them).
//
// Retirement is LAZY: postings are checked against `ClauseRec.retired`
// at query time; the accounting here (`retire` → dead counter) only
// drives periodic compaction, following the DemodIndex generation
// discipline (registration monotone within a generation; retirement
// never invalidates, only de-optimizes).  Compaction is deterministic —
// triggered purely by the total/dead counters, which are a pure
// function of the registration/retirement sequence.
//
// Phase 2 (k-channel Vandermonde rows — see `rows.rs`): every walked
// subterm ALSO gets a 4-word GF(2^64) row, computed bottom-up in the
// SAME registration walk (child content keys fold into the parent's
// hasher and row in one pass — no second traversal).  Head buckets
// carry a contiguous row column in lockstep with their postings (the
// bucket sweep reads it by index), and a content-keyed row table backs
// the decode chain's registered-term probes and subpattern descent.
// The row table is append-only and never compacted: rows are pure
// functions of content (staleness is impossible), and a retired term's
// lingering entry can only let a probe PASS a candidate the structural
// verify then rejects — prefilter semantics, never a wrong answer.

use smallvec::SmallVec;

use crate::syntactic::sentence::ElementHasher;

use super::super::canon::canonical_var_cached;
use super::super::clause::Term;
use super::super::hash64::Map64;
use super::super::kbo::KboOrdering;
use super::super::terms::TermFactsTable;
use super::super::units::op_tag;
use super::rows;

/// One subterm occurrence: owning clause arena id, literal index, and
/// the argument path from the literal atom down to the node (a pool id
/// — see [`SubtermPostings::path`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Posting {
    pub(crate) clause: u32,
    pub(crate) lit: u8,
    pub(crate) path: u32,
}

/// The path byte-vector type: one argument index per step.  Steps > 255
/// cannot be encoded; such occurrences are simply not registered
/// (sound — an unindexed occurrence is just never backward-rewritten).
type PathBytes = SmallVec<[u8; 8]>;

/// One head bucket: postings plus their occurrences' rows.  When
/// `Strategy.subterm_rows` is on the `rows` column rides in lockstep
/// (`posts[i]`'s subterm has row `rows[i]`) and the decode chain's sweep
/// reads it contiguously beside the posting scan; when off the column is
/// left EMPTY (`posts` still fully populated) — the decode chain that
/// reads it never runs, so phase-1 backward demodulation is unaffected.
#[derive(Debug, Default, Clone)]
pub(crate) struct HeadBucket {
    posts: Vec<Posting>,
    rows: Vec<rows::Row>,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct SubtermPostings {
    /// Ground subterm content key → occurrences (see module docs).
    exact: Map64<u64, Vec<Posting>>,
    /// (head key, len) → concrete-headed compound occurrences + rows.
    heads: Map64<(u64, u8), HeadBucket>,
    /// Content key → 4-channel row of EVERY walked subterm (compounds
    /// at all levels, leaf and blank children) — the decode chain's
    /// registered-term probe / descent surface.  Append-only (see
    /// module docs).
    rows: Map64<u64, rows::Row>,
    /// Interned paths; `Posting::path` indexes here.  Paths repeat
    /// massively across clauses (the pool stays tiny), and interning
    /// keeps `Posting` at 12 bytes.
    paths: Vec<PathBytes>,
    path_ids: Map64<PathBytes, u32>,
    /// Postings ever registered (live + dead) / postings owned by
    /// retired clauses — the compaction trigger counters.
    total: u64,
    dead: u64,
    /// Clause id → its posting count (for `retire` accounting).
    counts: Map64<u32, u32>,
}

impl SubtermPostings {
    /// Register every rewritable subterm occurrence of clause `id`
    /// (literal terms `terms`, canonical slot form).  One walk per
    /// clause, at the accept point — the same traversal the demod
    /// redex search performs (children of every `App` from index 1,
    /// top-level atom excluded).
    ///
    /// `store_rows` (= `Strategy.subterm_rows`) gates ONLY the phase-2a
    /// k-channel row machinery: when false, the content-keyed row table
    /// and the per-bucket lockstep row column are left empty and the
    /// GF(2^64) field arithmetic is skipped — the ~2% registration tax
    /// that fell out net-negative.  The POSTINGS (exact ground keys +
    /// (head, len) head buckets) are identical either way: content keys
    /// are still computed (the ground-compound exact key depends on
    /// them), and phase-1 backward demodulation reads only the postings.
    pub(crate) fn register_clause(
        &mut self,
        id: u32,
        terms: &[(bool, Term)],
        facts: &TermFactsTable,
        kbo: &KboOrdering,
        store_rows: bool,
    ) {
        let mut n = 0u32;
        let mut path: PathBytes = SmallVec::new();
        for (li, (_, t)) in terms.iter().enumerate().take(256) {
            debug_assert!(path.is_empty());
            self.walk(id, li as u8, t, true, true, &mut path, facts, kbo, store_rows, &mut n);
        }
        if n > 0 {
            self.total += u64::from(n);
            *self.counts.entry(id).or_insert(0) += n;
        }
    }

    /// One node of the registration walk: registers postings exactly as
    /// phase 1 did (post-order, arg positions only, head seats and
    /// their subtrees excluded via `register = false`) and computes the
    /// node's (content key, groundness, row) bottom-up in the SAME
    /// pass — child keys fold into the parent's `ElementHasher` stream
    /// and Vandermonde row as they return.  Ground compound keys are
    /// byte-identical to `TermFactsTable::ground_key_facts` /
    /// `intern_atom` (debug-twinned below); open compounds key on their
    /// slot-form content id (variables hash as canonical blanks).
    #[cfg_attr(not(any(test, debug_assertions)), allow(unused_variables))]
    #[allow(clippy::too_many_arguments)]
    fn walk(
        &mut self,
        id: u32,
        lit: u8,
        t: &Term,
        is_top: bool,
        register: bool,
        path: &mut PathBytes,
        facts: &TermFactsTable,
        kbo: &KboOrdering,
        store_rows: bool,
        n: &mut u32,
    ) -> (u64, bool) {
        match t {
            Term::Var(slot) => {
                // Canonical-blank coin: blanks act as constants under
                // one-way matching; the presence row backs decode probes.
                let k = rows::blank_key(*slot);
                if store_rows {
                    self.rows.entry(k).or_insert_with(|| rows::leaf_row(k));
                }
                (k, false)
            }
            Term::Sym(s) => {
                let k = s.id();
                if store_rows {
                    self.rows.entry(k).or_insert_with(|| rows::leaf_row(k));
                }
                if register && !is_top {
                    let p = self.posting(id, lit, path);
                    self.exact.entry(k).or_default().push(p);
                    *n += 1;
                }
                (k, true)
            }
            // Literals and bare operators: never a demodulator lhs
            // (`DemodIndex::add` drops those shapes) — no postings, but
            // they are decodable children, so they get presence rows.
            Term::Lit(l) => {
                let k = rows::lit_key(l);
                if store_rows {
                    self.rows.entry(k).or_insert_with(|| rows::leaf_row(k));
                }
                (k, true)
            }
            Term::Op(op) => {
                let k = u64::from(op_tag(op));
                if store_rows {
                    self.rows.entry(k).or_insert_with(|| rows::leaf_row(k));
                }
                (k, true)
            }
            Term::App(elems) => {
                let head = match elems.first() {
                    Some(Term::Sym(s)) => Some(s.id()),
                    Some(Term::Op(op)) => Some(u64::from(op_tag(op))),
                    // Variable-/compound-/literal-headed: no indexable
                    // demodulator lhs can match here — presence row only.
                    _ => None,
                };
                let mut h = ElementHasher::new(elems.len());
                // Row (phase-2a) computed only when `store_rows` is on —
                // the content `key` below is ALWAYS computed (the
                // ground-compound exact posting key depends on it), so
                // the postings stay identical with rows off.
                let mut row = match (store_rows, head) {
                    (true, Some(hk)) => rows::node_tags(hk, elems.len()),
                    _ => [0u64; 4],
                };
                let mut ground = true;
                for (i, e) in elems.iter().enumerate() {
                    // Postings only exist for arg positions reachable by
                    // the demod walk: not under head seats (i == 0), and
                    // only steps that fit a path byte.
                    let reg_child = register && (1..=255).contains(&i);
                    let (k, g) = if reg_child {
                        path.push(i as u8);
                        let r = self.walk(id, lit, e, false, true, path, facts, kbo, store_rows, n);
                        path.pop();
                        r
                    } else {
                        self.walk(id, lit, e, false, false, path, facts, kbo, store_rows, n)
                    };
                    ground &= g;
                    match e {
                        Term::Var(slot) => {
                            h.variable(canonical_var_cached(*slot as usize), false)
                        }
                        Term::Sym(s) => h.symbol(s.id()),
                        Term::Lit(l) => h.literal(l),
                        Term::Op(op) => h.op(op),
                        Term::App(_) => h.sub(k),
                    }
                    if store_rows && i >= 1 {
                        if let Some(hk) = head {
                            // Child key = the LINEAR atom of the row.
                            rows::accum_child(&mut row, rows::seat_elem(hk, i), k);
                        }
                    }
                }
                let key = h.finish();
                if store_rows {
                    if head.is_none() {
                        row = rows::leaf_row(key);
                    }
                    self.rows.entry(key).or_insert(row);
                }
                #[cfg(any(test, debug_assertions))]
                if ground {
                    debug_assert_eq!(
                        facts.ground_key_facts(t, kbo).map(|(k, _)| k),
                        Some(key),
                        "bottom-up walk key must equal ground_key_facts for {t:?}",
                    );
                }
                if register && !is_top {
                    if let Some(hk) = head {
                        let ar = elems.len().min(255) as u8;
                        let p = self.posting(id, lit, path);
                        let b = self.heads.entry((hk, ar)).or_default();
                        b.posts.push(p);
                        // Row column rides in lockstep ONLY when rows are
                        // stored; otherwise it stays empty (the decode
                        // chain that reads it never runs with rows off).
                        if store_rows {
                            b.rows.push(row);
                        }
                        *n += 1;
                        // Ground compound: also exact-keyed.
                        if ground {
                            self.exact.entry(key).or_default().push(p);
                            *n += 1;
                        }
                    }
                }
                (key, ground)
            }
        }
    }

    fn posting(&mut self, clause: u32, lit: u8, path: &PathBytes) -> Posting {
        let pid = match self.path_ids.get(path) {
            Some(&id) => id,
            None => {
                let id = self.paths.len() as u32;
                self.paths.push(path.clone());
                self.path_ids.insert(path.clone(), id);
                id
            }
        };
        Posting { clause, lit, path: pid }
    }

    /// The interned argument path of `p`.
    #[inline]
    pub(crate) fn path(&self, p: &Posting) -> &[u8] {
        &self.paths[p.path as usize]
    }

    /// Occurrences content-equal to the ground key `key` (the ground-lhs
    /// query).  Registration order — clause ids ascending (arena append
    /// order), literals and positions in walk order — so iteration is
    /// deterministic without a per-query sort.
    #[inline]
    pub(crate) fn exact_postings(&self, key: u64) -> &[Posting] {
        self.exact.get(&key).map_or(&[], Vec::as_slice)
    }

    /// Occurrences under head bucket `(head, len)` (the non-ground-lhs
    /// query surface) plus their lockstep row column.  Same
    /// deterministic order as [`Self::exact_postings`].
    #[inline]
    pub(crate) fn head_postings(&self, head: u64, len: u8) -> (&[Posting], &[rows::Row]) {
        self.heads
            .get(&(head, len))
            .map_or((&[][..], &[][..]), |b| (b.posts.as_slice(), b.rows.as_slice()))
    }

    /// The content-keyed row table — the decode chain's registered-term
    /// probe and subpattern-descent surface.
    #[inline]
    pub(crate) fn row_table(&self) -> &Map64<u64, rows::Row> {
        &self.rows
    }

    /// Account clause `id`'s postings as dead (the clause was retired).
    /// Lazy: the postings stay in place — queries filter on
    /// `ClauseRec.retired` — until [`Self::compact`] sweeps them.
    pub(crate) fn retire(&mut self, id: u32) {
        if let Some(n) = self.counts.remove(&id) {
            self.dead += u64::from(n);
        }
    }

    /// Whether the dead fraction crossed the compaction threshold
    /// (over half dead, past a floor that keeps small runs sweep-free).
    /// Purely counter-driven ⇒ deterministic.
    #[inline]
    pub(crate) fn should_compact(&self) -> bool {
        self.total >= 4096 && self.dead * 2 > self.total
    }

    /// Drop every posting whose clause `is_retired`, preserving order
    /// (so post-compaction iteration order is the registration order of
    /// the survivors).  Bucket row columns are swept in lockstep.  The
    /// row TABLE is untouched (append-only — see module docs).  Resets
    /// the accounting to all-live.
    pub(crate) fn compact(&mut self, is_retired: impl Fn(u32) -> bool) {
        let mut live = 0u64;
        for v in self.exact.values_mut() {
            v.retain(|p| !is_retired(p.clause));
            live += v.len() as u64;
        }
        self.exact.retain(|_, v| !v.is_empty());
        for b in self.heads.values_mut() {
            // Row column is either in lockstep (rows stored) or empty
            // (rows off) — sweep it only in the former case.
            let has_rows = !b.rows.is_empty();
            debug_assert!(
                !has_rows || b.posts.len() == b.rows.len(),
                "bucket lockstep when rows are stored",
            );
            let mut w = 0usize;
            for r in 0..b.posts.len() {
                if !is_retired(b.posts[r].clause) {
                    b.posts[w] = b.posts[r];
                    if has_rows {
                        b.rows[w] = b.rows[r];
                    }
                    w += 1;
                }
            }
            b.posts.truncate(w);
            if has_rows {
                b.rows.truncate(w);
            }
            live += w as u64;
        }
        self.heads.retain(|_, b| !b.posts.is_empty());
        self.total = live;
        self.dead = 0;
    }

    /// Total registered postings, live + dead (diagnostics/tests).
    #[allow(dead_code)]
    pub(crate) fn len(&self) -> u64 {
        self.total
    }
}

/// The exact-postings query key of a GROUND demodulator lhs — the same
/// derivation [`SubtermPostings::walk`] used at registration (compound:
/// shared content key; bare symbol: symbol id).  `None` for shapes that
/// are never registered (defensive — `DemodIndex` never produces them).
pub(crate) fn ground_lhs_key(
    l: &Term,
    facts: &TermFactsTable,
    kbo: &KboOrdering,
) -> Option<u64> {
    match l {
        Term::App(_) => facts.ground_key_facts(l, kbo).map(|(k, _)| k),
        Term::Sym(s) => Some(s.id()),
        _ => None,
    }
}

/// The head-bucket query key of a compound demodulator lhs — mirrors
/// [`SubtermPostings::walk`]'s bucket derivation (and `DemodIndex`'s).
pub(crate) fn head_lhs_key(l: &Term) -> Option<(u64, u8)> {
    let Term::App(elems) = l else { return None };
    let head = match elems.first() {
        Some(Term::Sym(s)) => s.id(),
        Some(Term::Op(op)) => u64::from(op_tag(op)),
        _ => return None,
    };
    Some((head, elems.len().min(255) as u8))
}

/// Phase-0 seat prefilter in MATCHING mode, terms-native: can the
/// demodulator lhs `l` (pattern — its slot variables free) possibly
/// one-way match occurrence `t`?  This is exactly the seat-class table
/// `AtomInfo::seats_match_onto` encodes, read directly off the term
/// nodes (no hashing, no allocation): a rigid pattern seat refutes an
/// occurrence seat of a different class — a bare variable, a different
/// leaf, or a differently-headed / differently-sized compound.  Depth-1
/// only; `match_one_way_off` verifies the rest.  NECESSARY for
/// `lσ = t`: σ binds `l`'s variables only, so every rigid seat of `l`
/// survives into the instance with head, length, and leaf content
/// intact.
pub(crate) fn seat_prefilter_match(l: &Term, t: &Term) -> bool {
    let (Term::App(le), Term::App(te)) = (l, t) else {
        // Non-App lhs queries go through the ground/exact path; the
        // bucket only holds Apps.  Defensive: let the matcher decide.
        return true;
    };
    if le.len() != te.len() {
        return false;
    }
    le.iter().zip(te).all(|(ls, ts)| match ls {
        Term::Var(_) => true,
        Term::Sym(a) => matches!(ts, Term::Sym(b) if a.id() == b.id()),
        Term::Lit(a) => matches!(ts, Term::Lit(b) if a == b),
        Term::Op(a) => matches!(ts, Term::Op(b) if op_tag(a) == op_tag(b)),
        Term::App(pe) => match ts {
            Term::App(oe) => {
                pe.len() == oe.len()
                    && match (pe.first(), oe.first()) {
                        (Some(Term::Sym(x)), Some(Term::Sym(y))) => x.id() == y.id(),
                        (Some(Term::Op(x)), Some(Term::Op(y))) => op_tag(x) == op_tag(y),
                        // Concrete pattern head vs anything else: refuted.
                        (Some(Term::Sym(_) | Term::Op(_)), _) => false,
                        // Shapeless pattern head: wildcard.
                        _ => true,
                    }
            }
            // Compound pattern seat vs leaf/variable occurrence seat:
            // one-way matching never binds occurrence variables.
            _ => false,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Symbol;

    fn sym(n: &str) -> Term {
        Term::Sym(Symbol::from(n))
    }
    fn app(v: Vec<Term>) -> Term {
        Term::App(v)
    }

    #[test]
    fn registration_covers_demod_visitable_nodes_only() {
        let facts = TermFactsTable::default();
        let kbo = KboOrdering::new();
        let mut po = SubtermPostings::default();
        // (p (f a) ?X b): registered — (f a) [heads + exact], a [exact],
        // b [exact].  NOT registered: the top atom, the heads p/f, ?X.
        let t = app(vec![
            sym("p"),
            app(vec![sym("f"), sym("a")]),
            Term::Var(0),
            sym("b"),
        ]);
        po.register_clause(7, &[(true, t)], &facts, &kbo, true);
        assert_eq!(po.len(), 4, "3 exact + 1 head posting");

        // Head bucket (f, 2) holds the (f a) occurrence at path [1],
        // with its row column in lockstep.
        let f_id = Symbol::from("f").id();
        let (bucket, brows) = po.head_postings(f_id, 2);
        assert_eq!(bucket.len(), 1);
        assert_eq!(brows.len(), 1, "row column rides in lockstep");
        assert_eq!(bucket[0].clause, 7);
        assert_eq!(bucket[0].lit, 0);
        assert_eq!(po.path(&bucket[0]), &[1]);

        // Exact postings: (f a) under its shared content key, a and b
        // under their symbol ids; the head symbols never post.
        let fa_key = facts.ground_key_facts(&app(vec![sym("f"), sym("a")]), &kbo).unwrap().0;
        assert_eq!(po.exact_postings(fa_key).len(), 1);
        assert_eq!(po.exact_postings(Symbol::from("a").id()).len(), 1);
        assert_eq!(po.exact_postings(Symbol::from("b").id()).len(), 1);
        assert!(po.exact_postings(Symbol::from("p").id()).is_empty(), "head seat never posts");
        // The a-occurrence path is [1, 1] (argument 1 of (f a), which
        // itself sits at argument 1 of the atom).
        let pa = po.exact_postings(Symbol::from("a").id())[0];
        assert_eq!(po.path(&pa), &[1, 1]);
    }

    // Knob OFF (`store_rows = false`, i.e. `Strategy.subterm_rows` off):
    // the POSTINGS must be byte-identical to the rows-on registration
    // (phase 1 unaffected), while the k-channel row machinery — the
    // content-keyed row table AND every bucket's row column — is left
    // completely empty (the ~2% registration tax removed).
    #[test]
    fn rows_off_leaves_postings_identical_and_row_state_empty() {
        let facts = TermFactsTable::default();
        let kbo = KboOrdering::new();
        let build = |store_rows: bool| {
            let mut po = SubtermPostings::default();
            // A fixture exercising every registered shape: ground compound
            // (exact + head), leaves (exact), an open compound (head only),
            // a repeated subterm, and a variable/literal child.
            let t = app(vec![
                sym("p"),
                app(vec![sym("f"), sym("a")]),
                app(vec![sym("g"), Term::Var(0), sym("a")]),
                sym("b"),
            ]);
            po.register_clause(9, &[(true, t)], &facts, &kbo, store_rows);
            po
        };
        let on = build(true);
        let off = build(false);

        // Same posting COUNT (the phase-1 accounting is row-independent).
        assert_eq!(off.len(), on.len(), "posting count is unaffected by rows");

        // Every exact posting bucket is identical.
        let f_id = Symbol::from("f").id();
        let g_id = Symbol::from("g").id();
        let fa_key = facts.ground_key_facts(&app(vec![sym("f"), sym("a")]), &kbo).unwrap().0;
        for key in [fa_key, Symbol::from("a").id(), Symbol::from("b").id()] {
            assert_eq!(off.exact_postings(key), on.exact_postings(key), "exact posting parity");
        }
        // Every head bucket's POSTINGS are identical...
        for (h, ar) in [(f_id, 2u8), (g_id, 3u8)] {
            let (op, _) = off.head_postings(h, ar);
            let (np, _) = on.head_postings(h, ar);
            assert_eq!(op, np, "head posting parity for ({h:#x}, {ar})");
        }
        // ...but the OFF bucket row columns are empty while ON's are full.
        let (fp_on, fr_on) = on.head_postings(f_id, 2);
        let (_fp_off, fr_off) = off.head_postings(f_id, 2);
        assert_eq!(fr_on.len(), fp_on.len(), "rows on: lockstep column");
        assert!(fr_off.is_empty(), "rows off: bucket row column empty");

        // The content-keyed row table is populated on, empty off.
        assert!(!on.row_table().is_empty(), "rows on: row table populated");
        assert!(off.row_table().is_empty(), "rows off: row table empty");
    }

    #[test]
    fn retire_and_compact_account_and_sweep() {
        let facts = TermFactsTable::default();
        let kbo = KboOrdering::new();
        let mut po = SubtermPostings::default();
        let mk = |c: &str| app(vec![sym("p"), app(vec![sym("f"), sym(c)])]);
        po.register_clause(0, &[(true, mk("a"))], &facts, &kbo, true);
        po.register_clause(1, &[(true, mk("b"))], &facts, &kbo, true);
        let before = po.len();
        assert!(before > 0);
        assert!(!po.should_compact(), "no dead postings yet");

        po.retire(0);
        // Below the floor: still no compaction demanded.
        assert!(!po.should_compact(), "floor keeps small runs sweep-free");

        // Sweep manually: clause 0's postings vanish, clause 1's stay,
        // and the row column stays in lockstep.
        po.compact(|c| c == 0);
        let f_id = Symbol::from("f").id();
        let (posts, brows) = po.head_postings(f_id, 2);
        assert!(posts.iter().all(|p| p.clause == 1));
        assert_eq!(posts.len(), brows.len(), "lockstep after compaction");
        assert_eq!(po.len() * 2, before, "half the postings survived");
        assert!(po.exact_postings(Symbol::from("a").id()).is_empty());
        assert_eq!(po.exact_postings(Symbol::from("b").id()).len(), 1);
    }

    #[test]
    fn seat_prefilter_matching_table() {
        // Pattern (f ?0 (g ?1) a) — per-seat classes: head handled by
        // the bucket, then Var / open-compound g / leaf a.
        let l = app(vec![sym("f"), Term::Var(0), app(vec![sym("g"), Term::Var(1)]), sym("a")]);
        let ok = app(vec![sym("f"), sym("x"), app(vec![sym("g"), sym("y")]), sym("a")]);
        assert!(seat_prefilter_match(&l, &ok));

        // Occurrence variable under the rigid compound seat: refuted.
        let var_under_rigid =
            app(vec![sym("f"), sym("x"), Term::Var(5), sym("a")]);
        assert!(!seat_prefilter_match(&l, &var_under_rigid));

        // Head clash inside the compound seat: refuted.
        let head_clash =
            app(vec![sym("f"), sym("x"), app(vec![sym("h"), sym("y")]), sym("a")]);
        assert!(!seat_prefilter_match(&l, &head_clash));

        // Leaf clash at the ground leaf seat: refuted.
        let leaf_clash =
            app(vec![sym("f"), sym("x"), app(vec![sym("g"), sym("y")]), sym("b")]);
        assert!(!seat_prefilter_match(&l, &leaf_clash));

        // The Var pattern seat is a true wildcard.
        let wild = app(vec![
            sym("f"),
            app(vec![sym("k"), sym("z")]),
            app(vec![sym("g"), app(vec![sym("g"), sym("w")])]),
            sym("a"),
        ]);
        assert!(seat_prefilter_match(&l, &wild));
    }

    /// Independent row-key recompute: leaves by their leaf streams,
    /// compounds by the slot-form content id (`slot_atom_content_id` —
    /// a DIFFERENT code path from the walk's incremental hasher).
    fn brute_key(t: &Term) -> u64 {
        match t {
            Term::Var(s) => rows::blank_key(*s),
            Term::Sym(s) => s.id(),
            Term::Lit(l) => rows::lit_key(l),
            Term::Op(op) => u64::from(op_tag(op)),
            Term::App(_) => super::super::super::clause::slot_atom_content_id(t),
        }
    }

    /// Independent row recompute from the term structure alone.
    fn brute_row(t: &Term) -> rows::Row {
        let Term::App(elems) = t else {
            return rows::leaf_row(brute_key(t));
        };
        let head = match elems.first() {
            Some(Term::Sym(s)) => Some(s.id()),
            Some(Term::Op(op)) => Some(u64::from(op_tag(op))),
            _ => None,
        };
        let Some(hk) = head else {
            return rows::leaf_row(brute_key(t));
        };
        let mut r = rows::node_tags(hk, elems.len());
        for (i, e) in elems.iter().enumerate().skip(1) {
            rows::accum_child(&mut r, rows::seat_elem(hk, i), brute_key(e));
        }
        r
    }

    fn every_subterm<'t>(t: &'t Term, out: &mut Vec<&'t Term>) {
        out.push(t);
        if let Term::App(elems) = t {
            for e in elems {
                every_subterm(e, out);
            }
        }
    }

    // Row/registration parity (the SoA-lockstep-style property test):
    // for every subterm of a mixed fixture set — ground/open compounds,
    // nested repeats, blanks, literal and op children — the registered
    // row table entry must equal an independent recompute from the term
    // structure, and every bucket's row column must equal the recompute
    // of the posted occurrence resolved through its path.
    #[test]
    fn row_registration_parity_property() {
        use crate::types::Literal;
        let facts = TermFactsTable::default();
        let kbo = KboOrdering::new();
        let mut po = SubtermPostings::default();
        let fixtures = vec![
            app(vec![
                sym("p"),
                app(vec![sym("f"), sym("a"), Term::Var(0)]),
                app(vec![
                    sym("g"),
                    app(vec![sym("f"), sym("a"), Term::Var(0)]),
                    Term::Lit(Literal::Str("s".into())),
                ]),
            ]),
            app(vec![
                sym("q"),
                app(vec![sym("h"), app(vec![sym("f"), app(vec![sym("f"), sym("a")]), sym("b")])]),
                Term::Lit(Literal::Number("3".into())),
                Term::Var(1),
            ]),
            app(vec![
                Term::Op(crate::parse::OpKind::Equal),
                app(vec![sym("mult"), Term::Var(0), app(vec![sym("inv"), Term::Var(0)])]),
                sym("e"),
            ]),
        ];
        for (ci, t) in fixtures.iter().enumerate() {
            po.register_clause(ci as u32, &[(true, t.clone())], &facts, &kbo, true);
        }
        // Every subterm: row table entry == independent recompute.
        for t in &fixtures {
            let mut subs = Vec::new();
            every_subterm(t, &mut subs);
            for s in subs {
                let k = brute_key(s);
                let got = po.row_table().get(&k).copied();
                assert_eq!(got, Some(brute_row(s)), "row parity for {s:?}");
            }
        }
        // Every bucket: the row column equals the recompute of the
        // occurrence the posting resolves to.
        for (li, t) in fixtures.iter().enumerate() {
            let mut subs = Vec::new();
            every_subterm(t, &mut subs);
            for s in subs {
                let Term::App(elems) = s else { continue };
                let head = match elems.first() {
                    Some(Term::Sym(x)) => x.id(),
                    Some(Term::Op(op)) => u64::from(op_tag(op)),
                    _ => continue,
                };
                let (posts, brows) = po.head_postings(head, elems.len().min(255) as u8);
                assert_eq!(posts.len(), brows.len(), "bucket lockstep");
                for (p, r) in posts.iter().zip(brows) {
                    if p.clause != li as u32 {
                        continue;
                    }
                    let mut occ = &fixtures[p.clause as usize];
                    for &step in po.path(p) {
                        let Term::App(es) = occ else { panic!("path resolves") };
                        occ = &es[step as usize];
                    }
                    assert_eq!(*r, brute_row(occ), "bucket row parity at {:?}", po.path(p));
                }
            }
        }
    }

    #[test]
    fn ground_lhs_key_matches_registration_keys() {
        let facts = TermFactsTable::default();
        let kbo = KboOrdering::new();
        let fa = app(vec![sym("f"), sym("a")]);
        let mut po = SubtermPostings::default();
        po.register_clause(3, &[(true, app(vec![sym("p"), fa.clone()]))], &facts, &kbo, true);
        let k = ground_lhs_key(&fa, &facts, &kbo).unwrap();
        assert_eq!(po.exact_postings(k).len(), 1, "query key == registration key");
        let ks = ground_lhs_key(&sym("a"), &facts, &kbo).unwrap();
        assert_eq!(po.exact_postings(ks).len(), 1, "leaf key == symbol id");
        assert_eq!(head_lhs_key(&fa), Some((Symbol::from("f").id(), 2)));
    }
}
