//! Source AST store and the source reactor.
//!
//! Consumes `SourceAdded` (raw file contents), parses them, and maintains the
//! canonical AST node per formula keyed by content fingerprint. Emits
//! `FormulaAdded` / `FormulaRemoved` (by fingerprint) for the downstream
//! sentence reactor, and `Diagnostic` events for parse errors and duplicate
//! formulas.
//!
//! File ingestion is a replacement: re-ingesting a file diffs its previous
//! fingerprint set against the new parse. A formula's reference set
//! (`references[hash]`) holds the span of every occurrence across all files —
//! it is both the cross-file reference count (gone ⇒ empty) and the set of
//! debug locations. `FormulaAdded` fires when that set goes empty→non-empty
//! (first occurrence anywhere); `FormulaRemoved` fires only when it goes
//! non-empty→empty (last occurrence gone).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::parse::doc::DocItem;
use crate::{AstNode, Diagnostic, Severity, Span};
use crate::cache::{EntryCache, EagerMapBehavior};
use crate::cache::events::{Event, EventKind};
use crate::syntactic::SyntacticLayer;

mod side;
#[cfg(test)]
mod tests;

pub(crate) use side::{SourceSide, SourceSideSnapshot};
/// Warning event for a formula that appears more than once in a single file.
fn duplicate_warning(dup: &Span, first: &Span) -> Event {
    Event::Diagnostic(Diagnostic {
        kind:          "ingest",
        range:         dup.clone(),
        severity:      Severity::Warning,
        code:          "duplicate-formula",
        message:       format!(
            "duplicate formula ignored; first occurrence at {}:{}",
            first.file, first.line,
        ),
        related:       Vec::new(),
        sids:          Vec::new(),
        highlight_arg: -1,
        highlight_var: None,
    })
}

/// Dedup one parse by fingerprint: keep the first occurrence of each formula,
/// and emit a `duplicate-formula` warning (citing both spans) for any repeat
/// within the same parse.  Pure — no store access, so it runs outside the lock.
///
/// Returns the deduped map AND the first-occurrence hashes in FILE ORDER —
/// `current` alone (a `HashMap`, `RandomState`) cannot answer "what order were
/// these seen in" without re-scrambling it; `order` is what lets
/// [`apply_source`] emit `FormulaAdded`/`FormulaReferenced` in source order
/// instead of process-random hash-bucket order.  That order ultimately seeds
/// each root's variable-scope-disambiguation counter
/// ([`crate::syntactic::sentence::ScopeCtx`]), which is baked into the
/// sentence's content hash — so an unordered emission here would make the
/// SAME axiom text hash differently from run to run, breaking content
/// addressing (and, downstream, native-prover search reproducibility).
fn dedup_parse(
    parsed: Vec<(u64, AstNode, Span)>,
) -> (HashMap<u64, (AstNode, Span)>, Vec<u64>, Vec<Event>) {
    let mut current: HashMap<u64, (AstNode, Span)> = HashMap::with_capacity(parsed.len());
    let mut order: Vec<u64> = Vec::with_capacity(parsed.len());
    let mut warnings = Vec::new();
    for (hash, node, span) in parsed {
        match current.get(&hash) {
            Some((_, first)) => warnings.push(duplicate_warning(&span, first)),
            None => {
                current.insert(hash, (node, span));
                order.push(hash);
            }
        }
    }
    (current, order, warnings)
}

/// Apply one file's deduped parse to the store as a *replacement* of that
/// file's prior contents, returning the `FormulaAdded` / `FormulaRemoved`
/// follow-ons.  `FormulaAdded` fires when a fingerprint's reference set goes
/// empty→non-empty (first occurrence anywhere); `FormulaRemoved` fires only
/// when it goes non-empty→empty (last occurrence gone).
fn apply_source(
    store:    &EntryCache<u64, AstNode>,
    side:     &SourceSide,
    file_key: &str,
    session:  &Arc<String>,
    current:  HashMap<u64, (AstNode, Span)>,
    // First-occurrence hashes in FILE order (see `dedup_parse`) — iterated
    // instead of `current`'s own (RandomState) key order so `FormulaAdded`
    // fires in a KB-content-determined sequence.
    order:    &[u64],
    // Fingerprints whose removal must be deferred (staged update of a promoted
    // axiom): kept live and in the file's membership, recorded in the recycle
    // bin, no `FormulaRemoved`. Empty for ordinary (immediate) ingests.
    protect:  &HashSet<u64>,
) -> Vec<Event> {
    let mut evs = Vec::new();
    // Fingerprints carried over unchanged (present in both prev and new parse),
    // surfaced as a single batched `FormulasUnchanged`.
    let mut retained: Vec<u64> = Vec::new();
    let mut current_hashes: HashSet<u64> = current.keys().copied().collect();
    // The file's previous membership, snapshotted before we mutate.
    let prev: HashSet<u64> = side.file_hashes.get(file_key)
        .map(|r| r.value().clone())
        .unwrap_or_default();

    for hash in order.iter().copied() {
        let (node, span) = &current[&hash];
        let mut refs = side.references.entry(hash).or_default();
        if prev.contains(&hash) {
            // Retained (unchanged or moved within this file): refresh this
            // file's span; the reference set stays non-empty, so no KB event.
            refs.retain(|sp| sp.file.as_str() != file_key);
            refs.insert(span.clone());
            // Present in the new content, so clear any stale "pending deletion"
            // mark a prior staged reload left in the recycle bin.
            side.unrecycle(file_key, hash);
            retained.push(hash);
        } else {
            let was_empty = refs.is_empty();
            refs.insert(span.clone());
            if was_empty {
                // First occurrence anywhere. `nodes` are keyed by content
                // fingerprint, so the canonical AST is immutable; insert once.
                let _ = store.get_or_insert_with(hash, |_| node.clone());
                evs.push(Event::FormulaAdded { node: hash, session: session.clone() });
            } else {
                // Already referenced elsewhere, so no `FormulaAdded` — but this
                // session newly references it, and the session/scope indices need
                // to learn that ownership.
                evs.push(Event::FormulaReferenced { node: hash, session: session.clone() });
            }
        }
    }

    // Formulas this file dropped: retract this file's span, and emit
    // FormulaRemoved ONLY when the last reference is gone.
    let dropped: Vec<u64> = prev.difference(&current_hashes).copied().collect();
    let mut deferred: Vec<u64> = Vec::new();
    for hash in dropped {
        // Staged removal of a promoted axiom: defer, leaving the formula intact
        // (ref + membership) and recording it in the recycle bin instead.
        if protect.contains(&hash) {
            side.recycle(file_key, hash);
            deferred.push(hash);
            continue;
        }
        // Decide emptiness under the entry guard, then drop it before `remove`:
        // holding a guard across a same-key DashMap op would deadlock.
        let now_empty = match side.references.get_mut(&hash) {
            Some(mut refs) => {
                refs.retain(|sp| sp.file.as_str() != file_key);
                refs.is_empty()
            }
            None => false,
        };
        if now_empty {
            side.references.remove(&hash);
            store.evict_keys(&[hash]);
            evs.push(Event::FormulaRemoved { node: hash });
        }
    }
    // Deferred (recycled) formulas stay part of the file's membership so a later
    // reconcile still sees them; they leave only on `accept_kif_update`.
    current_hashes.extend(deferred.iter().copied());

    // The file's set is now exactly this parse.
    side.file_hashes.insert(file_key.to_string(), current_hashes);
    // Record the source under its session's eviction group so `flush_session`
    // can reconcile every source the session produced.
    side.session_sources.entry(session.to_string()).or_default().insert(file_key.to_string());
    if !retained.is_empty() {
        evs.push(Event::FormulasUnchanged { nodes: retained });
    }
    if !deferred.is_empty() {
        evs.push(Event::FormulasRecycled { nodes: deferred });
    }
    evs
}

/// Behavior for the `syntactic::source` AST store / reactor.
#[derive(Debug, Default)]
pub(crate) struct SourceCache;

impl EagerMapBehavior for SourceCache {
    type Parent = SyntacticLayer;
    type Key    = u64;
    type Value  = AstNode;
    type Side   = SourceSide;
    type SideSnapshot = SourceSideSnapshot;

    const NAME: &'static str = "syntactic::source";

    fn consumes(&self) -> &'static [EventKind] {
        &[EventKind::SourceAdded]
    }

    fn produces(&self) -> &'static [EventKind] {
        &[EventKind::FormulaAdded, EventKind::FormulaRemoved, EventKind::FormulaReferenced]
    }

    fn snapshot_side(&self, side: &SourceSide) -> SourceSideSnapshot {
        SourceSideSnapshot {
            file_hashes: side.file_hashes.iter().map(|e| (e.key().clone(), e.value().clone())).collect(),
            references:  side.references.iter().map(|e| (e.key().clone(), e.value().clone())).collect(),
        }
    }

    fn restore_side(&self, side: &SourceSide, snap: SourceSideSnapshot) {
        for (k, v) in snap.file_hashes { side.file_hashes.insert(k, v); }
        for (k, v) in snap.references  { side.references.insert(k, v); }
    }

    fn react(
        &self,
        parent:  &SyntacticLayer,
        events:  &[&Event],
        store:   &EntryCache<Self::Key, Self::Value>,
        side:    &SourceSide
    ) -> Vec<Event> {
        events.iter().filter_map(|event| {
            match event {
                Event::SourceAdded { file, session, staged } => {
                    let parser = &file.parser;
                    // Source identity used to reconcile a source against its own
                    // previous contents. File ingests carry a real `path`; inline
                    // sources (`tell`, and the empty truncation `flush_session`
                    // re-ingests) carry an empty path but the same `session`, so
                    // inline content is keyed by session to keep distinct sessions
                    // isolated.
                    let file_key = {
                        let path = file.path.to_str().unwrap_or("");
                        if !path.is_empty() {
                            path.to_string()
                        } else if !file.name.is_empty() {
                            file.name.clone()
                        } else {
                            "inline".to_string()
                        }
                    };
                    if matches!(file.origin, crate::types::FileOrigin::Inline) {
                        side.mark_inline(&file_key);
                    }
                    let mut out = Vec::new();
                    let nodes: Vec<AstNode> = if file.prebuilt.is_none() {
                        // -- Outside the lock: parse + fingerprint + dedup ----------
                        let (nodes, errs) = parser.parse(file.contents.as_str(), &file_key);
                        out.extend(errs
                            .iter()
                            .map(|(_, e)| Event::Diagnostic(e.to_diagnostic())));
                        nodes.into_iter().filter_map(|doc| {
                            match doc {
                                DocItem::Stmt(node) => Some(node),
                                _ => None
                            }
                        }).collect()
                    } else {
                        file.prebuilt.clone().unwrap()
                    };

                    let parsed = nodes.into_iter()
                        .map(|node| {
                            // Strip top-level statement metadata (role/name/source
                            // from a dialect parser) so only the bare formula is
                            // fingerprinted, stored, and built into a sentence.
                            let node = node.strip_annotation();
                            let hash = node.fingerprint();
                            let span = node.span().clone();
                            (hash, node, span)
                        })
                        .collect();
                    let (current, order, mut dup_warnings) = dedup_parse(parsed);
                    out.append(&mut dup_warnings);

                    // The new parse, captured before `apply_source` mutates it,
                    // for tombstone bookkeeping.
                    let new_fps: HashSet<u64> = current.keys().copied().collect();
                    // For a staged file update, defer removal of any promoted axiom
                    // currently in this source: those go to the recycle bin (commit)
                    // and a session tombstone (scoped view).
                    let protect: HashSet<u64> = if *staged {
                        side.fingerprints_of(&file_key).into_iter()
                            .filter(|fp| parent.roots_of_fingerprint(*fp)
                                .iter().any(|s| parent.sessions.is_axiom(*s)))
                            .collect()
                    } else {
                        HashSet::new()
                    };

                    // -- Under the lock: pure map ops on the store --------------
                    let session = session.clone();
                    let follow_on = apply_source(store, side, &file_key, &session, current, &order, &protect);

                    // -- Negative overlay (session tombstones), scoped to this
                    //    review session. Operates only on fingerprints that already
                    //    existed (deferred axioms + retained formulas); brand-new
                    //    additions have no `forward` entry yet.
                    let sid = crate::syntactic::caches::session::session_id(&session);
                    let sids_of = |fp: u64| parent.roots_of_fingerprint(fp);
                    if *staged {
                        // Promoted axioms this update drops → tombstone in `session`.
                        for fp in protect.iter().filter(|fp| !new_fps.contains(fp)) {
                            for s in sids_of(*fp) { parent.sessions.add_tombstone(sid, s); }
                        }
                    }
                    // A formula present in the new content is kept/re-asserted, so
                    // it leaves this session's tombstones.
                    if parent.sessions.has_tombstones(sid) {
                        for fp in &new_fps {
                            for s in sids_of(*fp) { parent.sessions.untombstone(sid, s); }
                        }
                    }

                    out.extend(follow_on);
                    Some(out)
                }
                _ => None,
            }
        })
        .flatten()
        .collect()
    }
}

impl SyntacticLayer {
    /// Whether `session` holds any inline (`tell`) assertion.  Inline content is
    /// never liftable, so `make_session_axiomatic` rejects such a session.
    pub(crate) fn session_has_inline_assertions(&self, session: &str) -> bool {
        let side = self.source.side();
        side.sources_of_session(session).iter().any(|k| side.is_inline_source(k))
    }

    /// Whether `source_key` currently contributes any *axiom* (promoted) root —
    /// used by `flush_session` to leave a source intact when flushing would
    /// otherwise retract a promoted sentence (axioms survive session flush).
    pub(crate) fn source_produces_axiom(&self, source_key: &str) -> bool {
        for fp in self.source.side().fingerprints_of(source_key) {
            if self.roots_of_fingerprint(fp).iter().any(|sid| self.sessions.is_axiom(*sid)) {
                return true;
            }
        }
        false
    }

    /// A fresh, unique inline-source key (`__inline(N)__`) for a `tell`.
    pub(crate) fn next_inline_source_key(&self) -> String {
        self.source.side().next_inline_key()
    }

    /// The fingerprints currently recycled (staged for removal) for `source_key`.
    pub(crate) fn recycled_fingerprints_of(&self, source_key: &str) -> Vec<u64> {
        self.source.side().recycled_of(source_key)
    }

    /// Drop one `source_key → fp` reference; returns `true` when the last
    /// reference is gone (the formula is now unreferenced KB-wide).
    pub(crate) fn drop_source_ref(&self, source_key: &str, fp: u64) -> bool {
        self.source.side().drop_ref(source_key, fp)
    }

    /// Clear `source_key`'s recycle bin (its staged removals).
    pub(crate) fn clear_source_recycle(&self, source_key: &str) {
        self.source.side().clear_recycle(source_key)
    }

    /// Forget all source bookkeeping for `session`.
    pub(crate) fn forget_source_session(&self, session: &str) {
        self.source.side().forget_session(session)
    }

    /// The source keys belonging to `session`.
    pub(crate) fn sources_of_session(&self, session: &str) -> Vec<String> {
        self.source.side().sources_of_session(session)
    }

    /// The canonical source `AstNode` for a formula fingerprint, if still
    /// present.
    pub(crate) fn source_ast(&self, fp: u64) -> Option<AstNode> {
        self.source.get(&fp)
    }

    /// The source formula whose canonical range covers byte `offset` in `file`.
    ///
    /// The store keeps one canonical `AstNode` per fingerprint (carrying its
    /// first occurrence's spans), so this resolves offsets against that first
    /// occurrence: correct for a singly-loaded file, but a formula duplicated
    /// across files is located only at its canonical site.
    pub(crate) fn source_node_at(&self, file: &str, offset: usize) -> Option<AstNode> {
        let mut found = None;
        self.source.entries().for_each(|(_, n)| {
            if found.is_none() {
                let sp = n.span();
                if !sp.is_synthetic() && sp.file == file && offset >= sp.offset && offset < sp.end_offset {
                    found = Some(n.clone());
                }
            }
        });
        found
    }

    /// Every file tag currently loaded (the source store's per-file membership).
    pub(crate) fn source_files(&self) -> Vec<String> {
        self.source.side().file_hashes.iter().map(|e| e.key().clone()).collect()
    }

    /// The content fingerprints a file contributed (its formulas).
    pub(crate) fn file_fingerprints(&self, file: &str) -> Vec<u64> {
        self.source.side().file_hashes.get(file).map(|set| set.iter().copied().collect()).unwrap_or_default()
    }

    /// The root sentence ids a file produced — its fingerprints resolved through
    /// the sentence store's `forward` (`fingerprint → roots`) map.
    pub(crate) fn file_root_sids(&self, file: &str) -> Vec<crate::SentenceId> {
        let mut sids = Vec::new();
        for fp in self.file_fingerprints(file) {
            sids.extend(self.roots_of_fingerprint(fp));
        }
        sids
    }

    /// The source `AstNode` that produced root `sid` (its first source
    /// fingerprint), if any — the source-side provenance of a stored root.
    pub(crate) fn source_node_of(&self, sid: crate::SentenceId) -> Option<AstNode> {
        let fp = self.fingerprints_producing(sid).into_iter().next()?;
        self.source_ast(fp)
    }

    /// Source range of root `sid` (from its source AST); `None` for synthetic
    /// sentences with no source fingerprint.
    pub(crate) fn source_span_of(&self, sid: crate::SentenceId) -> Option<Span> {
        self.source_node_of(sid).map(|n| n.span().clone())
    }

    /// Source provenance for every root in one pass: walk the
    /// `fingerprint -> roots` map once, resolve each fingerprint's source AST,
    /// and credit it to each root the first time that root is seen.
    ///
    /// Bulk consumers (axiom-source index, man pages) must use this rather than
    /// `source_node_of` per root: that lookup is itself a linear scan over the
    /// forward map, so per-root calls go quadratic in KB size.
    pub(crate) fn root_source_nodes(&self) -> Vec<(crate::SentenceId, AstNode)> {
        let mut seen = std::collections::HashSet::new();
        let mut out  = Vec::new();
        for (fp, sids) in self.fingerprint_roots() {
            let Some(node) = self.source_ast(fp) else { continue };
            for sid in sids {
                if seen.insert(sid) {
                    out.push((sid, node.clone()));
                }
            }
        }
        out
    }
}
