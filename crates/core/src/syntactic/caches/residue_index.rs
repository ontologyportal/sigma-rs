//! Root sentences indexed by their residue fingerprints: a view lattice
//! that unifies the head index, the (head, arg1) subject lookup, and
//! partial-pattern lookup into one algebra.
//!
//! Every root gets per-seat coins (seat 0 = head) and a mask of its
//! non-ground seats. The base table buckets roots by (arity, mask)
//! keyed by their fingerprint (XOR of ground-seat coins). A query with
//! open seats probes through a lazily-derived union view: the base
//! bucket re-keyed with the union-mask seats' coins XORed off. Pattern
//! and fact compute the same bucket address independently:
//!
//! ```text
//! residue_under(fact, mask(pattern)) == fingerprint(pattern)
//! ```
//!
//! The classic indexes are points on this lattice:
//!
//! ```text
//! mask {}          exact lookup        (content addressing)
//! mask {2..}       (head, arg1) index  (subject lookup)
//! mask {1..}       head index          (`by_head`)
//! ```
//!
//! Fingerprints are two words (power sums ⟨Σx, Σx³⟩ over GF(2^64), see
//! `gf64`), so a probe's residual is decodable: up to two unknown
//! seat-fillers can be recovered algebraically from the residual plus
//! the coin dictionary, without resolving the stored sentence.

use std::collections::HashMap;

use smallvec::SmallVec;
use xxhash_rust::xxh64::xxh64;

use crate::cache::events::{Event, EventKind};
use crate::cache::{EagerBehavior, EagerIndex};
use crate::gf64::{self, Decoded, Sketch};
use crate::syntactic::SyntacticLayer;
use crate::types::{Element, Literal, Sentence, SentenceId, SymbolId};

/// Seat indices at or beyond this never carry coins (they are treated
/// as permanently masked); roots that wide fall back to head-only
/// indexing.
const MAX_SEATS: usize = 60;

/// Coin keyspace seed — distinct from every other hash stream.
const COIN_SEED: u64 = 0x51DE_C0DE_51DE_C0DE;

/// What a coin encodes — the decode phone book's payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoinMeaning {
    /// A ground symbol.
    Sym(SymbolId),
    /// A ground compound subterm (by content hash).
    Sub(SentenceId),
    /// A string/number literal (opaque — decode consumers filter).
    Lit,
}

/// The coin for `meaning` sitting in `seat`.  Nonzero (0 is the XOR
/// identity and can never participate in a sketch).
fn coin(seat: u8, meaning: &CoinMeaning) -> u64 {
    let (tag, key) = match meaning {
        CoinMeaning::Sym(id) => (b'S', *id),
        CoinMeaning::Sub(id) => (b'L', *id),
        CoinMeaning::Lit => unreachable!("literal coins minted via coin_lit"),
    };
    let mut buf = [0u8; 10];
    buf[0] = seat;
    buf[1] = tag;
    buf[2..].copy_from_slice(&key.to_be_bytes());
    let c = xxh64(&buf, COIN_SEED);
    if c == 0 { 1 } else { c }
}

fn coin_lit(seat: u8, lit: &Literal) -> u64 {
    let (tag, bytes): (u8, &[u8]) = match lit {
        Literal::Str(v) => (b'T', v.as_bytes()),
        Literal::Number(v) => (b'N', v.as_bytes()),
    };
    let mut buf = Vec::with_capacity(bytes.len() + 2);
    buf.push(seat);
    buf.push(tag);
    buf.extend_from_slice(bytes);
    let c = xxh64(&buf, COIN_SEED);
    if c == 0 { 1 } else { c }
}

/// Everything the index remembers about one root.
#[derive(Debug, Clone)]
pub(crate) struct RootInfo {
    arity: u8,
    /// Bit i set ⇔ seat i is non-ground (a variable, or contains one).
    mask: u64,
    /// Head symbol for symbol-headed roots (`None` ⇒ operator-headed).
    head: Option<SymbolId>,
    /// Coin per seat (0 for masked seats).
    coins: SmallVec<[u64; 6]>,
    /// Two-word power-sum fingerprint over the ground seats.
    sketch: Sketch,
}

type Bucket = SmallVec<[SentenceId; 2]>;

/// The whole index state, mutated under the `Eager` cache's lock.
#[derive(Debug, Default, Clone)]
pub(crate) struct ResidueIndex {
    /// Base tables: (arity, base-mask) → { s1 fingerprint → roots }.
    groups: HashMap<(u8, u64), HashMap<u64, Bucket>>,
    /// Derived union views: (arity, base-mask, union-mask) → re-keyed
    /// buckets. Lazily built on first probe; kept fresh by add/remove.
    views: HashMap<(u8, u64, u64), HashMap<u64, Bucket>>,
    /// Per-root info, for re-keying, removal, and residual decoding.
    roots: HashMap<SentenceId, RootInfo>,
    /// coin → (seat, meaning) decode dictionary. Append-only.
    dict: HashMap<u64, (u8, CoinMeaning)>,
    /// head symbol → arities seen (for arity-agnostic `by_head`).
    head_arities: HashMap<SymbolId, SmallVec<[u8; 4]>>,
    /// Roots too wide to fingerprint (arity > MAX_SEATS): head-only.
    oversize: HashMap<SymbolId, Bucket>,
}

impl ResidueIndex {
    // -- indexing ---------------------------------------------------------------

    /// Compute a root's [`RootInfo`], minting dictionary entries for its
    /// ground seats.  `None` for over-wide roots (handled separately).
    fn root_info(&mut self, parent: &SyntacticLayer, sentence: &Sentence) -> Option<RootInfo> {
        let n = sentence.elements.len();
        if n > MAX_SEATS {
            return None;
        }
        let mut mask = 0u64;
        let mut coins: SmallVec<[u64; 6]> = SmallVec::with_capacity(n);
        let mut sketch = Sketch::default();
        for (i, el) in sentence.elements.iter().enumerate() {
            let seat = i as u8;
            let entry: Option<(u64, CoinMeaning)> = match el {
                Element::Variable { .. } => None,
                Element::Symbol(s) => {
                    let m = CoinMeaning::Sym(s.id());
                    Some((coin(seat, &m), m))
                }
                Element::Op(op) => {
                    let m = CoinMeaning::Sym(u64::from(op_byte(op)));
                    Some((coin(seat, &m), m))
                }
                Element::Literal(l) => Some((coin_lit(seat, l), CoinMeaning::Lit)),
                Element::Sub(sid) => {
                    if parent.sentence_vars(*sid).is_empty() {
                        let m = CoinMeaning::Sub(*sid);
                        Some((coin(seat, &m), m))
                    } else {
                        None
                    }
                }
            };
            match entry {
                Some((c, m)) => {
                    coins.push(c);
                    sketch.toggle(c);
                    self.dict.entry(c).or_insert((seat, m));
                }
                None => {
                    mask |= 1u64 << i;
                    coins.push(0);
                }
            }
        }
        Some(RootInfo {
            arity: n as u8,
            mask,
            head: sentence.head_symbol(),
            coins,
            sketch,
        })
    }

    fn add_root(&mut self, parent: &SyntacticLayer, sid: SentenceId, sentence: &Sentence) {
        if self.roots.contains_key(&sid) {
            return;
        }
        let Some(info) = self.root_info(parent, sentence) else {
            if let Some(h) = sentence.head_symbol() {
                self.oversize.entry(h).or_default().push(sid);
            }
            return;
        };
        let gkey = (info.arity, info.mask);
        self.groups
            .entry(gkey)
            .or_default()
            .entry(info.sketch.s1)
            .or_default()
            .push(sid);
        for ((a, mp, u), tbl) in self.views.iter_mut() {
            if (*a, *mp) == gkey {
                let k = rekey(&info, *u);
                tbl.entry(k).or_default().push(sid);
            }
        }
        if let Some(h) = info.head {
            let ar = self.head_arities.entry(h).or_default();
            if !ar.contains(&info.arity) {
                ar.push(info.arity);
            }
        }
        self.roots.insert(sid, info);
    }

    fn remove_root(&mut self, sid: SentenceId) {
        let Some(info) = self.roots.remove(&sid) else {
            // Possibly an over-wide root.
            for bucket in self.oversize.values_mut() {
                bucket.retain(|s| *s != sid);
            }
            return;
        };
        let gkey = (info.arity, info.mask);
        if let Some(tbl) = self.groups.get_mut(&gkey) {
            if let Some(b) = tbl.get_mut(&info.sketch.s1) {
                b.retain(|s| *s != sid);
                if b.is_empty() {
                    tbl.remove(&info.sketch.s1);
                }
            }
            if tbl.is_empty() {
                self.groups.remove(&gkey);
            }
        }
        for ((a, mp, u), tbl) in self.views.iter_mut() {
            if (*a, *mp) == gkey {
                let k = rekey(&info, *u);
                if let Some(b) = tbl.get_mut(&k) {
                    b.retain(|s| *s != sid);
                    if b.is_empty() {
                        tbl.remove(&k);
                    }
                }
            }
        }
        // `head_arities` / `dict` are left in place: the arity may still
        // exist on other roots, and a dict entry can never go stale.
    }

    // -- probing ----------------------------------------------------------------

    /// All roots possibly matching a pattern with `p_mask` open seats and
    /// `p_coins` at its ground seats, within one arity.  One O(1) probe
    /// per (stored-mask, union-view); views derive lazily.
    fn probe(
        &mut self,
        arity: u8,
        p_mask: u64,
        p_coins: &[u64],
        out: &mut Vec<SentenceId>,
    ) {
        let masks: Vec<u64> = self
            .groups
            .keys()
            .filter(|(a, _)| *a == arity)
            .map(|(_, m)| *m)
            .collect();
        for mp in masks {
            let u = mp | p_mask;
            // Pattern residue under U: XOR off its coins at seats U opens
            // beyond the pattern's own mask.
            let mut key = 0u64;
            for (i, c) in p_coins.iter().enumerate() {
                if *c != 0 && (u >> i) & 1 == 0 {
                    key ^= *c;
                }
            }
            if mp == u {
                if let Some(b) = self.groups.get(&(arity, mp)).and_then(|t| t.get(&key)) {
                    out.extend(b.iter().copied());
                }
            } else {
                self.ensure_view(arity, mp, u);
                if let Some(b) = self.views.get(&(arity, mp, u)).and_then(|t| t.get(&key)) {
                    out.extend(b.iter().copied());
                }
            }
        }
    }

    fn ensure_view(&mut self, arity: u8, mp: u64, u: u64) {
        if self.views.contains_key(&(arity, mp, u)) {
            return;
        }
        let mut tbl: HashMap<u64, Bucket> = HashMap::new();
        if let Some(base) = self.groups.get(&(arity, mp)) {
            for bucket in base.values() {
                for sid in bucket {
                    let info = &self.roots[sid];
                    tbl.entry(rekey(info, u)).or_default().push(*sid);
                }
            }
        }
        self.views.insert((arity, mp, u), tbl);
    }

    /// Every root headed by `h`, across arities and masks.
    fn by_head_ids(&mut self, h: SymbolId) -> Vec<SentenceId> {
        let mut out = Vec::new();
        let arities = self.head_arities.get(&h).cloned().unwrap_or_default();
        for arity in arities {
            let m = CoinMeaning::Sym(h);
            let c0 = coin(0, &m);
            let p_mask = mask_all_but(arity, &[0]);
            self.probe(arity, p_mask, &[c0], &mut out);
        }
        if let Some(b) = self.oversize.get(&h) {
            out.extend(b.iter().copied());
        }
        out
    }

    /// `(head, arg1)` subject lookup.
    fn by_head_arg1_ids(&mut self, h: SymbolId, subject: SymbolId) -> Vec<SentenceId> {
        let mut out = Vec::new();
        let arities = self.head_arities.get(&h).cloned().unwrap_or_default();
        for arity in arities {
            if arity < 2 {
                continue;
            }
            let c0 = coin(0, &CoinMeaning::Sym(h));
            let c1 = coin(1, &CoinMeaning::Sym(subject));
            let p_mask = mask_all_but(arity, &[0, 1]);
            self.probe(arity, p_mask, &[c0, c1], &mut out);
        }
        out
    }

    /// Ground binary facts `(head subject OBJ)` with the object recovered
    /// by decoding the residual, without resolving the sentence. Returns
    /// `(object, sid)` pairs; a symbol object decodes to `Some`, while a
    /// non-symbol object or any decode failure yields `None` for the
    /// caller to resolve the slow way.
    fn binary_objects(
        &mut self,
        h: SymbolId,
        subject: SymbolId,
    ) -> Vec<(Option<SymbolId>, SentenceId)> {
        let c0 = coin(0, &CoinMeaning::Sym(h));
        let c1 = coin(1, &CoinMeaning::Sym(subject));
        let mut pattern = Sketch::default();
        pattern.toggle(c0);
        pattern.toggle(c1);

        let mut sids = Vec::new();
        // Arity 3, fully-ground roots only: a root with a variable object
        // cannot yield a ground object.
        if self.groups.contains_key(&(3, 0)) {
            self.probe_ground_binary(&mut sids, c0 ^ c1);
        }
        let mut out = Vec::with_capacity(sids.len());
        for sid in sids {
            let info = &self.roots[&sid];
            let residual = info.sketch.xor(pattern);
            let obj = match gf64::decode(residual, 1) {
                Decoded::One(c) => match self.dict.get(&c) {
                    Some((2, CoinMeaning::Sym(b))) => Some(*b),
                    _ => None,
                },
                _ => None,
            };
            out.push((obj, sid));
        }
        out
    }

    fn probe_ground_binary(&mut self, out: &mut Vec<SentenceId>, key_open2: u64) {
        // Ground arity-3 roots, viewed with seat 2 open.
        self.ensure_view(3, 0, 1 << 2);
        if let Some(b) = self.views.get(&(3, 0, 1 << 2)).and_then(|t| t.get(&key_open2)) {
            out.extend(b.iter().copied());
        }
    }

    /// Every head symbol with at least one indexed root.
    pub(crate) fn head_symbols(&self) -> Vec<SymbolId> {
        self.head_arities.keys().copied().chain(self.oversize.keys().copied()).collect()
    }
}

/// A root's view key under union-mask `u`: its fingerprint with the
/// coins at `u`'s seats XORed off.
fn rekey(info: &RootInfo, u: u64) -> u64 {
    let mut k = info.sketch.s1;
    let mut bits = u;
    while bits != 0 {
        let i = bits.trailing_zeros() as usize;
        if let Some(c) = info.coins.get(i) {
            k ^= *c;
        }
        bits &= bits - 1;
    }
    k
}

/// The open-mask for "everything except these seats" at `arity`.
fn mask_all_but(arity: u8, keep: &[usize]) -> u64 {
    let mut m = if arity as u32 >= 64 { u64::MAX } else { (1u64 << arity) - 1 };
    for k in keep {
        m &= !(1u64 << k);
    }
    m
}

fn op_byte(op: &crate::parse::OpKind) -> u8 {
    use crate::parse::OpKind::*;
    match op {
        And => b'a', Or => b'o', Not => b'n', Implies => b'i',
        Iff => b'f', Equal => b'e', ForAll => b'A', Exists => b'E',
    }
}

// -- The cache behavior ----------------------------------------------------------

/// Behavior for the `syntactic::residue_index` eager index.
#[derive(Debug, Default)]
pub(crate) struct ResidueCache;

impl EagerBehavior for ResidueCache {
    type Parent = SyntacticLayer;
    type Value = ResidueIndex;

    const NAME: &'static str = "syntactic::residue_index";

    fn initial(&self) -> ResidueIndex {
        ResidueIndex::default()
    }

    fn consumes(&self) -> &'static [EventKind] {
        &[EventKind::RootAdded, EventKind::RootRemoved]
    }

    /// Symbol-headed root changes re-emit as RelationAdded/RelationRemoved
    /// for the semantic layer.
    fn produces(&self) -> &'static [EventKind] {
        &[EventKind::RelationAdded, EventKind::RelationRemoved]
    }

    // Reads each added root's body (and its subterms' variable sets)
    // from the sentence store, which must run first.
    fn reads(&self) -> &'static [&'static str] {
        &["syntactic::sentences"]
    }

    fn react(
        &self,
        parent: &SyntacticLayer,
        events: &[&Event],
        store: &EagerIndex<ResidueIndex>,
    ) -> Vec<Event> {
        store.update_with(|idx| {
            let mut out = Vec::new();
            for e in events {
                match e {
                    Event::RootAdded { sid } => {
                        if let Some(s) = parent.sentence(*sid) {
                            idx.add_root(parent, *sid, &s);
                            if let Some(head_id) = s.head_symbol() {
                                out.push(Event::RelationAdded { sid: *sid, head_id });
                            }
                        }
                    }
                    Event::RootRemoved { sid, sentences } => {
                        idx.remove_root(*sid);
                        // The body rides on the event (the store copy is
                        // gone) so downstream reactors can read it.
                        if let Some(root) = sentences.iter().find(|s| s.hash() == *sid) {
                            if root.head_symbol().is_some() {
                                out.push(Event::RelationRemoved {
                                    sid: *sid,
                                    sentence: root.clone(),
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
            out
        })
    }

    /// Rebuild from the restored sentence store. Idempotent top-up:
    /// re-adds every store root (`add_root` short-circuits on roots it
    /// already holds), so a partially-thawed snapshot is completed rather
    /// than left as-is.
    fn initialize(&self, parent: &SyntacticLayer, store: &EagerIndex<ResidueIndex>) {
        store.update_with(|idx| {
            for sid in parent.root_sids() {
                if !idx.roots.contains_key(&sid) {
                    if let Some(s) = parent.sentence(sid) {
                        idx.add_root(parent, sid, &s);
                    }
                }
            }
        });
    }
}

// -- Layer accessors ---------------------------------------------------------------

impl SyntacticLayer {
    /// Every root headed by `head` (name form).
    pub(crate) fn by_head(&self, head: &str) -> Vec<SentenceId> {
        let Some(id) = self.sym_id(head) else { return Vec::new() };
        self.by_head_id(&id)
    }

    /// Every root headed by `head` (symbol id).
    pub(crate) fn by_head_id(&self, head: &SymbolId) -> Vec<SentenceId> {
        let h = *head;
        self.residue.update_with(|idx| idx.by_head_ids(h))
    }

    /// `(head, arg1)` subject lookup.
    pub(crate) fn by_head_arg1(&self, head: SymbolId, subject: SymbolId) -> Vec<SentenceId> {
        self.residue.update_with(|idx| idx.by_head_arg1_ids(head, subject))
    }

    /// Ground binary facts `(head subject OBJ)` with the object
    /// recovered by residual decoding where possible (`None` object ⇒
    /// caller resolves the sentence — non-symbol object or collision).
    pub(crate) fn binary_objects(
        &self,
        head: SymbolId,
        subject: SymbolId,
    ) -> Vec<(Option<SymbolId>, SentenceId)> {
        self.residue.update_with(|idx| idx.binary_objects(head, subject))
    }

    /// Every head symbol with at least one indexed root.
    pub(crate) fn residue_head_symbols(&self) -> Vec<SymbolId> {
        self.residue.with_ref(|idx| idx.head_symbols())
    }
}

#[cfg(test)]
mod tests {
    use crate::syntactic::SyntacticLayer;
    use std::collections::HashSet;

    fn set(v: Vec<u64>) -> HashSet<u64> {
        v.into_iter().collect()
    }

    /// Views serve the classic queries correctly on a mixed fixture
    /// (facts, rules, multiple arities, nested subs) — every result
    /// shape-verified against the stored sentences.
    #[test]
    fn views_serve_head_and_subject_queries() {
        let mut layer = SyntacticLayer::default();
        layer.load_kif(
            "(foo A B)(foo B C)(bar X Y)
             (=> (foo A C) (baz W D))
             (domain likes 1 Animal)
             (domain likes 2 Object)
             (domain hates 1 Agent)
             (instance likes BinaryPredicate)
             (part (ComponentFn Engine) Car)",
            "test",
        );
        for head in ["foo", "bar", "baz", "domain", "instance", "part", "nope"] {
            let old: HashSet<u64> = layer.by_head(head).iter().copied().collect();
            let new = set(layer.by_head(head));
            assert_eq!(old, new, "by_head({head}) must agree");
        }
        let domain = layer.sym_id("domain").unwrap();
        let likes = layer.sym_id("likes").unwrap();
        let hates = layer.sym_id("hates").unwrap();
        assert_eq!(
            set(layer.by_head_arg1(domain, likes)),
            set(layer.by_head_arg1(domain, likes)),
        );
        assert_eq!(
            set(layer.by_head_arg1(domain, hates)),
            set(layer.by_head_arg1(domain, hates)),
        );
        assert_eq!(layer.by_head_arg1(domain, likes).len(), 2);
    }

    /// Rules index under their operator head with masked antecedents —
    /// they must never pollute symbol-head views.
    #[test]
    fn rules_do_not_pollute_head_views() {
        let mut layer = SyntacticLayer::default();
        layer.load_kif("(=> (p ?X) (q ?X))\n(p a)", "test");
        assert_eq!(layer.by_head("p").len(), 1, "only the fact, not the rule");
        assert!(layer.by_head("q").is_empty());
    }

    /// Removal: retracting a root drops it from base tables AND any
    /// live derived view (the view was materialized first).
    #[test]
    fn removal_updates_live_views() {
        use crate::cache::events::Event;
        use crate::layer::Layer;

        let mut layer = SyntacticLayer::default();
        layer.load_kif("(domain likes 1 Animal)", "sess");
        let domain = layer.sym_id("domain").unwrap();
        let likes = layer.sym_id("likes").unwrap();
        // Materialize the views by querying first.
        assert_eq!(layer.by_head_arg1(domain, likes).len(), 1);
        assert_eq!(layer.by_head("domain").len(), 1);

        let sid = layer.by_head_arg1(domain, likes)[0];
        let sentence = layer.sentence(sid).expect("real sid");
        let _ = layer.cascade(vec![Event::RootRemoved {
            sid,
            sentences: vec![(*sentence).clone()],
        }]);

        assert!(layer.by_head_arg1(domain, likes).is_empty());
        assert!(layer.by_head("domain").is_empty());
    }

    /// Algebraic parameter extraction: ground binary objects recovered
    /// from the residual — never resolving the sentences — and exactly
    /// matching the shape-checked slow path.
    #[test]
    fn binary_objects_decode_from_residual() {
        let mut layer = SyntacticLayer::default();
        layer.load_kif(
            "(located A B)(located A C)(located B D)
             (located A 4.5)
             (part A B)",
            "test",
        );
        let located = layer.sym_id("located").unwrap();
        let a = layer.sym_id("A").unwrap();
        let b = layer.sym_id("B").unwrap();
        let c = layer.sym_id("C").unwrap();

        let got = layer.binary_objects(located, a);
        // Three (located A _) roots: B, C decode as symbols; 4.5 is a
        // literal → decode reports it as None (slow-path marker).
        assert_eq!(got.len(), 3);
        let syms: HashSet<u64> = got.iter().filter_map(|(o, _)| *o).collect();
        assert_eq!(syms, HashSet::from([b, c]));
        let none_count = got.iter().filter(|(o, _)| o.is_none()).count();
        assert_eq!(none_count, 1, "the literal object flags the slow path");
        // Every returned sid is a real (located A _) root.
        for (_, sid) in &got {
            let s = layer.sentence(*sid).unwrap();
            assert_eq!(s.head_symbol(), Some(located));
        }
    }

    /// Arbitrary-seat probing finds (located ? B) by second argument.
    #[test]
    fn probe_by_second_argument() {
        let mut layer = SyntacticLayer::default();
        layer.load_kif("(located A B)(located C B)(located B D)", "test");
        let located = layer.sym_id("located").unwrap();
        let b = layer.sym_id("B").unwrap();

        // Pattern (located ?X B): seats 0 and 2 ground, seat 1 open.
        let sids = layer.residue.update_with(|idx| {
            let c0 = super::coin(0, &super::CoinMeaning::Sym(located));
            let c2 = super::coin(2, &super::CoinMeaning::Sym(b));
            let mut out = Vec::new();
            idx.probe(3, 1 << 1, &[c0, 0, c2], &mut out);
            out
        });
        assert_eq!(sids.len(), 2, "(located A B) and (located C B)");
        for sid in sids {
            let s = layer.sentence(sid).unwrap();
            assert!(matches!(s.elements.get(2),
                Some(crate::Element::Symbol(sym)) if sym.id() == b));
        }
    }
}
