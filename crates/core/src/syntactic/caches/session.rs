//! `syntactic::sessions` — per-session sentence membership and axiom status.
//!
//! A *session* is an ingest tag (a file name, `__query__`, …). This cache maps
//! each session to the root sentences it produced and whether it has been
//! axiomatized.
//!
//!   * **membership** is written imperatively by the sentence build path
//!     (`register`, through a shared `&`).
//!   * **axiom status** is event-driven. `SessionAxiomatized` promotes a
//!     session's members: each newly-promoted sid (not already in the cache-wide
//!     `promoted` set) is emitted as `AxiomsPromoted`, consumed by the
//!     `axiom_index` and `sine` reactors. `SessionRetracted` drops a
//!     non-axiomatic session wholesale; `RootRemoved` clears an individual sid
//!     from every session's membership and the `promoted` set, dropping emptied
//!     sessions.
//!
//! A root is an *axiom* iff it is in the `promoted` set. New sentences are
//! transient until their session is axiomatized.

use std::collections::HashSet;

use dashmap::{DashMap, DashSet};
use serde::{Deserialize, Serialize};

use crate::cache::events::{Event, EventKind};
use crate::cache::{EagerMap, EagerMapBehavior, EntryCache};
use crate::syntactic::SyntacticLayer;
use crate::types::{SentenceId, Symbol};

/// A session identifier: the content hash of the (unique) session name.
pub(crate) type SessionId = u64;

/// Hash a session name to its [`SessionId`].
pub(crate) fn session_id(name: &str) -> SessionId {
    Symbol::hash_name(name)
}

/// Per-session record: the roots it produced and whether it is axiomatized.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct SessionEntry {
    /// Root sentence ids this session produced or references.
    pub sentences:   HashSet<SentenceId>,
    /// `true` once the session has been promoted to axioms. Sticky: a session
    /// never un-axiomatizes (individual axioms still retract via `RootRemoved`).
    pub axiomatized: bool,
}

/// Cache-wide companion state.
#[derive(Debug, Default)]
pub(crate) struct SessionSide {
    /// Every sid currently promoted to an axiom (across all sessions). A sentence
    /// shared across sessions promotes at most once. Cleared per-sid on
    /// `RootRemoved`.
    promoted: DashSet<SentenceId>,
    /// Which session(s) an axiom was promoted from, for lineage. Cleared per-sid
    /// on `RootRemoved`.
    promoted_by: DashMap<SentenceId, HashSet<String>>,
    /// Negative overlay: per session (by [`SessionId`]), the axiom sids a staged
    /// file update would remove. The axiom stays globally live but is hidden from
    /// this session's scoped view, provided no *other* session promoted it (see
    /// `tombstone_active`). Cleared on accept/reject, same-session re-assert, the
    /// sid's `RootRemoved`, and session retract.
    tombstones: DashMap<SessionId, HashSet<SentenceId>>,
}

impl SessionSide {
    /// Record that session `s` (by id) would remove axiom `sid`.
    fn add_tombstone(&self, s: SessionId, sid: SentenceId) {
        self.tombstones.entry(s).or_default().insert(sid);
    }

    /// Drop `sid` from session `s`'s tombstones (re-asserted / committed / kept).
    fn untombstone(&self, s: SessionId, sid: SentenceId) {
        let empty = match self.tombstones.get_mut(&s) {
            Some(mut set) => { set.remove(&sid); set.is_empty() }
            None => false,
        };
        if empty { self.tombstones.remove(&s); }
    }

    /// Drop `sid` from every session's tombstones (the formula is truly gone).
    fn untombstone_everywhere(&self, sid: SentenceId) {
        self.tombstones.iter_mut().for_each(|mut e| { e.remove(&sid); });
        self.tombstones.retain(|_, set| !set.is_empty());
    }

    /// Take and clear session `s`'s tombstones (accept/reject consume them).
    fn take_tombstones(&self, s: SessionId) -> Vec<SentenceId> {
        self.tombstones.remove(&s).map(|(_, set)| set.into_iter().collect()).unwrap_or_default()
    }

    /// Session `s`'s tombstoned sids (without clearing).
    fn tombstoned_sids(&self, s: SessionId) -> Vec<SentenceId> {
        self.tombstones.get(&s).map(|set| set.iter().copied().collect()).unwrap_or_default()
    }

    /// Whether session `s`'s tombstone of `sid` is **active** (would actually
    /// take effect): no session other than `s` promoted `sid`, so committing the
    /// removal would truly drop it from Base.
    fn tombstone_active(&self, s: SessionId, sid: SentenceId) -> bool {
        match self.promoted_by.get(&sid) {
            Some(promoters) => promoters.iter().all(|p| session_id(p) == s),
            None => true,
        }
    }
}

/// Behavior for the `syntactic::sessions` store.
#[derive(Debug, Default)]
pub(crate) struct SessionCache;

impl EagerMapBehavior for SessionCache {
    type Parent = SyntacticLayer;
    type Key    = String;
    type Value  = SessionEntry;
    type Side   = SessionSide;
    type SideSnapshot = Vec<SentenceId>;

    const NAME: &'static str = "syntactic::sessions";

    fn consumes(&self) -> &'static [EventKind] {
        &[
            EventKind::SessionAxiomatized,
            EventKind::SessionRetracted,
            EventKind::RootRemoved,
        ]
    }

    fn produces(&self) -> &'static [EventKind] {
        &[EventKind::AxiomsPromoted]
    }

    fn snapshot_side(&self, side: &SessionSide) -> Vec<SentenceId> {
        side.promoted.iter().map(|r| *r).collect()
    }

    fn restore_side(&self, side: &SessionSide, snap: Vec<SentenceId>) {
        for sid in snap {
            side.promoted.insert(sid);
        }
    }

    fn react(
        &self,
        _parent: &SyntacticLayer,
        events:  &[&Event],
        store:   &EntryCache<String, SessionEntry>,
        side:    &SessionSide,
    ) -> Vec<Event> {
        let mut out = Vec::new();
        for e in events {
            match e {
                // ── Promotion ────────────────────────────────────────────────
                Event::SessionAxiomatized { session } => {
                    // No entry ⇒ nothing to promote. Don't `modify_entry` an
                    // absent key — it `or_default`s a phantom empty session.
                    let Some(entry) = store.get(session) else { continue };
                    // Sorted (not the `HashSet`'s RandomState order): this list
                    // becomes the `AxiomsPromoted` event's `sids`, which drives
                    // both the SInE index's axiom-registration order (tie-break
                    // among same-g_min entries in `sym_to_axioms`) and the
                    // axiom-occurrence index — both must be a pure function of
                    // KB content for the native prover's search to be
                    // reproducible.  SentenceIds are content hashes, so sorting
                    // gives a stable, KB-content-determined order.
                    let mut promoted_now: Vec<SentenceId> = entry.sentences.iter().copied().collect();
                    promoted_now.sort_unstable();
                    let newly: Vec<SentenceId> = promoted_now
                        .iter()
                        .copied()
                        .filter(|sid| side.promoted.insert(*sid))
                        .collect();
                    store.modify_entry(session.clone(), |entry| {
                        entry.axiomatized = true;
                        entry.sentences.clear();
                    });
                    for sid in &promoted_now {
                        side.promoted_by.entry(*sid).or_default().insert(session.clone());
                    }
                    if !newly.is_empty() {
                        out.push(Event::AxiomsPromoted { sids: newly });
                    }
                }

                // ── Wholesale session retraction ─────────────────────────────
                // An axiomatic session must not be dropped wholesale; its axioms
                // only retract individually.
                Event::SessionRetracted { session } => {
                    side.tombstones.remove(&session_id(session));
                    let is_axiom_session =
                        store.get(session).map_or(false, |e| e.axiomatized);
                    if !is_axiom_session {
                        store.evict_keys(&[session.clone()]);
                    }
                }

                // ── Individual retraction ────────────────────────────────────
                Event::RootRemoved { sid, .. } => {
                    side.promoted.remove(sid);
                    side.promoted_by.remove(sid);
                    side.untombstone_everywhere(*sid);
                    let containing: Vec<String> = {
                        let mut v = Vec::new();
                        store.for_each(|(name, entry)| {
                            if entry.sentences.contains(sid) {
                                v.push(name.clone());
                            }
                        });
                        v
                    };
                    for name in containing {
                        store.modify_entry(name.clone(), |entry| {
                            entry.sentences.remove(sid);
                        });
                        if store.get(&name).map_or(false, |e| e.sentences.is_empty()) {
                            store.evict_keys(&[name]);
                        }
                    }
                }

                _ => {}
            }
        }
        out
    }
}

impl EagerMap<SessionCache> {
    /// Record that `session` produced root `sid`.
    ///
    /// Returns `true` iff `session` did not previously reference `sid` — i.e.
    /// this call newly associates the content-addressed root with the session.
    pub(crate) fn register(&self, session: &str, sid: SentenceId) -> bool {
        let mut newly = false;
        self.entries().modify_entry(session.to_string(), |entry| {
            newly = entry.sentences.insert(sid);
        });
        newly
    }

    /// The root sids recorded for `session` (empty if the session is unknown).
    ///
    /// Sorted (not the `HashSet`'s RandomState order): the native prover
    /// registers these sids as SUPPORT clauses in this exact order
    /// (`add_support_root`, via `prove.rs`'s `session_sids`), so an unsorted
    /// return would make the given-clause search depend on process-local
    /// hash seeding instead of KB content. SentenceIds are content hashes,
    /// so sorting gives a stable, KB-content-determined order.
    pub(crate) fn session_sentences(&self, session: &str) -> Vec<SentenceId> {
        let mut v: Vec<SentenceId> = self.entries()
            .get(&session.to_string())
            .map(|e| e.sentences.iter().copied().collect())
            .unwrap_or_default();
        v.sort_unstable();
        v
    }

    /// The root sids of the (unique) session whose name hashes to `sid`.
    ///
    /// Empty for an unknown id.
    pub(crate) fn session_sentences_by_id(&self, sid: SessionId) -> HashSet<SentenceId> {
        let mut out = HashSet::new();
        self.entries().for_each(|(name, entry)| {
            if session_id(name) == sid {
                out.extend(entry.sentences.iter().copied());
            }
        });
        out
    }

    /// The sessions (by [`SessionId`]) that reference root `sid`.
    ///
    /// Empty for an unknown or axiom-only sid.
    pub(crate) fn sessions_of(&self, sid: SentenceId) -> Vec<SessionId> {
        let mut out = Vec::new();
        self.entries().for_each(|(name, entry)| {
            if entry.sentences.contains(&sid) {
                out.push(session_id(name));
            }
        });
        out
    }

    /// `true` if `sid` is currently a promoted axiom; otherwise it is a transient
    /// assertion.
    pub(crate) fn is_axiom(&self, sid: SentenceId) -> bool {
        self.side().promoted.contains(&sid)
    }

    // -- Tombstones (scoped staged removals) — keyed by `SessionId` -----------

    /// Record that session `s` (by id) would remove axiom `sid`.
    pub(crate) fn add_tombstone(&self, s: SessionId, sid: SentenceId) {
        self.side().add_tombstone(s, sid);
    }

    /// Clear `sid` from session `s`'s tombstones (re-asserted in the same session).
    pub(crate) fn untombstone(&self, s: SessionId, sid: SentenceId) {
        self.side().untombstone(s, sid);
    }

    /// Take and clear session `s`'s tombstones (accept/reject).
    pub(crate) fn take_tombstones(&self, s: SessionId) -> Vec<SentenceId> {
        self.side().take_tombstones(s)
    }

    /// Whether session `s` has any tombstone (→ its scoped view is "active" even
    /// with no additive overlay).
    pub(crate) fn has_tombstones(&self, s: SessionId) -> bool {
        self.side().tombstones.get(&s).is_some_and(|set| !set.is_empty())
    }

    /// The sids session `s` actively hides (tombstoned AND the removal would
    /// take effect — no other session promoted them).
    pub(crate) fn active_tombstones(&self, s: SessionId) -> Vec<SentenceId> {
        let side = self.side();
        side.tombstoned_sids(s).into_iter()
            .filter(|sid| side.tombstone_active(s, *sid))
            .collect()
    }

    /// Demote `sid` for promoting-session `name`: drop it from the axiom's
    /// provenance. Returns `true` if no promoter remains, in which case it
    /// ceases to be a Base axiom.
    pub(crate) fn demote(&self, name: &str, sid: SentenceId) -> bool {
        let now_empty = match self.side().promoted_by.get_mut(&sid) {
            Some(mut p) => { p.remove(name); p.is_empty() }
            None => true,
        };
        if now_empty {
            self.side().promoted_by.remove(&sid);
            self.side().promoted.remove(&sid);
        }
        now_empty
    }

    /// The session(s) an axiom was promoted from (lineage). Empty for a non-axiom
    /// or unknown sid.
    pub(crate) fn provenance_of(&self, sid: SentenceId) -> Vec<String> {
        self.side().promoted_by.get(&sid).map(|s| s.iter().cloned().collect()).unwrap_or_default()
    }
}

impl SyntacticLayer {
    /// `true` if `sid` is an axiom (see [`EagerMap::<SessionCache>::is_axiom`]).
    #[allow(dead_code)]
    pub(crate) fn is_axiom(&self, sid: SentenceId) -> bool {
        self.sessions.is_axiom(sid)
    }

    /// The sessions (by [`SessionId`]) that reference root `sid`.
    #[allow(dead_code)]
    pub(crate) fn sessions_of(&self, sid: SentenceId) -> Vec<SessionId> {
        self.sessions.sessions_of(sid)
    }

    /// The session(s) an axiom was promoted from (lineage); see
    /// [`EagerMap::<SessionCache>::provenance_of`].
    #[allow(dead_code)]
    pub(crate) fn axiom_provenance(&self, sid: SentenceId) -> Vec<String> {
        self.sessions.provenance_of(sid)
    }
}
