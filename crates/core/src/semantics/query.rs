//! Semantic-layer query helpers: taxonomy walks, scope resolution, and scoped
//! sentence lookups.

use std::collections::HashSet;

use crate::Element;
use crate::{SymbolId, types::TaxRelation};
use crate::types::SentenceId;
use super::taxonomy::TaxDirection;
use super::types::{Scope, Scoped};

use super::SemanticLayer;

impl SemanticLayer {
    /// Check if a given symbol has an ancestor with a given name (`Base` scope).
    pub(crate) fn has_ancestor_by_name(&self, sym: SymbolId, ancestor: &str) -> bool {
        self.has_ancestor_by_name_scoped(sym, ancestor, Scope::Base)
    }

    /// [`Self::has_ancestor_by_name`] in an explicit [`Scope`].
    pub(crate) fn has_ancestor_by_name_scoped(&self, sym: SymbolId, ancestor: &str, scope: Scope) -> bool {
        let anc_id = match self.syntactic.sym_id(ancestor) {
            Some(id) => id,
            None     => return false,
        };
        self.has_ancestor_scoped(sym, anc_id, scope)
    }

    /// Collect the `subclass`/`instance` fact sentences along the taxonomy
    /// ancestor closure of `seed_syms`, walking upward from every seed symbol
    /// and returning the fact sentence for each edge.  Bounded by `cap` total
    /// facts.
    pub(crate) fn taxonomy_closure_facts(
        &self,
        seed_syms: &HashSet<SymbolId>,
        cap:       usize,
    ) -> HashSet<SentenceId> {
        self.taxonomy_closure_facts_scoped(seed_syms, cap, Scope::Base)
    }

    /// Depth-first walk over the strict subclass-closure of `start` — upward
    /// through `parents_of` when `up`, else downward through `children_of` —
    /// calling `visit` on each newly reached symbol (`start` itself is NOT
    /// visited).  `visit` returns `false` to stop the walk early.
    pub(crate) fn walk_subclass_closure(
        &self,
        start: SymbolId,
        up:    bool,
        mut visit: impl FnMut(SymbolId) -> bool,
    ) {
        let mut stack = vec![start];
        let mut seen: HashSet<SymbolId> = HashSet::new();
        seen.insert(start);
        while let Some(n) = stack.pop() {
            let next = if up { self.parents_of(n) } else { self.children_of(n) };
            for (m, rel) in next {
                if !matches!(rel, crate::TaxRelation::Subclass) || !seen.insert(m) {
                    continue;
                }
                if !visit(m) {
                    return;
                }
                stack.push(m);
            }
        }
    }

    /// [`Self::taxonomy_closure_facts`] in an explicit [`Scope`]: the upward
    /// walk follows `Base` ∪ the session overlay (via `parents_of_scoped`) so a
    /// session-local class chains up to its base ancestors.  Only `Base` axiom
    /// fact sentences are returned.
    pub(crate) fn taxonomy_closure_facts_scoped(
        &self,
        seed_syms: &HashSet<SymbolId>,
        cap:       usize,
        scope:     Scope,
    ) -> HashSet<SentenceId> {
        let mut out: HashSet<SentenceId> = HashSet::new();
        let roles = self.recognized_roles();
        let subclass_id = roles.map(|r| r.subclass).or_else(|| self.syntactic.sym_id("subclass"));
        let instance_id = roles.map(|r| r.instance).or_else(|| self.syntactic.sym_id("instance"));
        if subclass_id.is_none() && instance_id.is_none() {
            return out;
        }

        let mut seen: HashSet<SymbolId> = HashSet::new();
        let mut frontier: Vec<SymbolId> = seed_syms.iter().copied().collect();
        while let Some(c) = frontier.pop() {
            if !seen.insert(c) { continue; }
            if out.len() >= cap { break; }
            for (parent, rel) in self.parents_of_scoped(c, scope) {
                if !matches!(rel, TaxRelation::Subclass | TaxRelation::Instance) { continue; }
                let head_id = match rel {
                    TaxRelation::Subclass => subclass_id,
                    TaxRelation::Instance => instance_id,
                    _ => None,
                };
                let Some(head_id) = head_id else { continue };
                // Find the fact sentence `(head c parent)` among `c`'s sentences.
                for sid in self.syntactic.axiom_sentences_of(c).iter().copied() {
                    let Some(s) = self.syntactic.sentence(sid) else { continue };
                    if s.elements.len() == 3
                        && matches!(s.elements.first(), Some(Element::Symbol(sym)) if sym.id() == head_id)
                        && matches!(s.elements.get(1), Some(Element::Symbol(sym)) if sym.id() == c)
                        && matches!(s.elements.get(2), Some(Element::Symbol(sym)) if sym.id() == parent)
                    {
                        out.insert(sid);
                    }
                }
                frontier.push(parent);
            }
        }
        out
    }

    /// `true` iff `sym` is an **instance** of `class` (directly, or of a
    /// subclass of `class`) — i.e. there is an `instance` edge `sym → K` with
    /// `K == class` or `K` a subclass-descendant of `class`.
    ///
    /// Distinct from [`Self::has_ancestor`], which also returns `true` when
    /// `sym` is a *subclass* of `class`.
    pub(crate) fn reaches_via_instance(&self, sym: SymbolId, class: SymbolId) -> bool {
        self.parents_of(sym).into_iter().any(|(k, rel)| {
            matches!(rel, TaxRelation::Instance) && (k == class || self.has_ancestor(k, class))
        })
    }

    /// Return the name of the nearest ancestor of `sym` that appears in
    /// `candidates` (a slice of symbol names), or `None` if no candidate is
    /// reachable.
    ///
    /// The symbol itself is considered its own ancestor: if `sym`'s name is in
    /// `candidates` it is returned immediately.  Among reachable candidates the
    /// one reached soonest in a BFS over the `subclass` / `subrelation` /
    /// `subAttribute` edges is returned.
    #[allow(dead_code)]
    pub(crate) fn nearest_ancestor_among(
        &self,
        sym:        SymbolId,
        candidates: &[&str],
    ) -> Option<String> {
        if candidates.is_empty() { return None; }

        let cand_ids: Vec<(SymbolId, &str)> = candidates
            .iter()
            .filter_map(|n| self.syntactic.sym_id(n).map(|id| (id, *n)))
            .collect();
        if cand_ids.is_empty() { return None; }

        // BFS via `parents_of`, which manages the `tax_edges` lock internally —
        // do not nest a second `tax_edges` read-lock here.
        let mut visited: HashSet<SymbolId> = HashSet::new();
        let mut queue: std::collections::VecDeque<SymbolId> =
            std::collections::VecDeque::new();
        queue.push_back(sym);
        while let Some(cur) = queue.pop_front() {
            if !visited.insert(cur) { continue; }
            if let Some((_, name)) = cand_ids.iter().find(|(id, _)| *id == cur) {
                return Some((*name).to_string());
            }
            for (from, _rel) in self.parents_of(cur) {
                queue.push_back(from);
            }
        }
        None
    }

    /// Immediate taxonomy parents of `sym` as `(parent_id, relation)` pairs —
    /// `sym`'s incoming edges.  `Base` (axiom) only; the `_scoped` form adds a
    /// session overlay.
    pub(crate) fn parents_of(&self, sym: SymbolId) -> Vec<(SymbolId, TaxRelation)> {
        self.parents_of_scoped(sym, Scope::Base)
    }

    /// `parents_of` in an explicit [`Scope`]: `Base` axioms unioned with the
    /// session's transient overlay when `scope` is a session.
    pub(crate) fn parents_of_scoped(&self, sym: SymbolId, scope: Scope) -> Vec<(SymbolId, TaxRelation)> {
        self.tax_neighbours(TaxDirection::To(sym), scope)
    }

    /// Immediate taxonomy children of `sym` as `(child_id, relation)` pairs —
    /// `sym`'s outgoing edges.  `Base` only; the `_scoped` form adds an overlay.
    pub(crate) fn children_of(&self, sym: SymbolId) -> Vec<(SymbolId, TaxRelation)> {
        self.children_of_scoped(sym, Scope::Base)
    }

    /// `children_of` in an explicit [`Scope`] (see [`Self::parents_of_scoped`]).
    pub(crate) fn children_of_scoped(&self, sym: SymbolId, scope: Scope) -> Vec<(SymbolId, TaxRelation)> {
        self.tax_neighbours(TaxDirection::From(sym), scope)
    }

    /// The cache scope to key a *direct* (parents-of-`sym`-only) query under.
    ///
    /// `is_class` / `is_instance` depend solely on `sym`'s own incoming edges, so
    /// a session overlay can change the answer only if it carries an edge **into**
    /// `sym` (a non-empty `To(sym)` overlay) or hides an edge via a tombstone.
    /// Otherwise the session result equals the `Base` result and is keyed under
    /// `Base`.
    pub(crate) fn direct_scope(&self, sym: SymbolId, scope: Scope) -> Scope {
        match scope {
            Scope::Base => Scope::Base,
            Scope::Session(s) => {
                let has_overlay = self
                    .tax_edges
                    .get(&Scoped { scope, key: TaxDirection::To(sym) })
                    .is_some_and(|s| !s.is_empty());
                // A session that hides an edge (tombstone) must not fold onto Base.
                let hides = !self.syntactic.sessions.active_tombstones(s).is_empty();
                if has_overlay || hides { scope } else { Scope::Base }
            }
        }
    }

    /// The cache scope to key a *transitive* taxonomy query under — `has_ancestor`
    /// and the relation-kind caches (`is_relation` / `is_predicate` / `is_function`).
    ///
    /// Unlike [`Self::direct_scope`], these depend on `sym`'s whole upward
    /// closure, so a per-symbol overlay check is unsound.  The fall-through is
    /// per-session: a session that declares no taxonomy overlay (and hides
    /// nothing) is keyed under `Base`; only sessions that extend or hide part of
    /// the taxonomy get their own entries.
    pub(crate) fn closure_scope(&self, scope: Scope) -> Scope {
        let hides = matches!(scope, Scope::Session(s) if !self.syntactic.sessions.active_tombstones(s).is_empty());
        if self.tax_session_active(scope) || hides { scope } else { Scope::Base }
    }

    /// Given a candidate set of root sids, keep only those visible in `scope`:
    ///   * **Base** — axioms only.
    ///   * **Session(s)** — axioms *plus* the session's own transient roots,
    ///     minus this session's *active* tombstones (staged removals not held
    ///     alive by another promoter).
    pub(crate) fn scope_filter_sids(
        &self,
        candidates: impl IntoIterator<Item = SentenceId>,
        scope:      Scope,
    ) -> Vec<SentenceId> {
        match scope {
            Scope::Base => candidates
                .into_iter()
                .filter(|sid| self.syntactic.is_axiom(*sid))
                .collect(),
            Scope::Session(s) => {
                let tombstoned: HashSet<SentenceId> =
                    self.syntactic.sessions.active_tombstones(s).into_iter().collect();
                candidates
                    .into_iter()
                    .filter(|sid| !tombstoned.contains(sid))
                    .filter(|sid| {
                        self.syntactic.is_axiom(*sid)
                            || self.syntactic.sessions_of(*sid).contains(&s)
                    })
                    .collect()
            }
        }
    }

    /// Scoped subject lookup: root sids headed by `head` with first argument
    /// `subject`, [scope-filtered].
    ///
    /// [scope-filtered]: Self::scope_filter_sids
    pub(crate) fn subject_sids_scoped(
        &self,
        head:    SymbolId,
        subject: SymbolId,
        scope:   Scope,
    ) -> Vec<SentenceId> {
        self.scope_filter_sids(self.syntactic.by_head_arg1(head, subject), scope)
    }

    /// The neighbours of one adjacency `dir` in `scope`: the `Base` (axiom) edges
    /// unioned with the session overlay's edges when `scope` is a session.
    fn tax_neighbours(&self, dir: TaxDirection, scope: Scope) -> Vec<(SymbolId, TaxRelation)> {
        // Own the base edge set so the overlay can be unioned and tombstones
        // subtracted below.
        let mut out: HashSet<(SymbolId, TaxRelation)> = self
            .tax_edges
            .get(&Scoped { scope: Scope::Base, key: dir.clone() })
            .map(|a| (*a).clone())
            .unwrap_or_default();
        if let Scope::Session(s) = scope {
            // Additive overlay.
            if let Some(overlay) = self.tax_edges.get(&Scoped { scope, key: dir.clone() }) {
                out.extend(overlay.iter().cloned());
            }
            // Tombstones: subtract the edges this session's staged removals would
            // drop, but only the *active* ones.
            for tsid in self.syntactic.sessions.active_tombstones(s) {
                if let Some((from, to, rel)) = self.tax_edge_of(tsid) {
                    match &dir {
                        TaxDirection::To(x)   if *x == to   => { out.remove(&(from, rel)); }
                        TaxDirection::From(x) if *x == from => { out.remove(&(to,   rel)); }
                        _ => {}
                    }
                }
            }
        }
        out.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::caches::test_support::{base_layer, kif_layer};

    // -- nearest_ancestor_among -----------------------------------------------

    #[test]
    fn nearest_ancestor_among_finds_closest() {
        let layer = kif_layer("
            (subclass A B)
            (subclass B C)
            (subclass C D)
        ");
        let a = layer.syntactic.sym_id("A").unwrap();
        let result = layer.nearest_ancestor_among(a, &["C", "B", "D"]);
        assert_eq!(result.as_deref(), Some("B"),
            "B is the immediate parent of A, the closest among {{B, C, D}}");
    }

    #[test]
    fn nearest_ancestor_among_returns_self_when_in_list() {
        let layer = kif_layer("(subclass A B)");
        let a = layer.syntactic.sym_id("A").unwrap();
        assert_eq!(
            layer.nearest_ancestor_among(a, &["A", "B"]).as_deref(),
            Some("A"),
        );
    }

    #[test]
    fn nearest_ancestor_among_empty_list_returns_none() {
        let layer = base_layer();
        let human = layer.syntactic.sym_id("Human").unwrap();
        assert_eq!(layer.nearest_ancestor_among(human, &[]), None);
    }

    #[test]
    fn nearest_ancestor_among_unknown_candidate_returns_none() {
        let layer = kif_layer("(subclass Dog Animal)");
        let dog = layer.syntactic.sym_id("Dog").unwrap();
        assert_eq!(layer.nearest_ancestor_among(dog, &["Galaxy"]), None);
    }

    #[test]
    fn nearest_ancestor_among_unreachable_candidate_returns_none() {
        let layer = kif_layer("
            (subclass Dog Animal)
            (subclass Cat Animal)
        ");
        let dog = layer.syntactic.sym_id("Dog").unwrap();
        assert_eq!(layer.nearest_ancestor_among(dog, &["Cat"]), None);
    }

    // -- has_ancestor ---------------------------------------------------------

    #[test]
    fn has_ancestor_true_across_multi_hop_chain() {
        let layer = base_layer();
        let human = layer.syntactic.sym_id("Human").unwrap();
        assert!(layer.has_ancestor_by_name(human, "Entity"));
        assert!(layer.has_ancestor_by_name(human, "Animal"));
    }

    #[test]
    fn has_ancestor_false_for_non_existent_name() {
        let layer = base_layer();
        let human = layer.syntactic.sym_id("Human").unwrap();
        assert!(!layer.has_ancestor_by_name(human, "Unicorn"));
    }
}
