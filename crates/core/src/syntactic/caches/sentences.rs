//! `syntactic::sentences` — the primary sentence store, as a content-addressed
//! `EagerMap`: `SentenceId → Arc<Sentence>` (the `EntryCache`) plus a
//! [`SentenceSide`] of provenance / refcount / scope companion state.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::{DashMap, DashSet};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use crate::syntactic::caches::session::SessionCache;
use crate::syntactic::caches::source::SourceCache;
use crate::syntactic::caches::symbol::SymbolCache;
use crate::syntactic::sentence::ScopeCtx;
use crate::{AstNode, Element, Sentence, SymbolId};
use crate::cache::events::{Event, EventKind};
use crate::cache::{EagerMap, EagerMapBehavior, EntryCache};
use crate::syntactic::SyntacticLayer;
use crate::types::{ElementVec, SentenceId};

/// Companion (non-keyed) state for the sentence [`EagerMap`](crate::cache::EagerMap).
///
/// The keyed map — `SentenceId → Arc<Sentence>` — is the cache's `EntryCache`;
/// this `Side` holds everything else.
///
/// A sentence is collected only when it is in *neither* `roots` nor `subs`.
#[derive(Debug, Default)]
pub(crate) struct SentenceSide {
    /// Source fingerprint → the root sentence ids it produced (1→N for CAF /
    /// row-var expansion).  Read through `roots_of_fingerprint` / related
    /// accessors.
    forward:         DashMap<u64, SmallVec<[SentenceId; 2]>>,
    /// Sparse source-refcount: roots produced by *more than one* fingerprint
    /// (absent ⇒ exactly one source).
    source_overflow: DashMap<SentenceId, u32>,
    /// Sparse sub-usage refcount: subs referenced by *more than one*
    /// `Element::Sub` (absent ⇒ exactly one parent reference).
    parent_overflow: DashMap<SentenceId, u32>,
    /// Variable scope disambiguation counter (not persisted).
    scope_counter:   Arc<AtomicU64>,
    /// Source-backed sentence ids (≥1 producing fingerprint).  A root may also
    /// be a sub.  Read via `root_sids` / `num_roots`.
    roots:           DashSet<SentenceId>,
    /// Sentence ids referenced as a sub by ≥1 parent.  A sub may also be a root.
    /// Read via `sub_sentences`.
    subs:            DashSet<SentenceId>,
    /// Per-root recorded sub-sentence ids.  Read via `subs_of`.
    sentence_subs:   DashMap<SentenceId, Vec<SentenceId>>
}

/// Serializable snapshot of [`SentenceSide`] for whole-cache persistence.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct SentenceSideSnapshot {
    forward:         Vec<(u64, Vec<SentenceId>)>,
    source_overflow: Vec<(SentenceId, u32)>,
    parent_overflow: Vec<(SentenceId, u32)>,
    scope_counter:   u64,
    roots:           Vec<SentenceId>,
    subs:            Vec<SentenceId>,
    sentence_subs:   Vec<(SentenceId, Vec<SentenceId>)>
}

/// Behavior for the `syntactic::sentences` store.
#[derive(Debug, Default)]
pub(crate) struct SentenceCache;

impl EagerMapBehavior for SentenceCache {
    type Parent = SyntacticLayer;
    type Key    = SentenceId;
    type Value  = Arc<Sentence>;
    type Side   = SentenceSide;
    type SideSnapshot = SentenceSideSnapshot;

    const NAME: &'static str = "syntactic::sentences";

    fn snapshot_side(&self, side: &SentenceSide) -> SentenceSideSnapshot {
        SentenceSideSnapshot {
            forward:         side.forward.iter().map(|e| (*e.key(), e.value().to_vec())).collect(),
            source_overflow: side.source_overflow.iter().map(|e| (*e.key(), *e.value())).collect(),
            parent_overflow: side.parent_overflow.iter().map(|e| (*e.key(), *e.value())).collect(),
            scope_counter:   side.scope_counter.load(Ordering::Relaxed),
            roots:           side.roots.iter().map(|r| *r).collect(),
            subs:            side.subs.iter().map(|r| *r).collect(),
            sentence_subs:   side.sentence_subs.iter().map(|e| (*e.key(), e.value().clone())).collect(),
        }
    }

    fn restore_side(&self, side: &SentenceSide, snap: SentenceSideSnapshot) {
        for (fp, sids) in snap.forward         { side.forward.insert(fp, sids.into_iter().collect()); }
        for (sid, n) in snap.source_overflow   { side.source_overflow.insert(sid, n); }
        for (sid, n) in snap.parent_overflow   { side.parent_overflow.insert(sid, n); }
        side.scope_counter.store(snap.scope_counter, Ordering::Relaxed);
        for sid in snap.roots { side.roots.insert(sid); }
        for sid in snap.subs  { side.subs.insert(sid); }
        for (sid, n) in snap.sentence_subs   { side.sentence_subs.insert(sid, n); }
    }

    fn consumes(&self) -> &'static [EventKind] {
        &[EventKind::FormulaAdded, EventKind::FormulaRemoved, EventKind::FormulaReferenced]
    }

    fn produces(&self) -> &'static [EventKind] {
        &[EventKind::RootAdded, EventKind::RootRemoved, EventKind::SentencesChanged,
          EventKind::SessionReferenced]
    }

    // `react` resolves fingerprints against the source store, so the source
    // reactor must run first.
    fn reads(&self) -> &'static [&'static str] {
        &[SourceCache::NAME]
    }

    fn react(
        &self,
        parent:  &SyntacticLayer,
        events:  &[&Event],
        // The build/removal methods reach the keyed map + side through
        // `parent.store` (the whole `EagerMap`), so the split views are unused.
        _store:  &EntryCache<SentenceId, Arc<Sentence>>,
        _side:   &SentenceSide,
    ) -> Vec<Event> {
        let mut out: Vec<Event> = Vec::new();
        for event in events {
            if let Event::FormulaAdded { node: hash, session } = event {
                let Some(raw) = parent.source.get(hash) else {
                    continue // removed between event emission and now
                };
                let session = session.clone();

                let normalized: Vec<_> = crate::parse::macros::expand_node(raw)
                    .iter()
                    .flat_map(crate::parse::macros::normalize_ast)
                    .collect();
                let mut scope_sids: Vec<SentenceId> = Vec::new();
                for ast in &normalized {
                    let (root, scope) = parent.sentences.append_root_sentence(
                        *hash, &session, ast, &parent.symbols, &parent.sessions);
                    if let Some(sid) = root {
                        out.push(Event::RootAdded { sid });
                    }
                    if let Some(sid) = scope {
                        scope_sids.push(sid);
                    }
                }
                // Re-assert into a session that didn't own these roots: no
                // `RootAdded` fired, so notify scope-bearing indices directly.
                if !scope_sids.is_empty() {
                    out.push(Event::SessionReferenced {
                        session: session.to_string(),
                        sids:    scope_sids,
                    });
                }
            }
            else if let Event::FormulaRemoved { node: hash } = event {
                // Symbol pruning spans two caches: collect the still-referenced
                // symbols from the sentence store, then evict the rest from the
                // separate symbol store.
                let removed = parent.sentences.remove_hash(*hash);
                if !removed.is_empty() {
                    let referenced  = parent.sentences.referenced_symbols();
                    let removed_syms = parent.symbols.retain_referenced(&referenced);
                    let first_sid = removed.first().map(|r| r.sid);
                    for r in removed {
                        out.push(Event::RootRemoved { sid: r.sid, sentences: r.sentences });
                    }
                    if let Some(sid) = first_sid {
                        if !removed_syms.is_empty() {
                            // Orphaned symbols are batch-global; attribute them to
                            // the first removed root.
                            out.push(Event::SymbolsRetracted {
                                sid,
                                syms: removed_syms.into_iter().collect(),
                            });
                        }
                    }
                }
            }
            else if let Event::FormulaReferenced { node: hash, session } = event {
                // A session newly references an already-ingested fingerprint.
                // Resolve it to the root sids it produced, record this session's
                // membership, and surface the newly-owned roots so scope-bearing
                // indices pick up the added scope.
                let sids: Vec<SentenceId> = parent.sentences.side().forward
                    .get(hash).map(|r| r.value().to_vec()).unwrap_or_default();
                let mut scope_sids: Vec<SentenceId> = Vec::new();
                for sid in sids {
                    if parent.sessions.register(session, sid) {
                        scope_sids.push(sid);
                    }
                }
                if !scope_sids.is_empty() {
                    out.push(Event::SessionReferenced {
                        session: session.to_string(),
                        sids:    scope_sids,
                    });
                }
            }
        }
        out
    }
}

impl SyntacticLayer {
    /// The number of root sentences.
    #[inline]
    pub(crate) fn num_roots(&self) -> usize {
        self.sentences.side().roots.len()
    }

    /// Return true if `sid` is a known sentence.
    #[inline]
    pub(crate) fn has_sentence(&self, sid: SentenceId) -> bool {
        self.sentences.get(&sid).is_some()
    }

    /// Fetch the sentence with the given sid, if it exists.  Returns a cheap
    /// `Arc` clone.
    #[inline]
    pub(crate) fn sentence(&self, sid: SentenceId) -> Option<Arc<Sentence>> {
        self.sentences.entries().get(&sid)
    }

    /// The sub-sentence ids: every sentence referenced as a sub by ≥1 parent.
    /// Under content-addressing a sentence can be *both* a root and a sub.
    pub(crate) fn sub_sentences(&self) -> HashSet<SentenceId> {
        self.sentences.side().subs.iter().map(|r| *r).collect()
    }

    /// Every root sentence id (source-backed roots).
    pub(crate) fn root_sids(&self) -> Vec<SentenceId> {
        self.sentences.side().roots.iter().map(|r| *r).collect()
    }

    /// The root sentence ids a source fingerprint produced (the `forward`
    /// `fingerprint -> roots` map).  Empty if the fingerprint produced nothing.
    pub(crate) fn roots_of_fingerprint(&self, fp: u64) -> Vec<SentenceId> {
        self.sentences.side().forward.get(&fp).map(|s| s.to_vec()).unwrap_or_default()
    }

    /// The source fingerprints that produced `sid` — the inverse of `forward`.
    /// Linear scan (cold paths only, e.g. display / provenance).
    pub(crate) fn fingerprints_producing(&self, sid: SentenceId) -> Vec<u64> {
        self.sentences.side().forward.iter()
            .filter(|e| e.value().contains(&sid))
            .map(|e| *e.key())
            .collect()
    }

    /// A snapshot of the whole `forward` map (`fingerprint -> roots`), for
    /// bulk provenance.
    pub(crate) fn fingerprint_roots(&self) -> Vec<(u64, Vec<SentenceId>)> {
        self.sentences.side().forward.iter()
            .map(|e| (*e.key(), e.value().to_vec()))
            .collect()
    }

    /// The recorded sub-sentence ids of `root` (its `Element::Sub`
    /// descendents), or `None` when the root has no recorded subs.
    pub(crate) fn subs_of(&self, root: SentenceId) -> Option<Vec<SentenceId>> {
        self.sentences.side().sentence_subs.get(&root).map(|v| v.clone())
    }
}

impl EagerMap<SentenceCache> {
    /// Append a single already-parsed, fully normalized (macro-expanded + CAF)
    /// root AST node, recording its source provenance under `hash` / `session`.
    ///
    /// The id is the content hash, so an identical concrete fact resolves to the
    /// same id instead of duplicating.  Returns `(root_added, scope_added)`:
    /// `root_added` is `Some` only when the sentence newly *became* a root; a
    /// repeat source for an existing root is folded into the refcount, but if it
    /// brings a *new session*, `scope_added` is `Some`.
    pub(crate) fn append_root_sentence(
        &self,
        hash:     u64,      // content fingerprint of the source AST
        session:  &str,     // the ingest session tag this formula arrived under
        node:     &AstNode, // a fully normalized root AST node
        symbols:  &EagerMap<SymbolCache>,
        sessions: &EagerMap<SessionCache>,
    ) -> (Option<SentenceId>, Option<SentenceId>) {
        if !matches!(node, AstNode::List { .. }) { return (None, None); }

        // Build the root, a post-order list of every sub-sentence (children
        // before parents), and the symbols mentioned.  Ids are content hashes,
        // so nothing is interned yet.
        let ctx = ScopeCtx::new(self.side().scope_counter.clone());
        let Some((root_sent, sub_sents, syms)) = Sentence::from_node(node, &ctx) else {
            return (None, None);
        };

        // Intern the symbols (idempotent; keyed by `hash(name)`).
        for sym in syms {
            symbols.intern(sym);
        }

        // Intern sub-sentences (children first), registering each newly-interned
        // sentence's parent→sub edges exactly once so removal can refcount shared
        // subs.  The collected post-order id list is the root's transitive sub
        // set, recorded in `sentence_subs` below.
        let mut descendents = Vec::with_capacity(sub_sents.len());
        for sent in sub_sents {
            let (sid, is_new) = self.intern_sentence(sent);
            if is_new { self.register_sub_edges(sid); }
            descendents.push(sid);
        }

        let (root_sid, is_new) = self.intern_sentence(root_sent);
        if is_new {
            self.register_sub_edges(root_sid);
            self.assign_var_indices(root_sid);
        }

        self.side().forward.entry(hash).or_default().push(root_sid);
        let newly_root = self.side().roots.insert(root_sid);
        if newly_root {
            // Keyed off root status (not the body's `is_new`) so a sentence that
            // re-becomes a root after surviving as a shared sub is repopulated.
            self.side().sentence_subs.insert(root_sid, descendents);
        } else {
            *self.side().source_overflow.entry(root_sid).or_insert(1) += 1;
        }

        // Record session membership for every source (including dedup hits) so
        // eviction sees all sessions.
        let session_is_new = sessions.register(session, root_sid);

        crate::log!(Trace, "sigmakee_rs_core::syntactic", format!(
            "registered root id={root_sid:#x} (fingerprint={hash:#x}, new={is_new}, newly_root={newly_root})"));

        (
            newly_root.then_some(root_sid),
            (!newly_root && session_is_new).then_some(root_sid),
        )
    }

    /// Assign each distinct scoped variable in the root formula `root_sid` a
    /// 0-based `var_index`, shared by all its occurrences and across nested
    /// sub-sentences, in first-appearance (pre-order) order.
    pub(crate) fn assign_var_indices(&self, root_sid: SentenceId) {
        let mut map:  HashMap<SymbolId, u32> = HashMap::new();
        let mut next: u32 = 0;
        self.stamp_var_indices(root_sid, &mut map, &mut next);
    }

    /// Pre-order walk helper for [`Self::assign_var_indices`].
    fn stamp_var_indices(
        &self,
        sid:  SentenceId,
        map:  &mut HashMap<SymbolId, u32>,
        next: &mut u32,
    ) {
        /// One variable slot or sub-sentence in element order.
        enum Item { Var(usize, SymbolId), Sub(SentenceId) }

        // Snapshot the relevant elements (in order) from the Arc so no map guard
        // is held across the mutation / recursion below.
        let Some(sentence) = self.get(&sid) else { return };
        let items: Vec<Item> = sentence.elements.iter().enumerate()
            .filter_map(|(i, el)| match el {
                Element::Variable { id, .. } => Some(Item::Var(i, *id)),
                Element::Sub(sub) => Some(Item::Sub(*sub)),
                _ => None,
            })
            .collect();
        drop(sentence);

        for item in items {
            match item {
                Item::Var(i, id) => {
                    let vi = *map.entry(id).or_insert_with(|| { let v = *next; *next += 1; v });
                    self.entries().modify_entry(sid, |arc| {
                        if let Some(Element::Variable { var_index, .. }) =
                            Arc::make_mut(arc).elements.get_mut(i)
                        {
                            *var_index = vi;
                        }
                    });
                }
                Item::Sub(sub) => self.stamp_var_indices(sub, map, next),
            }
        }
    }

    /// Intern a parent-less sentence built directly from `elements`, returning
    /// its content-hash id.  Idempotent via [`Self::intern_sentence`].
    pub(crate) fn push_sentence(&self, elements: ElementVec) -> SentenceId {
        self.intern_sentence(Sentence { parent: Vec::new(), elements }).0
    }

    /// Intern `sentence` under its content-hash id, returning `(id, is_new)`.
    /// Idempotent: identical structure (ignoring `span` / `var_index`) resolves
    /// to the existing id with `is_new == false`.  Debug-asserts against a
    /// 64-bit collision between two structurally-distinct sentences.
    fn intern_sentence(&self, sentence: Sentence) -> (SentenceId, bool) {
        let id = sentence.hash();
        if let Some(_existing) = self.entries().get(&id) {
            debug_assert!(
                sentence == *_existing,
                "SentenceId content-hash collision {id:#x}",
            );
            (id, false)
        } else {
            self.entries().update(id, Arc::new(sentence));
            (id, true)
        }
    }

    /// Register one `Element::Sub` reference to `sid` (once per sub element as a
    /// parent is built).  `subs` holds ids with ≥1 parent; `parent_overflow` is
    /// the sparse >1 count (absent ⇒ exactly one).
    fn add_sub_use(&self, sid: SentenceId) {
        if !self.side().subs.insert(sid) {
            *self.side().parent_overflow.entry(sid).or_insert(1) += 1;
        }
    }

    /// Outgoing `Element::Sub` children of `sid` (each is one sub-reference).
    fn sub_children(&self, sid: SentenceId) -> Vec<SentenceId> {
        self.get(&sid)
            .map(|s| s.elements.iter().filter_map(|e| match e {
                Element::Sub(c) => Some(*c),
                _ => None,
            }).collect())
            .unwrap_or_default()
    }

    /// Register the parent→sub edges of a *newly interned* sentence (one
    /// [`Self::add_sub_use`] per `Element::Sub`), so removal can refcount them.
    fn register_sub_edges(&self, sid: SentenceId) {
        for c in self.sub_children(sid) {
            self.add_sub_use(c);
        }
    }

    /// Add the direct (non-recursive) `Element::Symbol` ids of the single
    /// sentence `sid` to `out`.
    fn collect_own_symbols(&self, sid: SentenceId, out: &mut HashSet<SymbolId>) {
        let Some(sentence) = self.get(&sid) else { return };
        for el in &sentence.elements {
            if let Element::Symbol(sym) = el { out.insert(sym.id()); }
        }
    }

    /// The set of symbol ids still referenced by some live root sentence — the
    /// symbol store uses it to evict orphans after a removal batch.  A root
    /// contributes its own symbols plus those of every recorded descendent.
    pub(crate) fn referenced_symbols(&self) -> HashSet<SymbolId> {
        let mut referenced = HashSet::new();
        // Snapshot the root ids so no `DashSet` guard is held across the walks.
        let roots: Vec<SentenceId> = self.side().roots.iter().map(|r| *r).collect();
        for sid in roots {
            self.collect_own_symbols(sid, &mut referenced);
            // Clone the descendent list out so no `DashMap` guard is held across
            // the per-sentence lookups below.
            let subs: Vec<SentenceId> = self.side().sentence_subs
                .get(&sid).map(|r| r.clone()).unwrap_or_default();
            for sub in subs {
                self.collect_own_symbols(sub, &mut referenced);
            }
        }
        referenced
    }

    /// Drop a source formula's contribution, by content fingerprint.
    ///
    /// A root loses root status only when its *last* source goes
    /// (`source_overflow`, absent ⇒ one).  Returns the sentences that ceased to
    /// be roots (the `RootRemoved` set), each tagged with the head + transitive
    /// symbol set its indices need — captured *before* the body is reclaimed.
    pub(crate) fn remove_hash(&self, hash: u64) -> Vec<RemovedRoot> {
        let Some((_, sids)) = self.side().forward.remove(&hash) else { return Vec::new() };

        let mut removed_roots = Vec::new();
        for sid in sids {
            // Holding a `get`/`get_mut` guard across an `insert`/`remove` on the
            // same `DashMap` deadlocks; copy the count out first.
            let cur = self.side().source_overflow.get(&sid).map(|r| *r);
            let last_source = match cur {
                Some(n) if n > 2 => { self.side().source_overflow.insert(sid, n - 1); false }
                Some(_)          => { self.side().source_overflow.remove(&sid); false } // 2 → 1
                None             => true, // was the sole source
            };
            if last_source {
                self.side().roots.remove(&sid);
                self.side().sentence_subs.remove(&sid);
                // If the root is *also* a sub (a surviving parent still
                // references it), `collect_if_unreferenced` won't evict it, so
                // capture a clone to hand consumers the body anyway.
                let surviving_root = if self.side().subs.contains(&sid) {
                    self.get(&sid).map(|a| (*a).clone())
                } else {
                    None
                };
                let mut sentences = Vec::new();
                self.collect_if_unreferenced(sid, &mut sentences);
                if let Some(root) = surviving_root {
                    sentences.insert(0, root);
                }
                removed_roots.push(RemovedRoot { sid, sentences });
            }
        }
        removed_roots
    }

    /// Reclaim `sid`'s body iff it is referenced in *neither* direction — not a
    /// root (no source) and not a sub (no parent).  Cascades into its children:
    /// dropping the body releases one sub-reference per `Element::Sub`.
    fn collect_if_unreferenced(&self, sid: SentenceId, out: &mut Vec<Sentence>) {
        if self.side().roots.contains(&sid) || self.side().subs.contains(&sid) { return; }
        let Some(sentence) = self.get(&sid) else { return };
        self.entries().evict_keys(&[sid]);
        crate::log!(Trace, "sigmakee_rs_core::syntactic", format!("collected sentence sid={sid:#x}"));
        // The held `Arc` keeps the body alive after eviction so we can read its
        // children; no map guard is held across the recursive decrement.
        let children: Vec<SentenceId> = sentence.elements.iter()
            .filter_map(|el| match el {
                Element::Sub(c) => Some(*c),
                _ => None,
            })
            .collect();
        out.push(Arc::try_unwrap(sentence).unwrap_or_else(|a| (*a).clone()));
        for child in children {
            self.dec_sub_use(child, out);
        }
    }

    /// Release one `Element::Sub` reference to `sid`.  When the last parent
    /// reference goes, `sid` ceases to be a sub and is collected if also not a
    /// root.
    fn dec_sub_use(&self, sid: SentenceId, out: &mut Vec<Sentence>) {
        let cur = self.side().parent_overflow.get(&sid).map(|r| *r);
        match cur {
            Some(n) if n > 2 => { self.side().parent_overflow.insert(sid, n - 1); }
            Some(_)          => { self.side().parent_overflow.remove(&sid); } // 2 → 1
            None => {
                // Was the sole parent reference.
                self.side().subs.remove(&sid);
                self.collect_if_unreferenced(sid, out);
            }
        }
    }
}

/// A root that ceased to be a root, plus the data its downstream indices need
/// to de-index it — captured before the body (and any exclusively-owned subs)
/// are torn out of the store, so consumers never read freed state.
pub(crate) struct RemovedRoot {
    /// The sentence id that is no longer a root.
    pub sid:       SentenceId,
    /// The sentence bodies removed by this root's retraction — the root itself
    /// plus any sub-sentences it orphaned — moved out of the store so downstream
    /// reactors can read the head / symbols / edge straight from the body.  The
    /// root (`hash() == sid`) is `sentences[0]`.
    pub sentences: Vec<Sentence>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Variables are interned with scope-qualified names (`X__<scope>`) during
    /// the sentence build, so `?X` in two distinct quantifier scopes yields two
    /// distinct symbols rather than aliasing to one.
    #[test]
    fn variables_have_scope_qualified_occurrences() {
        let mut store = SyntacticLayer::default();
        store.load_kif("(forall (?X) (P ?X))\n(forall (?X) (Q ?X))", "t.kif");
        let xs: Vec<String> = store.symbols.snapshot().into_values()
            .map(|sym| sym.name().to_string())
            .filter(|k| k.starts_with("X__"))
            .collect();
        assert!(xs.len() >= 2, "expected distinct X__<scope> ids, got {:?}", xs);
    }
}
