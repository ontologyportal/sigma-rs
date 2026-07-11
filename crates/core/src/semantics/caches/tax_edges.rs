//! `semantic::tax_edges` — the taxonomy as a bidirectional adjacency index.
//!
//! Keyed by `TaxDirection` (`To(sym)` / `From(sym)`) so both neighbour lookups
//! are O(1):
//!   * `To(sym)`   → `{(parent, rel)}`  — `sym`'s incoming edges  (its parents)
//!   * `From(sym)` → `{(child,  rel)}`  — `sym`'s outgoing edges  (its children)
//!
//! Event-driven: consumes `RelationAdded` / `RelationRemoved` and filters them
//! in O(1) on `head_id`, so only `subclass` / `instance` / `subrelation` /
//! `subAttribute` relations become edges.  Each change emits `TaxonomyChanged`
//! so the derived `is_*` / `has_ancestor` caches invalidate.

use std::collections::HashSet;
use std::sync::Arc;

use dashmap::DashMap;

use crate::{Element, SemanticError, Sentence, SentenceId, SymbolId, TaxRelation, ToDiagnostic};
use crate::cache::EagerMapBehavior;
use crate::cache::events::{Event, EventKind};
use crate::semantics::SemanticLayer;
use crate::semantics::taxonomy::TaxDirection;
use crate::semantics::types::{Scope, Scoped};
use crate::syntactic::caches::session::session_id;

/// The keyed store: `(scope, direction) → {(neighbour, rel)}`.  `Scope::Base`
/// holds promoted-axiom edges; `Scope::Session(X)` holds X's un-promoted
/// transient edges (the delta only — never base ∪ overlay).
type EdgeStore = crate::cache::EntryCache<Scoped<TaxDirection>, Arc<HashSet<(SymbolId, TaxRelation)>>>;

/// Where one tax-edge root's edge currently lives, recorded so removal and
/// promotion-graduation touch exactly the right scope entries without a full-map
/// scan.
#[derive(Debug, Clone)]
struct EdgeRecord {
    from:   SymbolId,
    to:     SymbolId,
    rel:    TaxRelation,
    scopes: HashSet<Scope>,
}

/// Companion side state: `sid → EdgeRecord` for every tax-edge root — the
/// authoritative "where does this edge live" index.
#[derive(Debug, Default)]
pub(crate) struct TaxEdgesSide {
    edges: DashMap<SentenceId, EdgeRecord>,
    /// Refcount of overlay edges per session scope: `session_overlay[S] > 0` ⇔
    /// session `S` carries at least one transient taxonomy edge.  Lets the
    /// transitive `has_ancestor` / `is_relation` / `is_predicate` / `is_function`
    /// caches fall through to `Base` for sessions that declare no taxonomy at all.
    /// Derived state, rebuilt from `edges` in `restore_side`.
    session_overlay: DashMap<Scope, usize>,
}

impl TaxEdgesSide {
    /// The taxonomy edge `(from, to, rel)` a sentence id produced, if it is a
    /// tracked tax edge.
    pub(crate) fn edge_of(&self, sid: SentenceId) -> Option<(SymbolId, SymbolId, TaxRelation)> {
        self.edges.get(&sid).map(|r| (r.from, r.to, r.rel.clone()))
    }

    /// `true` iff `scope` is a session with ≥1 transient taxonomy edge.
    pub(crate) fn session_active(&self, scope: Scope) -> bool {
        matches!(scope, Scope::Session(_))
            && self.session_overlay.get(&scope).is_some_and(|c| *c > 0)
    }

    /// Drop all edge records and overlay refcounts, resetting the side to empty.
    #[cfg(feature = "native-prover")]
    pub(crate) fn clear(&self) {
        self.edges.clear();
        self.session_overlay.clear();
    }
}

impl SemanticLayer {
    /// `true` iff `scope` is a session carrying its own transient taxonomy edges.
    pub(crate) fn tax_session_active(&self, scope: Scope) -> bool {
        self.tax_edges.side().session_active(scope)
    }

    /// The taxonomy edge `(from, to, rel)` a sentence id produced, if it is a
    /// tracked tax edge.
    pub(crate) fn tax_edge_of(&self, sid: SentenceId) -> Option<(SymbolId, SymbolId, TaxRelation)> {
        self.tax_edges.side().edge_of(sid)
    }
}

/// Bump the per-session overlay refcount (no-op for `Base`).
fn bump_overlay(side: &TaxEdgesSide, scope: Scope) {
    if matches!(scope, Scope::Session(_)) {
        *side.session_overlay.entry(scope).or_insert(0) += 1;
    }
}

/// Drop one from the per-session overlay refcount, evicting the key at zero.
fn unbump_overlay(side: &TaxEdgesSide, scope: Scope) {
    if !matches!(scope, Scope::Session(_)) { return; }
    let now_zero = match side.session_overlay.get_mut(&scope) {
        Some(mut c) => { *c = c.saturating_sub(1); *c == 0 }
        None        => false,
    };
    if now_zero { side.session_overlay.remove(&scope); }
}

/// Flat, serializable form of one `EdgeRecord`.
type EdgeSnap = (SentenceId, SymbolId, SymbolId, TaxRelation, Vec<Scope>);

/// Insert edge `(from, to, rel)` into `scope`'s `To`/`From` adjacency entries.
fn add_edge(store: &EdgeStore, scope: Scope, from: SymbolId, to: SymbolId, rel: TaxRelation) {
    store.modify_entry(Scoped { scope, key: TaxDirection::To(to) },    |s| { Arc::make_mut(s).insert((from, rel.clone())); });
    store.modify_entry(Scoped { scope, key: TaxDirection::From(from) }, |s| { Arc::make_mut(s).insert((to,   rel.clone())); });
}

/// Remove edge `(from, to, rel)` from `scope`, evicting now-empty adjacency keys.
fn remove_edge(store: &EdgeStore, scope: Scope, from: SymbolId, to: SymbolId, rel: TaxRelation) {
    let to_key = Scoped { scope, key: TaxDirection::To(to) };
    store.modify_entry(to_key.clone(), |s| { Arc::make_mut(s).remove(&(from, rel.clone())); });
    if store.get(&to_key).is_some_and(|s| s.is_empty()) { store.evict_keys(&[to_key]); }
    let from_key = Scoped { scope, key: TaxDirection::From(from) };
    store.modify_entry(from_key.clone(), |s| { Arc::make_mut(s).remove(&(to, rel.clone())); });
    if store.get(&from_key).is_some_and(|s| s.is_empty()) { store.evict_keys(&[from_key]); }
}

/// The scope(s) a root `sid`'s edge belongs to: `Base` once promoted, else every
/// session that references it (transient overlay).
fn edge_scopes(parent: &SemanticLayer, sid: SentenceId) -> Vec<Scope> {
    if parent.syntactic.is_axiom(sid) {
        vec![Scope::Base]
    } else {
        parent.syntactic.sessions_of(sid).into_iter().map(Scope::Session).collect()
    }
}

/// Behavior for the `semantic::tax_edges` adjacency index.
#[derive(Debug, Default)]
pub(crate) struct TaxEdges;

impl EagerMapBehavior for TaxEdges {
    type Parent = SemanticLayer;
    type Key    = Scoped<TaxDirection>;
    type Value  = Arc<HashSet<(SymbolId, TaxRelation)>>;
    type Side   = TaxEdgesSide;
    type SideSnapshot = Vec<EdgeSnap>;

    const NAME: &'static str = "semantic::tax_edges";

    fn consumes(&self) -> &'static [EventKind] {
        &[EventKind::RelationAdded, EventKind::RelationRemoved,
          EventKind::AxiomsPromoted, EventKind::SessionRetracted,
          EventKind::SessionReferenced]
    }

    fn produces(&self) -> &'static [EventKind] {
        &[EventKind::TaxonomyChanged]
    }

    fn reads(&self) -> &'static [&'static str] {
        &["syntactic::sentences", "syntactic::sessions"]
    }

    fn snapshot_side(&self, side: &TaxEdgesSide) -> Vec<EdgeSnap> {
        side.edges.iter().map(|e| {
            let r = e.value();
            (*e.key(), r.from, r.to, r.rel.clone(), r.scopes.iter().copied().collect())
        }).collect()
    }

    fn restore_side(&self, side: &TaxEdgesSide, snap: Vec<EdgeSnap>) {
        for (sid, from, to, rel, scopes) in snap {
            for scope in &scopes { bump_overlay(side, *scope); }
            side.edges.insert(sid, EdgeRecord { from, to, rel, scopes: scopes.into_iter().collect() });
        }
    }

    fn react(
        &self,
        parent: &SemanticLayer,
        events: &[&Event],
        store:  &EdgeStore,
        side:   &TaxEdgesSide,
    ) -> Vec<Event> {
        let mut out = Vec::new();
        for event in events {
            match event {
                Event::RelationAdded { sid, head_id } => {
                    if parent.tax_role_of(*head_id).is_none() { continue; }
                    match parent.try_extract_edge(*sid) {
                        Some(Ok((from, to, rel))) => {
                            let scopes = edge_scopes(parent, *sid);
                            for scope in &scopes {
                                add_edge(store, *scope, from, to, rel.clone());
                                bump_overlay(side, *scope);
                            }
                            side.edges.insert(*sid, EdgeRecord {
                                from, to, rel, scopes: scopes.into_iter().collect(),
                            });
                            out.push(Event::TaxonomyChanged { syms: vec![from, to] });
                        }
                        Some(Err(err)) => out.push(Event::Diagnostic(err.to_diagnostic())),
                        None => {}
                    }
                }
                // Move the promoted edge from its session overlay(s) into `Base`.
                Event::AxiomsPromoted { sids } => {
                    for sid in sids {
                        let Some((from, to, rel, session_scopes)) =
                            side.edges.get(sid).map(|rec| {
                                let ss: Vec<Scope> = rec.scopes.iter().copied()
                                    .filter(|s| matches!(s, Scope::Session(_))).collect();
                                (rec.from, rec.to, rec.rel.clone(), ss)
                            })
                        else { continue };
                        add_edge(store, Scope::Base, from, to, rel.clone());
                        for scope in &session_scopes {
                            remove_edge(store, *scope, from, to, rel.clone());
                            unbump_overlay(side, *scope);
                        }
                        if let Some(mut rec) = side.edges.get_mut(sid) {
                            rec.scopes = std::iter::once(Scope::Base).collect();
                        }
                        out.push(Event::TaxonomyChanged { syms: vec![from, to] });
                    }
                }
                // A dedup re-assert associates existing roots with a NEW session.
                // No `RelationAdded` fired (the root already existed), so extend
                // the edge's scope set here.  Only edges already tracked in
                // `side.edges` (taxonomy) match; everything else is an O(1) miss.
                Event::SessionReferenced { session, sids } => {
                    let scope = Scope::Session(session_id(session));
                    let mut syms = Vec::new();
                    for sid in sids {
                        let Some(mut rec) = side.edges.get_mut(sid) else { continue };
                        // Already in Base: the session sees it there.  A redundant
                        // overlay would wrongly mark the session active and defeat
                        // the Base fall-through.
                        if rec.scopes.contains(&Scope::Base) { continue; }
                        if rec.scopes.insert(scope) {
                            add_edge(store, scope, rec.from, rec.to, rec.rel.clone());
                            bump_overlay(side, scope);
                            syms.push(rec.from);
                            syms.push(rec.to);
                        }
                    }
                    if !syms.is_empty() {
                        out.push(Event::TaxonomyChanged { syms });
                    }
                }
                // A session is dropped wholesale.  Edges it held *alone* already
                // left via `RelationRemoved`, so the only survivors are edges still
                // shared with another session; strip just this session's scope from
                // them, keeping the edge for its other owners.
                Event::SessionRetracted { session } => {
                    let scope = Scope::Session(session_id(session));
                    if !side.session_overlay.contains_key(&scope) { continue; }
                    let mut syms = Vec::new();
                    side.edges.retain(|_, rec| {
                        if rec.scopes.remove(&scope) {
                            remove_edge(store, scope, rec.from, rec.to, rec.rel.clone());
                            unbump_overlay(side, scope);
                            syms.push(rec.from);
                            syms.push(rec.to);
                        }
                        !rec.scopes.is_empty()
                    });
                    if !syms.is_empty() {
                        out.push(Event::TaxonomyChanged { syms });
                    }
                }
                Event::RelationRemoved { sid, .. } => {
                    if let Some((_, rec)) = side.edges.remove(sid) {
                        for scope in &rec.scopes {
                            remove_edge(store, *scope, rec.from, rec.to, rec.rel.clone());
                            unbump_overlay(side, *scope);
                        }
                        out.push(Event::TaxonomyChanged { syms: vec![rec.from, rec.to] });
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// Build the adjacency from scratch by scanning the sentence store — the
    /// one-time prime for a `SemanticLayer` wrapping an already-populated
    /// `SyntacticLayer`.  Self-guards on `is_empty`, so a restored adjacency is
    /// left untouched.  Each edge is scoped from the session cache so the
    /// base/overlay split is rebuilt faithfully.
    fn initialize(&self, parent: &SemanticLayer, store: &EdgeStore, side: &TaxEdgesSide) {
        if !store.is_empty() { return; }
        // ROOTS ONLY — must match the reactive path, which fires `RelationAdded`
        // only for root symbol-headed sentences.  Sub-sentences would pull edges
        // out of rule hypotheses.
        let sids: Vec<SentenceId> = parent.syntactic.root_sids();
        for sid in sids {
            if let Some(Ok((from, to, rel))) = parent.try_extract_edge(sid) {
                let scopes = edge_scopes(parent, sid);
                for scope in &scopes {
                    add_edge(store, *scope, from, to, rel.clone());
                    bump_overlay(side, *scope);
                }
                side.edges.insert(sid, EdgeRecord {
                    from, to, rel, scopes: scopes.into_iter().collect(),
                });
            }
        }
    }
}

impl SemanticLayer {
    // -- Taxonomy management ---------------------------------------------------

    /// Try to extract a taxonomy edge from a single sentence `sid`.
    ///
    /// * `None`        — not headed by a taxonomy predicate (not an edge; skip).
    /// * `Some(Ok(_))` — a well-formed `(from, to, rel)` edge.
    /// * `Some(Err(_))`— taxonomy-headed but malformed (bad arity / a literal in
    ///                   a class position); the caller surfaces it as a diagnostic.
    fn try_extract_edge(
        &self,
        sid: SentenceId,
    ) -> Option<Result<(SymbolId, SymbolId, TaxRelation), SemanticError>> {
        let sentence = self.syntactic.sentence(sid)?;
        try_extract_edge_from(self, sid, &sentence)
    }
}

/// One taxonomy argument, classified for edge extraction.
enum EdgeArg {
    /// A symbol or (non-row) variable: a usable endpoint id.
    Id(SymbolId),
    /// A complex term (sub-sentence, e.g. `(UnionFn A B)`) or anything else not
    /// representable as a flat endpoint — legitimate, but not a simple edge.  Skip
    /// silently rather than flag it.
    Skip,
    /// A literal where a class/term is expected — a genuine malformation.
    Bad,
}

fn classify_arg(el: Option<&Element>) -> EdgeArg {
    match el {
        Some(Element::Symbol(sym)) => EdgeArg::Id(sym.id()),
        Some(Element::Variable { id, is_row: false, .. }) => EdgeArg::Id(*id),
        Some(Element::Literal(_)) => EdgeArg::Bad,
        _ => EdgeArg::Skip, // sub-term, row-variable, op, or missing
    }
}

/// Extract a taxonomy edge directly from a sentence body.
///
/// Once the head filter confirms a taxonomy predicate, the two arguments are
/// validated: a well-formed binary `(rel child parent)` yields the edge
/// `(from = parent, to = child, rel)`; bad arity or a literal argument yields a
/// `SemanticError`.  Returns `None` when the sentence is not a taxonomy edge at
/// all (or carries a complex-term argument, which is skipped silently).
fn try_extract_edge_from(
    layer: &SemanticLayer,
    sid: SentenceId,
    sentence: &Sentence,
) -> Option<Result<(SymbolId, SymbolId, TaxRelation), SemanticError>> {
    let head_sym = sentence.head_symbol()?;
    let rel      = layer.tax_role_of(head_sym)?; // not a taxonomy edge → skip
    let rel_name = || rel.as_sym().name().to_string();

    // Taxonomy predicates are binary: exactly two arguments after the head.
    if sentence.elements.len() != 3 {
        return Some(Err(SemanticError::ArityMismatch {
            sid,
            rel:      rel_name(),
            expected: 2,
            got:      sentence.elements.len().saturating_sub(1),
        }));
    }

    // arg 1 (child / specific) and arg 2 (parent / general).
    let to = match classify_arg(sentence.elements.get(1)) {
        EdgeArg::Id(id) => id,
        EdgeArg::Skip   => return None,
        EdgeArg::Bad    => return Some(Err(SemanticError::DomainMismatch {
            sid, rel: rel_name(), arg: 0, domain: "Entity".to_string(),
        })),
    };
    let from = match classify_arg(sentence.elements.get(2)) {
        EdgeArg::Id(id) => id,
        EdgeArg::Skip   => return None,
        EdgeArg::Bad    => return Some(Err(SemanticError::DomainMismatch {
            sid, rel: rel_name(), arg: 1, domain: "Entity".to_string(),
        })),
    };
    Some(Ok((from, to, rel)))
}
