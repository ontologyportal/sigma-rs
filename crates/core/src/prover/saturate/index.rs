// crates/core/src/saturate/index.rs
//
// The residue index (prototype §5): base tables + lazy union views.
// NO scans — retrieval is hash-table probes all the way down.
//
//     groups[gkey][mask][residue] -> [entries]
//
// A stored atom indexes under its own mask `Mp` keyed by its fingerprint.
// A query atom `q` (mask `Mq`) must probe each stored mask group at the
// *union* mask `U = Mp ∪ Mq` — seats either side leaves open can't
// participate in the key.  Rather than re-walking stored atoms, the union
// table is *derived*: each entry's residue moves from `Mp` to `U` by
// XOR-ing off the coins of the seats in `U ∖ Mp` (Mechanism 3 — one XOR
// per extra seat, never a re-hash).  Derived views are cached and kept
// live by `add`.
//
// The residue/view engine is generic ([`ResidueTable<L>`], parameterized
// over the entry *location* `L`).  Two wrappers ride it: [`LiteralIndex`]
// (whole literals, located by `EntryRef`, grouped by polarity+arity) and
// [`TermIndex`] (subterm *positions*, located by `TermPos`, grouped by
// arity) — the superposition retrieval substrate.  One algebra, two
// granularities.

use smallvec::SmallVec;

use super::hash64::Map64;

use super::clause::AtomId;
use super::AtomInfo;

/// Where an indexed literal lives: (clause arena index, literal index).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EntryRef {
    pub(crate) clause: u32,
    pub(crate) lit:    u8,
}

/// Where an indexed *subterm* lives: a literal plus the position path from
/// the literal's atom down to the subterm (argument indices).  An empty
/// path is the whole atom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TermPos {
    pub(crate) clause: u32,
    pub(crate) lit:    u8,
    pub(crate) path:   SmallVec<[u8; 4]>,
}

/// One indexed entry: its location plus the atom identity the views need
/// for re-keying (coins come from the atom's memoized info).
#[derive(Debug, Clone)]
struct Entry<L> {
    at:   L,
    atom: AtomId,
}

/// Entry locations expose their owning clause so the index can retire a
/// clause's entries (subsumption / backward simplification) by tombstone.
pub(crate) trait Located {
    fn loc_clause(&self) -> u32;
}
impl Located for EntryRef {
    #[inline] fn loc_clause(&self) -> u32 { self.clause }
}
impl Located for TermPos {
    #[inline] fn loc_clause(&self) -> u32 { self.clause }
}

/// Group / view keys, folded to `u64` so the engine needn't be generic
/// over the key type.  Wrappers encode their partition (polarity+arity,
/// or arity) into `gkey`.
type ViewKey = (u64, u64, u64); // (gkey, Mp, U)

/// Resolve an atom's [`AtomInfo`] — the index is representation-agnostic;
/// the prover supplies a closure over its `AtomInfos`/`AtomTable`/store.
pub(crate) trait InfoSource {
    fn info(&self, atom: AtomId) -> std::sync::Arc<AtomInfo>;
}

impl<F> InfoSource for F
where
    F: Fn(AtomId) -> std::sync::Arc<AtomInfo>,
{
    fn info(&self, atom: AtomId) -> std::sync::Arc<AtomInfo> { self(atom) }
}

/// The generic residue-keyed multimap with lazy union views.
#[derive(Debug, Clone)]
struct ResidueTable<L> {
    groups: Map64<u64, Map64<u64, Map64<u64, Vec<Entry<L>>>>>,
    views:  Map64<ViewKey, Map64<u64, Vec<Entry<L>>>>,
    /// Retired clauses — their entries linger in `groups`/`views` (the
    /// lazy views cache copies, so physical removal would have to purge
    /// every derived view) but are filtered out of every probe.  Cheap
    /// when empty (the common case); see `retire`.
    retired: super::hash64::Set64<u32>,
    view_derivations: u64,
}

impl<L> Default for ResidueTable<L> {
    fn default() -> Self {
        Self {
            groups: Map64::default(),
            views: Map64::default(),
            retired: super::hash64::Set64::default(),
            view_derivations: 0,
        }
    }
}

impl<L: Clone + Located> ResidueTable<L> {
    /// Tombstone a clause: its entries are filtered out of subsequent
    /// probes without touching the (view-cached) tables.
    fn retire(&mut self, clause: u32) {
        self.retired.insert(clause);
    }
    /// Index `atom` (info `info`) at location `at` under group `gkey`.
    /// Keeps any cached union views over the same `(gkey, mask)` fresh, so
    /// entries added after a view was derived still surface in probes.
    fn add(&mut self, gkey: u64, at: L, atom: AtomId, info: &AtomInfo) {
        let m = info.mask;
        let r = info.base_residue;
        let entry = Entry { at, atom };
        self.groups
            .entry(gkey).or_default()
            .entry(m).or_default()
            .entry(r).or_default()
            .push(entry.clone());
        // Funnel-pour into live views derived from this (gkey, mask).
        for ((g2, mp, u), tbl) in self.views.iter_mut() {
            if *g2 == gkey && *mp == m {
                let r2 = info.residue_under(*u);
                tbl.entry(r2).or_default().push(entry.clone());
            }
        }
    }

    /// The union view of `gkey`'s `mp`-mask table at mask `u`, derived on
    /// first use (one coin-XOR per entry per extra seat).
    fn view(&mut self, gkey: u64, mp: u64, u: u64, src: &impl InfoSource)
        -> Option<&Map64<u64, Vec<Entry<L>>>>
    {
        if mp == u {
            return self.groups.get(&gkey)?.get(&mp);
        }
        let vk: ViewKey = (gkey, mp, u);
        if !self.views.contains_key(&vk) {
            let base = self.groups.get(&gkey)?.get(&mp)?;
            self.view_derivations += 1;
            let mut v: Map64<u64, Vec<Entry<L>>> =
                Map64::with_capacity_and_hasher(base.len(), Default::default());
            for entries in base.values() {
                for e in entries {
                    let r2 = src.info(e.atom).residue_under(u);
                    v.entry(r2).or_default().push(e.clone());
                }
            }
            self.views.insert(vk, v);
        }
        self.views.get(&vk)
    }

    /// All entries in group `gkey` possibly unifiable with query atom `q`:
    /// one O(1) probe per (stored-mask, union-view).  A *superset* of the
    /// unifiable set (64-bit collisions and seat-64 overflow only widen
    /// it) — callers verify with real unification.
    fn probe(&mut self, gkey: u64, q: &AtomInfo, src: &impl InfoSource) -> Vec<L> {
        let mut out = Vec::new();
        let masks: Vec<u64> = match self.groups.get(&gkey) {
            Some(g) => g.keys().copied().collect(),
            None => return out,
        };
        for mp in masks {
            let u = mp | q.mask;
            let rq = q.residue_under(u);
            if let Some(tbl) = self.view(gkey, mp, u, src) {
                if let Some(entries) = tbl.get(&rq) {
                    out.extend(entries.iter().map(|e| e.at.clone()));
                }
            }
        }
        // Filter tombstoned clauses after the view borrows end.
        if !self.retired.is_empty() {
            out.retain(|at| !self.retired.contains(&at.loc_clause()));
        }
        out
    }

    /// Candidate count in group `gkey` — the same probes as [`Self::probe`]
    /// without materializing entries (exact: retired entries excluded).
    fn count(&mut self, gkey: u64, q: &AtomInfo, src: &impl InfoSource) -> usize {
        if !self.retired.is_empty() {
            return self.probe(gkey, q, src).len();
        }
        let masks: Vec<u64> = match self.groups.get(&gkey) {
            Some(g) => g.keys().copied().collect(),
            None => return 0,
        };
        let mut n = 0;
        for mp in masks {
            let u = mp | q.mask;
            let rq = q.residue_under(u);
            if let Some(tbl) = self.view(gkey, mp, u, src) {
                n += tbl.get(&rq).map_or(0, Vec::len);
            }
        }
        n
    }
}

// -- Literal index (whole literals, grouped by polarity + arity) --------------

#[derive(Debug, Default, Clone)]
pub(crate) struct LiteralIndex {
    t: ResidueTable<EntryRef>,
}

impl LiteralIndex {
    #[inline]
    fn gkey(pos: bool, arity: u8) -> u64 {
        (u64::from(arity) << 1) | u64::from(pos)
    }

    /// Index literal `lit` of clause `clause` (polarity `pos`, atom `atom`).
    pub(crate) fn add(
        &mut self, at: EntryRef, pos: bool, atom: AtomId, src: &impl InfoSource,
    ) {
        let info = src.info(atom);
        self.t.add(Self::gkey(pos, info.arity), at, atom, &info);
    }

    /// Indexed literals of polarity `pos` possibly unifiable with `q`.
    pub(crate) fn probe(
        &mut self, pos: bool, q: &AtomInfo, src: &impl InfoSource,
    ) -> Vec<EntryRef> {
        self.t.probe(Self::gkey(pos, q.arity), q, src)
    }

    /// Candidates with the *opposite* polarity — resolution partners.
    pub(crate) fn complementary(
        &mut self, pos: bool, q: &AtomInfo, src: &impl InfoSource,
    ) -> Vec<EntryRef> {
        self.probe(!pos, q, src)
    }

    /// Complementary candidate count (the fewest-candidates heuristic).
    pub(crate) fn count_complementary(
        &mut self, pos: bool, q: &AtomInfo, src: &impl InfoSource,
    ) -> usize {
        self.t.count(Self::gkey(!pos, q.arity), q, src)
    }

    /// How many union views were derived (a retrieval-cost probe; tests).
    pub(crate) fn view_derivations(&self) -> u64 {
        self.t.view_derivations
    }

    /// Tombstone a clause — its literals no longer surface as partners
    /// (subsumption / backward simplification).
    pub(crate) fn retire(&mut self, clause: u32) {
        self.t.retire(clause);
    }
}

// -- Term index (subterm positions, grouped by arity) -------------------------
//
// The superposition retrieval substrate: every non-variable subterm
// position of every active clause, keyed by the subterm's fingerprint.
// Probing with an equation side `s` returns the positions `s` may rewrite
// (THE KEY EQUATION at subterm grain), verified by real unification.

#[derive(Debug, Default, Clone)]
pub(crate) struct TermIndex {
    t: ResidueTable<TermPos>,
}

impl TermIndex {
    /// Index the subterm `atom` (its memoized `info`) at position `pos`.
    /// Arity is the group key — terms carry no polarity.  `atom` is passed
    /// explicitly because `AtomInfo` is shared and id-agnostic, and the
    /// union views re-key by `info(atom).residue_under(u)`.
    pub(crate) fn add(&mut self, pos: TermPos, atom: AtomId, info: &AtomInfo) {
        self.t.add(u64::from(info.arity), pos, atom, info);
    }

    /// Subterm positions possibly unifiable with query term `q`.
    pub(crate) fn probe(&mut self, q: &AtomInfo, src: &impl InfoSource) -> Vec<TermPos> {
        self.t.probe(u64::from(q.arity), q, src)
    }

    /// Tombstone a clause — its subterm positions no longer surface as
    /// superposition / backward-demodulation targets.
    #[allow(dead_code)] // consumed in Phase 4 (backward demod / superposition)
    pub(crate) fn retire(&mut self, clause: u32) {
        self.t.retire(clause);
    }
}
