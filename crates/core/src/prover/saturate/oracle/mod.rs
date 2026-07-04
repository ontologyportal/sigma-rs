// crates/core/src/saturate/oracle.rs
//
// The theory oracle (prototype KBOracle): entailment of ground binary
// atoms straight from the semantic layer's closures, bypassing
// resolution entirely.  Analogous to SPASS soft sorts / SMT theory
// propagation: the prover's `make` step discharges oracle-entailed
// literals instead of deriving them clause by clause.
//
//   * `(equal x x)`           — reflexivity (content addressing makes
//                               this an id comparison, compounds included);
//   * `(instance x C)`        — direct classes + upward subclass chain;
//   * `(subclass C D)`        — the subclass chain itself;
//   * `(R a b)` for any R     — direct edges of R ∪ below(R) (the
//                               subrelation lattice, mined rule-edges
//                               included), plus graph reachability when
//                               R is a declared TransitiveRelation.
//
// Every positive answer can produce *witnesses* — the unit facts (with
// store sids, for file:line citation) whose closure entails the atom —
// which the prover surfaces as proof-step premises.
//
// Derived ground units feed back through `add_unit` into a per-problem
// *learned overlay*; base closures stay untouched (they are shared,
// memoized semantic caches).

use std::collections::HashMap;
use std::collections::HashSet;
use super::hash64::{Map64, Set64};

use crate::semantics::SemanticLayer;
use crate::semantics::types::{RelationDomain, Scope};
use crate::types::{Element, Sentence, SentenceId, SymbolId, TaxRelation};

use super::clause::{AtomId, AtomTable};

/// Provable class-disjointness, built once per problem from SUMO's
/// `(disjoint A B)` facts and the pairwise-disjoint members of
/// `(partition C P1 …)` / `(disjointDecomposition C P1 …)`.  Two classes
/// are disjoint iff some ancestor of one is declared disjoint from some
/// ancestor of the other (disjointness inherits downward).
#[derive(Debug, Default, Clone)]
struct DisjointSets {
    /// Normalized `(min_id, max_id)` directly-declared disjoint pairs.
    pairs: Set64<(SymbolId, SymbolId)>,
    /// Provenance: the root sid of the `disjoint`/`partition`/
    /// `disjointDecomposition` declaration that established each pair, so a
    /// refutation can cite it as a proof step.  First declaration wins.
    src: std::collections::HashMap<(SymbolId, SymbolId), SentenceId>,
    /// Root sids of the decomposition/`disjoint` *meaning* axioms whose
    /// semantics this oracle now supplies (the `<=>`/`=>` definitions of
    /// `disjoint`/`partitionN`/`disjointDecompositionN`).  The prover
    /// omits these from resolution so it discharges via the oracle
    /// instead of flooding on the (huge, high-arity) defining clauses.
    /// Only populated under the decomposition opt-in.
    meaning: Vec<SentenceId>,
}

impl DisjointSets {
    /// `disjoint_id` / `partition_id` are the recognized (or default)
    /// heads; `disjointDecomposition` stays on its global name (no
    /// recognizer — row-variable defining axiom).
    fn build(sem: &SemanticLayer, disjoint_id: SymbolId, partition_id: SymbolId) -> Self {
        let mut pairs = Set64::default();
        let mut src: std::collections::HashMap<(SymbolId, SymbolId), SentenceId> =
            std::collections::HashMap::new();
        let mut meaning: Vec<SentenceId> = Vec::new();
        let norm = |a: SymbolId, b: SymbolId| if a <= b { (a, b) } else { (b, a) };
        let sym = |e: Option<&Element>| match e {
            Some(Element::Symbol(s)) => Some(s.id()),
            _ => None,
        };
        // (disjoint A B)
        for sid in sem.syntactic.by_head_id(&disjoint_id).iter() {
            let Some(s) = sem.syntactic.sentence(*sid) else { continue };
            if s.elements.len() == 3 {
                if let (Some(a), Some(b)) = (sym(s.elements.get(1)), sym(s.elements.get(2))) {
                    pairs.insert(norm(a, b));
                    src.entry(norm(a, b)).or_insert(*sid);
                }
            }
        }
        // (partition C P1 P2 …) / (disjointDecomposition C P1 …): the
        // members P_i are pairwise disjoint.  (exhaustiveDecomposition is
        // NOT disjoint — excluded deliberately.)
        let dd = crate::types::Symbol::hash_name("disjointDecomposition");
        for head in [partition_id, dd] {
            for sid in sem.syntactic.by_head_id(&head).iter() {
                let Some(s) = sem.syntactic.sentence(*sid) else { continue };
                let parts: Vec<SymbolId> =
                    s.elements.iter().skip(2).filter_map(|e| sym(Some(e))).collect();
                for i in 0..parts.len() {
                    for j in (i + 1)..parts.len() {
                        pairs.insert(norm(parts[i], parts[j]));
                        src.entry(norm(parts[i], parts[j])).or_insert(*sid);
                    }
                }
            }
        }

        // Shape-recognized decomposition relations (opt-in): some
        // dialects (OpenCyc, and SUMO's row-variable `partition`) express
        // disjointness only through a relation whose defining axiom makes
        // its tail arguments disjoint — and the TPTP translation splits
        // the row variable into `partitionN`/`disjointDecompositionN`, so
        // the single recognized `partition_id` misses them.  Discover such
        // relations by shape, prefix- and arity-agnostically, keyed on the
        // recognized `disjoint`:
        //   (=> (R …xi…xj…) (disjoint xi xj))         — base
        //   (=> (R …) (and … (R' …same vars…) …))     — inherit R' pairs
        // then read each discovered relation's ground facts.
        // Further opt-in (on top of recognition): discovering decomposition
        // relations and recognizing the biconditional `disjoint` form adds
        // disjointness power that cracks the antonym / case-elimination
        // families, but activating the disjointness oracle where it was
        // previously inert reorders the given-clause search and can lose a
        // few proofs it used to find quickly — so it stays behind its own
        // flag, keeping plain `SIGMA_RECOGNIZE_ROLES` regression-free.
        if crate::semantics::roles::disjoint_decomp_active() {
            discover_decomposition_pairs(sem, disjoint_id, &norm, &sym, &mut pairs, &mut meaning);
        }

        Self { pairs, src, meaning }
    }

    fn directly_disjoint(&self, a: SymbolId, b: SymbolId) -> bool {
        self.pairs.contains(&if a <= b { (a, b) } else { (b, a) })
    }

    /// The declaration sid that made `a`/`b` disjoint (for proof citation).
    fn pair_source(&self, a: SymbolId, b: SymbolId) -> Option<SentenceId> {
        self.src.get(&if a <= b { (a, b) } else { (b, a) }).copied()
    }

    fn is_empty(&self) -> bool { self.pairs.is_empty() }
}

/// Discover "disjoint-decomposition" relations by the shape of their
/// defining axioms and harvest the disjoint pairs from their ground
/// facts.  Prefix- and arity-agnostic: keyed on the recognized
/// `disjoint`, it picks up the TPTP-split `partitionN` /
/// `disjointDecompositionN` family (and any renamed analogue) that the
/// single `partition_id` lookup cannot reach.  Mutates `pairs` in place.
fn discover_decomposition_pairs(
    sem:         &SemanticLayer,
    disjoint_id: SymbolId,
    norm:        &dyn Fn(SymbolId, SymbolId) -> (SymbolId, SymbolId),
    sym:         &dyn Fn(Option<&Element>) -> Option<SymbolId>,
    pairs:       &mut Set64<(SymbolId, SymbolId)>,
    meaning:     &mut Vec<SentenceId>,
) {
    use crate::parse::OpKind;
    let syn = &sem.syntactic;

    let sub = |e: &Element| match e {
        Element::Sub(s) => Some(*s),
        _ => None,
    };
    // Raw head id of an atom sentence (no all-variable requirement).
    let head_of = |s: &Sentence| match s.elements.first() {
        Some(Element::Symbol(h)) => Some(h.id()),
        _ => None,
    };
    // (R v1 … vn) with all-variable args → (R, [var ids]); element index
    // of arg k is k+1.
    let atom_vars = |s: &Sentence| -> Option<(SymbolId, Vec<SymbolId>)> {
        let Some(Element::Symbol(h)) = s.elements.first() else { return None };
        let mut vs = Vec::with_capacity(s.elements.len().saturating_sub(1));
        for e in &s.elements[1..] {
            match e {
                Element::Variable { id, .. } => vs.push(*id),
                _ => return None,
            }
        }
        Some((h.id(), vs))
    };
    // element index of var `v` among an atom's args (head is index 0).
    let pos = |args: &[SymbolId], v: SymbolId| args.iter().position(|x| *x == v).map(|k| k + 1);
    let conjuncts = |sid: SentenceId| -> Vec<SentenceId> {
        match syn.sentence(sid) {
            Some(s) if s.op() == Some(&OpKind::And) => {
                s.elements[1..].iter().filter_map(sub).collect()
            }
            _ => vec![sid],
        }
    };

    // Collect every rule edge `(antecedent, consequent)` anywhere in the
    // tree.  `=>` is one edge; `<=>` is two (both directions).  The
    // defining axioms live nested under `forall`/`and`, and `<=>` is kept
    // un-split (stored as `Iff`), so a recursive walk is required.
    // (root, antecedent, consequent) — `root` is the top-level axiom the
    // edge came from, so a flooding meaning axiom can be omitted whole.
    fn collect_edges(
        syn:  &crate::syntactic::SyntacticLayer,
        root: SentenceId,
        sid:  SentenceId,
        out:  &mut Vec<(SentenceId, SentenceId, SentenceId)>,
    ) {
        use crate::parse::OpKind;
        let Some(s) = syn.sentence(sid) else { return };
        if s.elements.len() == 3 {
            let a = match &s.elements[1] { Element::Sub(x) => Some(*x), _ => None };
            let b = match &s.elements[2] { Element::Sub(x) => Some(*x), _ => None };
            if let (Some(a), Some(b)) = (a, b) {
                match s.op() {
                    Some(OpKind::Implies) => out.push((root, a, b)),
                    Some(OpKind::Iff) => { out.push((root, a, b)); out.push((root, b, a)); }
                    _ => {}
                }
            }
        }
        for e in &s.elements {
            if let Element::Sub(c) = e {
                collect_edges(syn, root, *c, out);
            }
        }
    }
    let roots = syn.root_sids();
    let mut edges: Vec<(SentenceId, SentenceId, SentenceId)> = Vec::new();
    for &r in &roots {
        collect_edges(syn, r, r, &mut edges);
    }

    // decomp[R] = element-index pairs (i,j) whose values R makes disjoint.
    let mut decomp: HashMap<SymbolId, HashSet<(usize, usize)>> = HashMap::new();

    // Pass A — base: (R …) ⟹ (disjoint xi xj).
    for &(_root, ant, con) in &edges {
        let (Some(ant_s), Some(con_s)) = (syn.sentence(ant), syn.sentence(con)) else { continue };
        let Some((rh, rargs)) = atom_vars(&ant_s) else { continue };
        if rh == disjoint_id {
            continue;
        }
        if let Some((ch, cargs)) = atom_vars(&con_s) {
            if ch == disjoint_id && cargs.len() == 2 {
                if let (Some(i), Some(j)) = (pos(&rargs, cargs[0]), pos(&rargs, cargs[1])) {
                    decomp.entry(rh).or_default().insert((i.min(j), i.max(j)));
                }
            }
        }
    }

    // Pass B — inherit through (R …) ⟹ (and … (R' …same vars…) …).
    for _ in 0..6 {
        let mut added = false;
        for &(_root, ant, con) in &edges {
            let Some(ant_s) = syn.sentence(ant) else { continue };
            let Some((rh, rargs)) = atom_vars(&ant_s) else { continue };
            if rh == disjoint_id {
                continue;
            }
            for csid in conjuncts(con) {
                let Some(cs) = syn.sentence(csid) else { continue };
                let Some((ch, cargs)) = atom_vars(&cs) else { continue };
                let Some(child) = decomp.get(&ch).cloned() else { continue };
                for (ci, cj) in child {
                    let (Some(&cvi), Some(&cvj)) = (cargs.get(ci - 1), cargs.get(cj - 1)) else {
                        continue;
                    };
                    if let (Some(ri), Some(rj)) = (pos(&rargs, cvi), pos(&rargs, cvj)) {
                        if decomp.entry(rh).or_default().insert((ri.min(rj), ri.max(rj))) {
                            added = true;
                        }
                    }
                }
            }
        }
        if !added {
            break;
        }
    }

    // Harvest disjoint pairs from each discovered relation's ground facts.
    let mut harvested = 0usize;
    for (r, poss) in &decomp {
        for sid in syn.by_head_id(r).iter() {
            let Some(s) = syn.sentence(*sid) else { continue };
            for &(i, j) in poss {
                if let (Some(a), Some(b)) = (sym(s.elements.get(i)), sym(s.elements.get(j))) {
                    pairs.insert(norm(a, b));
                    harvested += 1;
                }
            }
        }
    }
    // Collect the MEANING-axiom roots: any rule whose antecedent or
    // consequent atom is headed by `disjoint` or a discovered
    // decomposition relation.  These are the `<=>`/`=>` definitions the
    // oracle now supplies — the prover omits them from resolution so it
    // discharges through the oracle instead of flooding on the (huge,
    // high-arity) defining clauses.  Their ground FACTS are kept (already
    // harvested above; harmless as passive units).
    let mut theory: HashSet<SymbolId> = decomp.keys().copied().collect();
    theory.insert(disjoint_id);
    let mut meaning_set: HashSet<SentenceId> = HashSet::new();
    for &(root, ant, con) in &edges {
        let hit = [ant, con].iter().any(|&sid| {
            syn.sentence(sid).and_then(|s| head_of(&s)).is_some_and(|h| theory.contains(&h))
        });
        if hit {
            meaning_set.insert(root);
        }
    }
    meaning.extend(meaning_set.iter().copied());
    if std::env::var_os("SIGMA_ORACLE_TRACE").is_some() {
        eprintln!(
            "DECOMP discover: {} relations, {} pairs harvested, {} meaning axioms ({} edges)",
            decomp.len(), harvested, meaning.len(), edges.len(),
        );
    }
}

/// One witnessing fact: the (rel x y) triple as symbol ids, plus the
/// store sid when the fact is a stored sentence (`None` for learned
/// units and virtual facts like `(instance R TransitiveRelation)`
/// reached through the closure rather than a single root).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Witness {
    pub(crate) rel: SymbolId,
    pub(crate) x:   SymbolId,
    pub(crate) y:   SymbolId,
    pub(crate) sid: Option<SentenceId>,
}

/// One functional-dependency declaration: within `rel`, the argument
/// at `key_pos` determines the other — subject to sort guards.  Mined
/// from explicit uniqueness axioms
/// (`¬guards ∨ ¬R(u₁,v₁) ∨ ¬R(u₂,v₂) ∨ v₁=v₂`, key var shared) and
/// from `(instance R SingleValuedRelation)` declarations.
#[derive(Debug, Clone)]
pub(crate) struct FdDecl {
    /// 1 or 2: which argument is the determining key.
    pub(crate) key_pos: u8,
    /// Classes the KEY must provably be an instance of.
    pub(crate) key_guards: Vec<SymbolId>,
    /// Classes the determined VALUE must provably be an instance of.
    pub(crate) val_guards: Vec<SymbolId>,
    /// The uniqueness axiom / declaration root (proof citation).
    pub(crate) axiom: Option<SentenceId>,
}

/// One observed ground fact of an FD relation, kept RAW — the fixpoint
/// groups by CURRENT equality representative each pass, so later
/// merges re-bucket for free.
#[derive(Debug, Clone, Copy)]
struct FdFact {
    x: SymbolId,
    y: SymbolId,
    /// The deriving clause (proof-DAG anchor), when known.
    clause: Option<u32>,
}

/// Why two equality classes merged — the label on a union-find edge,
/// walked by [`SemanticOracle::eq_explain`] for proof transcripts.
#[derive(Debug, Clone, Default)]
pub(crate) struct EqJust {
    pub(crate) fact_sids: Vec<SentenceId>,
    pub(crate) clause_parents: Vec<u32>,
    pub(crate) axiom: Option<SentenceId>,
}

/// The semantic-layer-backed oracle, scoped to one problem.
pub(crate) struct SemanticOracle<'a> {
    sem:   &'a SemanticLayer,
    scope: Scope,
    /// Learned ground edges from derived units: rel → x → y → source
    /// clause id.  `instance` / `subclass` / `subrelation` edges land
    /// here too and are consulted alongside the base closures.  The
    /// source clause id (when known) is the derivation's proof-DAG
    /// anchor: a discharge against a learned edge cites that clause as
    /// a parent so the unit's own derivation surfaces in the transcript.
    learned: Map64<SymbolId, Map64<SymbolId, Map64<SymbolId, Option<u32>>>>,
    /// Ground-constant equality closure: a union-find (child → parent)
    /// over symbols asserted/derived `equal`.  The representative is the
    /// numerically-smallest id in a class (deterministic).  The prover
    /// normalizes ground arguments to representatives before indexing, so
    /// `equal` constants collapse to one symbol and resolution connects
    /// facts stated about either spelling.
    eq: Map64<SymbolId, SymbolId>,
    /// Provable class-disjointness for the sorted-relation filter.
    disjoint: DisjointSets,
    /// Functional-dependency congruence state: declarations per
    /// relation, raw observed facts, and equalities derived but not
    /// yet surfaced as unit clauses (the prover drains these into the
    /// saturation so they resolve/paramodulate like any equality).
    fd_decls: Map64<SymbolId, Vec<FdDecl>>,
    fd_facts: Map64<SymbolId, Vec<FdFact>>,
    pending_eq: Vec<(SymbolId, SymbolId, EqJust)>,
    /// Justification label per union edge (keyed by the child
    /// endpoint) — the equality proof forest.
    eq_just: Map64<SymbolId, EqJust>,
    /// Exhaustive class decompositions: class → (members, declaration
    /// sid).  `partition` and `exhaustiveDecomposition` both assert
    /// exhaustiveness (partition adds disjointness — handled by
    /// `DisjointSets`); SUMO's own exhaustiveness axiom routes through
    /// ListFn existentials that saturation cannot chew, so the oracle
    /// does the case analysis directly.
    exhaustive: Map64<SymbolId, Vec<(Vec<SymbolId>, SentenceId)>>,
    /// NEGATIVE learned ground edges (¬(rel x y) asserted/derived) —
    /// the exclusions exhaustiveness propagation eliminates against.
    neg_learned: Map64<SymbolId, Map64<SymbolId, Map64<SymbolId, Option<u32>>>>,
    /// Derived positive facts awaiting surfacing as unit clauses
    /// (exhaustiveness propagation: all-but-one member excluded).
    pending_facts: Vec<(SymbolId, SymbolId, SymbolId, EqJust)>,
    /// Witness-free `holds` memo: (rel,x,y) → (epoch, result).  TRUE is
    /// monotone within a run (the learned overlay and equality closure
    /// only grow), so true entries never expire; FALSE entries are
    /// valid only at the epoch they were computed — `add_unit` /
    /// `add_equality` bump it.  Kills the repeated sub_chain BFS +
    /// scoped-cache walks that dominate `stale()` re-checks and
    /// theory-propagation misses.
    holds_memo: std::cell::RefCell<Map64<(SymbolId, SymbolId, SymbolId), (u64, bool)>>,
    epoch: std::cell::Cell<u64>,
    /// Rule-mined symmetric relations (schema channel): relations whose
    /// symmetry is stated by an explicit `¬R(x,y) ∨ R(y,x)` axiom
    /// rather than an `(instance R SymmetricRelation)` declaration.
    /// The sid (when known) is the mined axiom — orientation citations.
    sym_mined: Map64<SymbolId, Option<SentenceId>>,
    /// Rule-mined transitive relations, ditto — `is_transitive` treats
    /// them exactly like declared ones (reachability closure), the sid
    /// labels closure witnesses.
    trans_mined: Map64<SymbolId, Option<SentenceId>>,
    // Well-known ids.  `SymbolId` is the name's content hash, so these
    // are correct whether or not the KB ever interned the names.
    instance_id:   SymbolId,
    subclass_id:   SymbolId,
    subrelation_id: SymbolId,
    transitive_id: SymbolId,
    symmetric_id:  SymbolId,
    /// Class-disjointness / exhaustive-decomposition heads (recognized
    /// or default); drive `DisjointSets` and the `exhaustive` map.
    disjoint_id:   SymbolId,
    partition_id:  SymbolId,
    /// Lazily-built temporal point network (best-effort interval/point
    /// reasoning over `before`/`meets`/`during`/…); `None` until the
    /// first temporal query.  `RefCell` because `holds` is `&self`; not
    /// snapshotted — cheap to rebuild from facts.
    temporal: std::cell::RefCell<Option<super::temporal::TemporalNet>>,
}

/// The oracle's OWNED state, detached from its `&SemanticLayer` borrow
/// — what a background snapshot stores (the layer outlives every
/// snapshot; the borrow is re-supplied on rehydration).  Interior
/// mutability is flattened: plain map + counter, no cells.
#[derive(Debug, Clone)]
pub(crate) struct OracleSnapshot {
    learned: Map64<SymbolId, Map64<SymbolId, Map64<SymbolId, Option<u32>>>>,
    eq: Map64<SymbolId, SymbolId>,
    disjoint: DisjointSets,
    fd_decls: Map64<SymbolId, Vec<FdDecl>>,
    fd_facts: Map64<SymbolId, Vec<FdFact>>,
    pending_eq: Vec<(SymbolId, SymbolId, EqJust)>,
    eq_just: Map64<SymbolId, EqJust>,
    exhaustive: Map64<SymbolId, Vec<(Vec<SymbolId>, SentenceId)>>,
    neg_learned: Map64<SymbolId, Map64<SymbolId, Map64<SymbolId, Option<u32>>>>,
    pending_facts: Vec<(SymbolId, SymbolId, SymbolId, EqJust)>,
    holds_memo: Map64<(SymbolId, SymbolId, SymbolId), (u64, bool)>,
    epoch: u64,
    sym_mined: Map64<SymbolId, Option<SentenceId>>,
    trans_mined: Map64<SymbolId, Option<SentenceId>>,
    /// The taxonomy-role ids in force when the snapshot was taken —
    /// recognized roles (when `recognize_roles` ran before freeze) must
    /// survive rehydration, not silently revert to the English-name
    /// defaults.
    roles: crate::semantics::roles::TaxonomyRoles,
}

impl<'a> SemanticOracle<'a> {
    /// Capture the owned state (background-load products: input
    /// equalities, FD/schema registries, exhaustive sets, learned
    /// edges from input units, the holds memo).
    pub(crate) fn snapshot(&self) -> OracleSnapshot {
        OracleSnapshot {
            learned: self.learned.clone(),
            eq: self.eq.clone(),
            disjoint: self.disjoint.clone(),
            fd_decls: self.fd_decls.clone(),
            fd_facts: self.fd_facts.clone(),
            pending_eq: self.pending_eq.clone(),
            eq_just: self.eq_just.clone(),
            exhaustive: self.exhaustive.clone(),
            neg_learned: self.neg_learned.clone(),
            pending_facts: self.pending_facts.clone(),
            holds_memo: self.holds_memo.borrow().clone(),
            epoch: self.epoch.get(),
            sym_mined: self.sym_mined.clone(),
            trans_mined: self.trans_mined.clone(),
            roles: crate::semantics::roles::TaxonomyRoles {
                instance:    self.instance_id,
                subclass:    self.subclass_id,
                subrelation: self.subrelation_id,
                transitive:  self.transitive_id,
                symmetric:   self.symmetric_id,
                disjoint:    self.disjoint_id,
                partition:   self.partition_id,
                // domain/range live on the semantic layer, not the oracle —
                // defaults here (never read through the oracle's snapshot).
                ..Default::default()
            },
        }
    }

    /// Rehydrate an oracle from a snapshot, re-supplying the layer
    /// borrow.  `DisjointSets::build` is skipped — the snapshot's copy
    /// is authoritative for its key (the KB fingerprint is part of the
    /// snapshot cache key).
    pub(crate) fn from_snapshot(
        sem:   &'a SemanticLayer,
        scope: Scope,
        snap:  &OracleSnapshot,
    ) -> Self {
        Self {
            sem,
            scope,
            learned: snap.learned.clone(),
            eq: snap.eq.clone(),
            disjoint: snap.disjoint.clone(),
            fd_decls: snap.fd_decls.clone(),
            fd_facts: snap.fd_facts.clone(),
            pending_eq: snap.pending_eq.clone(),
            eq_just: snap.eq_just.clone(),
            exhaustive: snap.exhaustive.clone(),
            neg_learned: snap.neg_learned.clone(),
            pending_facts: snap.pending_facts.clone(),
            holds_memo: std::cell::RefCell::new(snap.holds_memo.clone()),
            epoch: std::cell::Cell::new(snap.epoch),
            sym_mined: snap.sym_mined.clone(),
            trans_mined: snap.trans_mined.clone(),
            instance_id:    snap.roles.instance,
            subclass_id:    snap.roles.subclass,
            subrelation_id: snap.roles.subrelation,
            transitive_id:  snap.roles.transitive,
            symmetric_id:   snap.roles.symmetric,
            disjoint_id:    snap.roles.disjoint,
            partition_id:   snap.roles.partition,
            temporal:       std::cell::RefCell::new(None),
        }
    }

    pub(crate) fn new(sem: &'a SemanticLayer, scope: Scope) -> Self {
        let roles = crate::semantics::roles::TaxonomyRoles::default();
        let mut oracle = Self {
            sem,
            scope,
            learned: Map64::default(),
            eq: Map64::default(),
            disjoint: DisjointSets::build(sem, roles.disjoint, roles.partition),
            // (exhaustive sets are filled right after construction —
            // see `build_exhaustive`; they need `self` for sid lookups.)
            holds_memo: std::cell::RefCell::new(Map64::default()),
            epoch: std::cell::Cell::new(0),
            exhaustive: Map64::default(),
            neg_learned: Map64::default(),
            pending_facts: Vec::new(),
            fd_decls: Map64::default(),
            fd_facts: Map64::default(),
            pending_eq: Vec::new(),
            eq_just: Map64::default(),
            sym_mined:   Map64::default(),
            trans_mined: Map64::default(),
            instance_id:    roles.instance,
            subclass_id:    roles.subclass,
            subrelation_id: roles.subrelation,
            transitive_id:  roles.transitive,
            symmetric_id:   roles.symmetric,
            disjoint_id:    roles.disjoint,
            partition_id:   roles.partition,
            temporal:       std::cell::RefCell::new(None),
        };
        oracle.build_exhaustive(sem);
        oracle
    }

    /// The taxonomy-role ids currently in force (recognized or the
    /// English-name defaults).  Pre-pass helpers that read declarations
    /// (`subrelation`, FD `instance`) consult this so they engage on
    /// renamed dialects too.
    pub(crate) fn roles(&self) -> crate::semantics::roles::TaxonomyRoles {
        crate::semantics::roles::TaxonomyRoles {
            instance:    self.instance_id,
            subclass:    self.subclass_id,
            subrelation: self.subrelation_id,
            transitive:  self.transitive_id,
            symmetric:   self.symmetric_id,
            disjoint:    self.disjoint_id,
            partition:   self.partition_id,
            // domain/range are the semantic layer's; not used via the oracle.
            ..Default::default()
        }
    }

    /// Override the taxonomy-role ids with shape-recognized values
    /// (`Strategy.recognize_roles`).  No-op-safe: passing `default()`
    /// leaves the historical behavior.  The taxonomy ids are plain swaps;
    /// `disjoint`/`partition` additionally key the `DisjointSets` and
    /// `exhaustive` maps, so those are rebuilt when the heads change.
    pub(crate) fn set_roles(&mut self, roles: crate::semantics::roles::TaxonomyRoles, sem: &'a SemanticLayer) {
        self.instance_id    = roles.instance;
        self.subclass_id    = roles.subclass;
        self.subrelation_id = roles.subrelation;
        self.transitive_id  = roles.transitive;
        self.symmetric_id   = roles.symmetric;
        // Disjointness / exhaustive decomposition are keyed on the
        // `disjoint` / `partition` heads — rebuild against the recognized
        // ones (cheap; only runs under `recognize_roles`).
        if self.disjoint_id != roles.disjoint || self.partition_id != roles.partition {
            self.disjoint_id  = roles.disjoint;
            self.partition_id = roles.partition;
            self.disjoint = DisjointSets::build(sem, self.disjoint_id, self.partition_id);
            self.exhaustive = Map64::default();
            self.build_exhaustive(sem);
        }
    }

    /// Collect `(partition C P1 …)` / `(exhaustiveDecomposition C P1 …)`
    /// declarations — every instance of C is an instance of SOME Pi.
    fn build_exhaustive(&mut self, sem: &SemanticLayer) {
        let sym = |e: Option<&Element>| match e {
            Some(Element::Symbol(s)) => Some(s.id()),
            _ => None,
        };
        let ed = crate::types::Symbol::hash_name("exhaustiveDecomposition");
        for head in [self.partition_id, ed] {
            for sid in sem.syntactic.by_head_id(&head).iter() {
                let Some(s) = sem.syntactic.sentence(*sid) else { continue };
                let Some(class) = sym(s.elements.get(1)) else { continue };
                let members: Vec<SymbolId> =
                    s.elements.iter().skip(2).filter_map(|e| sym(Some(e))).collect();
                if members.len() >= 2 {
                    self.exhaustive.entry(class).or_default().push((members, *sid));
                }
            }
        }
    }

    /// Decode a stored/interned atom into `(rel, x, y)` symbol ids —
    /// the shape the oracle can discharge.  `None` for non-binary,
    /// non-ground, compound-argument, or variable-headed atoms (those
    /// fall back to ordinary resolution).
    pub(crate) fn decode(
        atoms: &AtomTable,
        syn:   &crate::syntactic::SyntacticLayer,
        atom:  AtomId,
    ) -> Option<(SymbolId, SymbolId, SymbolId)> {
        let s = atoms.resolve(atom, syn)?;
        if s.elements.len() != 3 { return None; }
        let Some(Element::Symbol(rel)) = s.elements.first() else { return None };
        let Element::Symbol(x) = &s.elements[1] else { return None };
        let Element::Symbol(y) = &s.elements[2] else { return None };
        Some((rel.id(), x.id(), y.id()))
    }

    /// Equality reflexivity at the atom level: `(equal t t)` for ANY
    /// ground t, compounds included — content addressing turns the
    /// structural comparison into an element/id comparison.
    pub(crate) fn equal_reflexive(
        atoms: &AtomTable,
        syn:   &crate::syntactic::SyntacticLayer,
        atom:  AtomId,
    ) -> Option<bool> {
        let s = atoms.resolve(atom, syn)?;
        if s.elements.len() != 3 { return None; }
        if !matches!(s.elements.first(), Some(Element::Op(crate::parse::OpKind::Equal))) {
            return None;
        }
        Some(element_eq(&s.elements[1], &s.elements[2]))
    }

    /// Extend the learned overlay with a derived ground unit.
    pub(crate) fn add_unit(&mut self, rel: SymbolId, x: SymbolId, y: SymbolId, src: Option<u32>) {
        self.epoch.set(self.epoch.get() + 1);
        let slot = self.learned.entry(rel).or_default().entry(x).or_default().entry(y).or_insert(src);
        // A later registration may carry provenance an earlier one lacked
        // (e.g. assumption pre-pass, then the made clause) — upgrade, never
        // downgrade to None.
        if src.is_some() { *slot = src; }
        // FD congruence: facts of a functional relation feed the
        // fixpoint; new `instance` facts can satisfy an FD guard, so
        // they trigger a recheck too.
        if rel == self.instance_id && !self.exhaustive.is_empty() {
            self.exh_propagate(x);
        }
        if self.fd_decls.contains_key(&rel) {
            if std::env::var_os("SIGMA_ORACLE_TRACE").is_some() {
                eprintln!("FD-OBSERVE rel={rel:?} x={x:?} y={y:?} src={src:?}");
            }
            self.fd_facts.entry(rel).or_default().push(FdFact { x, y, clause: src });
            self.fd_fixpoint();
        } else if rel == self.instance_id && !self.fd_decls.is_empty() {
            self.fd_fixpoint();
        }
    }

    /// The proof-DAG source clause of a learned edge, when recorded.
    pub(crate) fn learned_src(&self, rel: SymbolId, x: SymbolId, y: SymbolId) -> Option<u32> {
        self.learned.get(&rel)?.get(&x)?.get(&y).copied().flatten()
    }

    /// The current knowledge epoch (bumped by every learned unit /
    /// equality / mined registration) — external memos key negative
    /// results on it, mirroring the holds memo's discipline.
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.get()
    }

    /// Register a rule-mined symmetric relation (schema channel).  A
    /// NEW registration bumps the epoch — memoized-FALSE `holds`
    /// entries that the reverse-edge check could now answer TRUE
    /// expire.  Re-sightings (the same derived rule re-made) are no-ops.
    pub(crate) fn register_symmetric(&mut self, rel: SymbolId, sid: Option<SentenceId>) {
        if let Some(slot) = self.sym_mined.get_mut(&rel) {
            if slot.is_none() && sid.is_some() { *slot = sid; }
            return;
        }
        self.sym_mined.insert(rel, sid);
        self.epoch.set(self.epoch.get() + 1);
    }

    /// Register a rule-mined transitive relation (schema channel).
    pub(crate) fn register_transitive(&mut self, rel: SymbolId, sid: Option<SentenceId>) {
        if let Some(slot) = self.trans_mined.get_mut(&rel) {
            if slot.is_none() && sid.is_some() { *slot = sid; }
            return;
        }
        self.trans_mined.insert(rel, sid);
        self.epoch.set(self.epoch.get() + 1);
    }

    /// Is `rel` symmetric — declared `(instance rel SymmetricRelation)`
    /// (directly or via a subclass of SymmetricRelation, in scope,
    /// including learned units) or rule-mined?  Deliberately NOT
    /// inherited through `subrelation`: `brother ⊑ sibling` is the
    /// counterexample (the subrelation's extra constraint breaks the
    /// symmetry argument).  Memoized via the `holds` front door.
    pub(crate) fn is_symmetric(&self, rel: SymbolId) -> bool {
        self.sym_mined.contains_key(&rel)
            || self.holds(self.instance_id, rel, self.symmetric_id, None)
    }

    /// The citable source of `rel`'s symmetry: the mined axiom's sid,
    /// or the declaration fact's sid.
    pub(crate) fn symmetric_source(&self, rel: SymbolId) -> Option<SentenceId> {
        if let Some(sid) = self.sym_mined.get(&rel) {
            return *sid;
        }
        self.edge_fact_sid(self.instance_id, rel, self.symmetric_id)
    }

    /// Union two ground constants into one equality class (smallest id
    /// becomes the representative).
    pub(crate) fn add_equality(&mut self, a: SymbolId, b: SymbolId) {
        self.epoch.set(self.epoch.get() + 1);
        let (ra, rb) = (self.eq_rep(a), self.eq_rep(b));
        if ra != rb {
            let (root, child) = if ra <= rb { (ra, rb) } else { (rb, ra) };
            self.eq.insert(child, root);
            // An external merge can collapse two FD keys.
            self.fd_fixpoint();
        }
    }

    /// Record a NEGATIVE ground unit (¬(rel x y)) and run
    /// exhaustiveness propagation: excluding a decomposition member
    /// may leave exactly one candidate.
    pub(crate) fn add_neg_unit(&mut self, rel: SymbolId, x: SymbolId, y: SymbolId, src: Option<u32>) {
        self.epoch.set(self.epoch.get() + 1);
        let slot = self.neg_learned.entry(rel).or_default()
            .entry(x).or_default().entry(y).or_insert(src);
        if src.is_some() { *slot = src; }
        if rel == self.instance_id && !self.exhaustive.is_empty() {
            self.exh_propagate(x);
        }
    }

    /// Derived positive facts from exhaustiveness case-elimination,
    /// drained by the prover into activated unit clauses.
    pub(crate) fn take_pending_facts(&mut self) -> Vec<(SymbolId, SymbolId, SymbolId, EqJust)> {
        std::mem::take(&mut self.pending_facts)
    }

    /// Exhaustiveness case analysis for `x`: for each decomposition of
    /// a class `x` inhabits, if every member but one is excluded —
    /// negatively asserted/derived, or refuted by disjointness — the
    /// survivor is entailed.
    fn exh_propagate(&mut self, x: SymbolId) {
        if self.exhaustive.is_empty() {
            return;
        }
        let classes: Vec<(SymbolId, Vec<(Vec<SymbolId>, SentenceId)>)> = self
            .exhaustive
            .iter()
            .map(|(c, v)| (*c, v.clone()))
            .collect();
        let mut derived: Vec<(SymbolId, SymbolId, SymbolId, EqJust)> = Vec::new();
        for (class, decomps) in classes {
            if !self.holds_instance(x, class, None) {
                continue;
            }
            'decomp: for (members, decl_sid) in decomps {
                let mut survivor: Option<SymbolId> = None;
                let mut just = EqJust { axiom: Some(decl_sid), ..Default::default() };
                for &m in &members {
                    if self.holds_instance(x, m, None) {
                        continue 'decomp; // already settled
                    }
                    let excluded_neg = self
                        .neg_learned
                        .get(&self.instance_id)
                        .and_then(|by_x| by_x.get(&x))
                        .and_then(|by_y| by_y.get(&m));
                    let excluded = match excluded_neg {
                        Some(src) => {
                            if let Some(c) = src { just.clause_parents.push(*c); }
                            true
                        }
                        None => {
                            let mut why: Vec<Witness> = Vec::new();
                            if self.refutes_instance(self.instance_id, x, m, Some(&mut why)) {
                                just.fact_sids.extend(why.iter().filter_map(|w| w.sid));
                                true
                            } else {
                                false
                            }
                        }
                    };
                    if !excluded {
                        if survivor.is_some() {
                            continue 'decomp; // two candidates open
                        }
                        survivor = Some(m);
                    }
                }
                if let Some(k) = survivor {
                    if let Some(sid) = self.edge_fact_sid(self.instance_id, x, class) {
                        just.fact_sids.push(sid);
                    }
                    derived.push((self.instance_id, x, k, just));
                }
            }
        }
        for (rel, x, y, just) in derived {
            // Feed the closure too, so chained eliminations fire.
            self.add_unit(rel, x, y, None);
            self.pending_facts.push((rel, x, y, just));
        }
    }

    /// Declare a functional dependency on `rel`.
    pub(crate) fn register_fd(&mut self, rel: SymbolId, decl: FdDecl) {
        if std::env::var_os("SIGMA_ORACLE_TRACE").is_some() {
            eprintln!("FD-REGISTER rel={rel:?} decl={decl:?}");
        }
        self.fd_decls.entry(rel).or_default().push(decl);
        // Late registration: facts observed before the declaration
        // (none today — mining precedes loading — but cheap to honor).
        self.fd_fixpoint();
    }

    pub(crate) fn has_fd(&self) -> bool { !self.fd_decls.is_empty() }

    /// Equalities derived by FD congruence since the last drain — the
    /// prover turns each into an activated `(equal a b)` unit clause
    /// with the justification as proof parents.
    pub(crate) fn take_pending_eq(&mut self) -> Vec<(SymbolId, SymbolId, EqJust)> {
        std::mem::take(&mut self.pending_eq)
    }

    /// The proof-forest labels along both arguments' paths to their
    /// common representative: every fact / deriving clause / axiom
    /// that contributed a merge between the two.  (Slight
    /// over-approximation — the labels on the full root paths — but
    /// every cited premise is a real input to the closure.)
    pub(crate) fn eq_explain(&self, a: SymbolId, b: SymbolId) -> (Vec<SentenceId>, Vec<u32>) {
        let mut sids: Vec<SentenceId> = Vec::new();
        let mut clauses: Vec<u32> = Vec::new();
        for start in [a, b] {
            let mut s = start;
            let mut guard = 0u32;
            while let Some(&p) = self.eq.get(&s) {
                if p == s { break; }
                if let Some(j) = self.eq_just.get(&s) {
                    sids.extend(j.fact_sids.iter().copied());
                    clauses.extend(j.clause_parents.iter().copied());
                    if let Some(ax) = j.axiom { sids.push(ax); }
                }
                s = p;
                guard += 1;
                if guard > 1 << 20 { break; }
            }
        }
        sids.sort_unstable(); sids.dedup();
        clauses.sort_unstable(); clauses.dedup();
        (sids, clauses)
    }

    /// Union with a proof-forest label, queueing the equality for
    /// surfacing as a unit clause.  Internal to the FD fixpoint.
    fn union_just(&mut self, a: SymbolId, b: SymbolId, just: EqJust) {
        let (ra, rb) = (self.eq_rep(a), self.eq_rep(b));
        if ra == rb { return; }
        self.epoch.set(self.epoch.get() + 1);
        let (root, child) = if ra <= rb { (ra, rb) } else { (rb, ra) };
        self.eq.insert(child, root);
        if std::env::var_os("SIGMA_ORACLE_TRACE").is_some() {
            eprintln!("FD-MERGE {a:?} = {b:?} just={just:?}");
        }
        self.eq_just.insert(child, just.clone());
        self.pending_eq.push((a, b, just));
    }

    /// FD congruence fixpoint: per declaration, group that relation's
    /// observed facts by the key argument's CURRENT representative
    /// (guards checked against the live taxonomy, learned instances
    /// included); two guarded facts sharing a key with different value
    /// representatives merge the values.  Merging can collapse keys —
    /// of this or any other FD relation — so iterate until quiet.
    fn fd_fixpoint(&mut self) {
        if self.fd_decls.is_empty() {
            return;
        }
        loop {
            let mut merges: Vec<(SymbolId, SymbolId, EqJust)> = Vec::new();
            for (rel, decls) in self.fd_decls.iter() {
                let Some(facts) = self.fd_facts.get(rel) else { continue };
                for decl in decls {
                    let mut by_key: Map64<SymbolId, (SymbolId, &FdFact)> = Map64::default();
                    for f in facts {
                        let (key, val) = if decl.key_pos == 1 { (f.x, f.y) } else { (f.y, f.x) };
                        if !decl.key_guards.iter().all(|c| self.holds_instance(key, *c, None))
                            || !decl.val_guards.iter().all(|c| self.holds_instance(val, *c, None))
                        {
                            continue;
                        }
                        let (krep, vrep) = (self.eq_rep(key), self.eq_rep(val));
                        match by_key.get(&krep) {
                            None => { by_key.insert(krep, (vrep, f)); }
                            Some(&(prev_vrep, prev_f)) => {
                                if prev_vrep != vrep {
                                    let mut j = EqJust { axiom: decl.axiom, ..Default::default() };
                                    for g in [prev_f, f] {
                                        if let Some(c) = g.clause { j.clause_parents.push(c); }
                                    }
                                    merges.push((prev_vrep, vrep, j));
                                }
                            }
                        }
                    }
                }
            }
            if merges.is_empty() {
                break;
            }
            for (a, b, j) in merges {
                self.union_just(a, b, j);
            }
        }
    }

    /// Union with a FORCED root (the literal-preference path: numeric
    /// literals stay representatives so normalization rewrites symbols
    /// toward numbers, never the reverse).
    pub(crate) fn add_equality_rooted(&mut self, root: u64, child: u64) {
        self.epoch.set(self.epoch.get() + 1);
        let (rr, rc) = (self.eq_rep(root), self.eq_rep(child));
        if rr != rc {
            self.eq.insert(rc, rr);
            self.fd_fixpoint();
        }
    }

    /// The representative of `s`'s equality class (itself if unconstrained).
    pub(crate) fn eq_rep(&self, mut s: SymbolId) -> SymbolId {
        let mut guard = 0u32;
        while let Some(&p) = self.eq.get(&s) {
            if p == s { break; }
            s = p;
            guard += 1;
            if guard > 1 << 20 { break; } // defensive: chains are short
        }
        s
    }

    /// Whether any ground equality has been registered.
    pub(crate) fn has_equalities(&self) -> bool { !self.eq.is_empty() }

    /// Whether any class-disjointness is declared (gates the sorted filter).
    pub(crate) fn has_disjointness(&self) -> bool { !self.disjoint.is_empty() }

    /// Root sids of the decomposition/`disjoint` meaning axioms whose
    /// semantics this oracle supplies — the prover omits them from
    /// resolution (discharge-and-omit).  Empty unless the decomposition
    /// opt-in is active.
    pub(crate) fn decomposition_meaning_axioms(&self) -> &[SentenceId] {
        &self.disjoint.meaning
    }

    /// Entailment of a ground symbol equality `(equal x y)`:
    ///   * reflexivity / equality-class congruence (the union-find), or
    ///   * **subclass antisymmetry** — `X ⊆ Y ∧ Y ⊆ X ⇒ X = Y` is a SUMO
    ///     axiom (`subclass` is a partial order), so two mutually-
    ///     subclassing classes are equal.
    /// When `why` is `Some`, the two subclass chains are appended.
    pub(crate) fn equal_holds(
        &self,
        x: SymbolId,
        y: SymbolId,
        mut why: Option<&mut Vec<Witness>>,
    ) -> bool {
        if x == y {
            return true;
        }
        if self.eq_rep(x) == self.eq_rep(y) {
            if let Some(w) = why.as_deref_mut() {
                // Cite the facts/axioms whose merges connect the two
                // (the proof forest); deriving clauses surface through
                // `eq_explain` at the discharge site.
                let (sids, _) = self.eq_explain(x, y);
                for sid in sids {
                    w.push(Witness { rel: self.subclass_id, x, y, sid: Some(sid) });
                }
            }
            return true;
        }
        // Antisymmetry: both subclass directions ⇒ equal.
        let fwd = self.holds(self.subclass_id, x, y, why.as_deref_mut());
        if fwd && self.holds(self.subclass_id, y, x, why.as_deref_mut()) {
            return true;
        }
        false
    }

    /// Provable FALSEHOOD of `(instance x c)`: some class `x` is known
    /// to inhabit (stored or learned) is provably disjoint from `c`.
    /// Witnesses cite the instance fact that anchors the refutation.
    pub(crate) fn refutes_instance(
        &self,
        rel: SymbolId,
        x:   SymbolId,
        c:   SymbolId,
        mut why: Option<&mut Vec<Witness>>,
    ) -> bool {
        if rel != self.instance_id || self.disjoint.is_empty() {
            return false;
        }
        let direct: Vec<SymbolId> = self
            .sem
            .parents_of_scoped(x, self.scope)
            .into_iter()
            .filter(|(_, r)| matches!(r, TaxRelation::Instance))
            .map(|(d, _)| d)
            .chain(self.learned_objects(self.instance_id, x).collect::<Vec<_>>())
            .collect();
        for d in direct {
            if let Some((a1, a2, decl)) = self.provably_disjoint_chain(d, c) {
                if let Some(w) = why.as_deref_mut() {
                    // The membership that conflicts: `(instance x d)`.
                    w.push(Witness {
                        rel: self.instance_id, x, y: d,
                        sid: self.edge_fact_sid(self.instance_id, x, d),
                    });
                    // The subclass chains up to the disjoint ancestors, so the
                    // proof shows WHY d ⊑ a1 and c ⊑ a2 …
                    self.push_subclass_path(d, a1, w);
                    self.push_subclass_path(c, a2, w);
                    // … and the partition/disjoint declaration that makes
                    // a1 and a2 disjoint — the referee, as a full proof step.
                    w.push(Witness { rel: self.disjoint_id, x: a1, y: a2, sid: decl });
                }
                return true;
            }
        }
        false
    }

    /// Like [`provably_disjoint`], but returns the witnessing ancestor pair
    /// `(a1, a2)` (with `a1` an ancestor of `c1`, `a2` of `c2`) and the sid
    /// of the declaration that made them disjoint.  `None` if not disjoint.
    fn provably_disjoint_chain(
        &self, c1: SymbolId, c2: SymbolId,
    ) -> Option<(SymbolId, SymbolId, Option<SentenceId>)> {
        if self.disjoint.is_empty() {
            return None;
        }
        let a1 = self.ancestors_incl(c1);
        let a2 = self.ancestors_incl(c2);
        for x in &a1 {
            for y in &a2 {
                if self.disjoint.directly_disjoint(*x, *y) {
                    return Some((*x, *y, self.disjoint.pair_source(*x, *y)));
                }
            }
        }
        None
    }

    /// Push the `(subclass child parent)` edges along one path from `from`
    /// up to ancestor `to` (inclusive endpoints excluded when equal) as
    /// witnesses, so a refutation's proof traces the subclass chain.
    fn push_subclass_path(&self, from: SymbolId, to: SymbolId, w: &mut Vec<Witness>) {
        if from == to {
            return;
        }
        // BFS up the subclass graph, recording parent pointers, then walk back.
        let mut prev: std::collections::HashMap<SymbolId, SymbolId> =
            std::collections::HashMap::new();
        let mut queue = std::collections::VecDeque::from([from]);
        let mut found = false;
        while let Some(cur) = queue.pop_front() {
            if cur == to { found = true; break; }
            for (p, rel) in self.sem.parents_of_scoped(cur, self.scope) {
                if matches!(rel, TaxRelation::Subclass) && !prev.contains_key(&p) && p != from {
                    prev.insert(p, cur);
                    queue.push_back(p);
                }
            }
            if prev.len() > 256 { break; }
        }
        if !found {
            return;
        }
        // Reconstruct to → … → from, collecting (child, parent) edges, then
        // emit them REVERSED so the chain reads bottom-up from `from` (the
        // leaf class) toward `to` (the disjoint ancestor) — the order a
        // reader follows: `(subclass InvestmentAccount DepositAccount)`,
        // `(subclass DepositAccount FinancialAccount)`, … up to the ancestor.
        let mut edges: Vec<(SymbolId, SymbolId)> = Vec::new();
        let mut node = to;
        while node != from {
            let Some(&child) = prev.get(&node) else { break };
            edges.push((child, node));
            node = child;
        }
        for (child, parent) in edges.into_iter().rev() {
            w.push(Witness {
                rel: self.subclass_id, x: child, y: parent,
                sid: self.edge_fact_sid(self.subclass_id, child, parent),
            });
        }
    }

    /// `true` if `c1` and `c2` are provably disjoint — some ancestor of
    /// `c1` is declared disjoint from some ancestor of `c2`.  Cheap
    /// short-circuit when no disjointness is declared at all.
    pub(crate) fn provably_disjoint(&self, c1: SymbolId, c2: SymbolId) -> bool {
        if self.disjoint.is_empty() {
            return false;
        }
        let a1 = self.ancestors_incl(c1);
        let a2 = self.ancestors_incl(c2);
        a1.iter().any(|x| a2.iter().any(|y| self.disjoint.directly_disjoint(*x, *y)))
    }

    /// `c` and its subclass ancestors (inclusive), in the problem scope.
    fn ancestors_incl(&self, c: SymbolId) -> Vec<SymbolId> {
        let mut out = vec![c];
        let mut i = 0;
        while i < out.len() {
            let cur = out[i];
            i += 1;
            for (p, rel) in self.sem.parents_of_scoped(cur, self.scope) {
                if matches!(rel, TaxRelation::Subclass) && !out.contains(&p) {
                    out.push(p);
                }
            }
            if out.len() > 256 { break; } // defensive bound
        }
        out
    }

    /// Whether a ground symbol-headed relation atom `(rel a1 … an)` is
    /// **provably ill-sorted** — some ground symbol argument's class is
    /// disjoint from the position's declared `domain` (or, for
    /// `domainSubclass`, the argument class is disjoint from the required
    /// superclass, so it can't be a subclass of it).  Sound one-way
    /// check: returns `true` only on a provable type violation, never on
    /// merely-unproven typing, so it never rejects a well-typed atom.
    /// `args` are the argument symbol ids in order (None for non-symbol
    /// arguments, which are skipped).
    pub(crate) fn ill_sorted(&self, rel: SymbolId, args: &[Option<SymbolId>]) -> bool {
        if self.disjoint.is_empty() {
            return false;
        }
        let domain = self.sem.domain_scoped(rel, self.scope);
        for (i, arg) in args.iter().enumerate() {
            let Some(arg) = arg else { continue };
            let Some(rd) = domain.get(i) else { continue };
            match rd {
                RelationDomain::Domain(c) => {
                    // `arg` must be an instance of `c`: ill-sorted if any
                    // class `arg` is an instance of is disjoint from `c`.
                    for (cls, rel2) in self.sem.parents_of_scoped(*arg, self.scope) {
                        if matches!(rel2, TaxRelation::Instance)
                            && self.provably_disjoint(cls, *c)
                        {
                            return true;
                        }
                    }
                }
                RelationDomain::DomainSubclass(c) => {
                    // `arg` (a class) must be a subclass of `c`: ill-sorted
                    // if `arg` is disjoint from `c`.
                    if self.provably_disjoint(*arg, *c) {
                        return true;
                    }
                }
                RelationDomain::Unknown => {}
            }
        }
        false
    }

    fn learned_objects(&self, rel: SymbolId, x: SymbolId) -> impl Iterator<Item = SymbolId> + '_ {
        self.learned
            .get(&rel)
            .and_then(|m| m.get(&x))
            .into_iter()
            .flat_map(|s| s.keys().copied())
    }

    /// Entailment of the ground binary atom `(rel x y)`.  When `why` is
    /// `Some`, the witnessing facts are appended on success.
    pub(crate) fn holds(
        &self,
        rel: SymbolId,
        x:   SymbolId,
        y:   SymbolId,
        why: Option<&mut Vec<Witness>>,
    ) -> bool {
        // Witness-collecting calls bypass the memo (a hit cannot
        // reproduce the witness chain).  Callers needing witnesses
        // gate on a witness-free call first, so the expensive path
        // runs only on entailed atoms.
        if why.is_some() {
            return self.holds_uncached(rel, x, y, why);
        }
        if let Some(&(ep, res)) = self.holds_memo.borrow().get(&(rel, x, y)) {
            if res || ep == self.epoch.get() {
                return res;
            }
        }
        let res = self.holds_uncached(rel, x, y, None);
        self.holds_memo
            .borrow_mut()
            .insert((rel, x, y), (self.epoch.get(), res));
        res
    }

    /// Ground binary `(rel x y)` argument pairs from the store (base ∪
    /// session) — the temporal network's fact source, each tagged with the
    /// asserting sentence's sid for witness provenance.
    fn ground_pairs(&self, rel: SymbolId) -> Vec<(SymbolId, SymbolId, Option<SentenceId>)> {
        let mut out = Vec::new();
        for sid in self.sem.syntactic.by_head_id(&rel).iter() {
            let Some(s) = self.sem.syntactic.sentence(*sid) else { continue };
            if s.elements.len() == 3 {
                if let (Some(Element::Symbol(a)), Some(Element::Symbol(b))) =
                    (s.elements.get(1), s.elements.get(2))
                {
                    out.push((a.id(), b.id(), Some(*sid)));
                }
            }
        }
        out
    }

    /// Best-effort temporal discharge: does the interval/point network
    /// ENTAIL `(rel x y)`?  Gated by `SIGMA_TEMPORAL` (the point-network
    /// fragment escalates everything else to resolution).  Lazily builds
    /// the network from the KB's temporal facts on first use.
    fn temporal_entails(
        &self,
        rel: SymbolId,
        x:   SymbolId,
        y:   SymbolId,
        why: Option<&mut Vec<Witness>>,
    ) -> bool {
        if std::env::var_os("SIGMA_TEMPORAL").is_none() {
            return false;
        }
        self.temporal_entails_ungated(rel, x, y, why)
    }

    /// Ungated temporal entailment for callers that scope the decision
    /// themselves — the rule-join discharge consults the point network for
    /// its ground body checks even when the global `SIGMA_TEMPORAL` oracle
    /// gate is unset, so the network's use stays confined to that pass.
    /// Never touches the `holds` memo (a direct query), so baseline
    /// `holds()` behavior is unchanged.
    pub(crate) fn temporal_holds(
        &self,
        rel: SymbolId,
        x:   SymbolId,
        y:   SymbolId,
        why: Option<&mut Vec<Witness>>,
    ) -> bool {
        self.temporal_entails_ungated(rel, x, y, why)
    }

    /// [`temporal_entails`](Self::temporal_entails) minus the env gate: the
    /// shared lazy-network build + query used by both the gated `holds()`
    /// path and the join-scoped [`temporal_holds`](Self::temporal_holds).
    fn temporal_entails_ungated(
        &self,
        rel: SymbolId,
        x:   SymbolId,
        y:   SymbolId,
        why: Option<&mut Vec<Witness>>,
    ) -> bool {
        let ids = super::temporal::TemporalRelIds::standard();
        if !ids.is_temporal(rel) {
            return false;
        }
        let tp = ids.time_point;
        if self.temporal.borrow().is_none() {
            let net = super::temporal::build_net(
                &ids,
                |r| self.ground_pairs(r),
                |s| self.holds_instance(s, tp, None),
            );
            *self.temporal.borrow_mut() = Some(net);
        }
        let mut guard = self.temporal.borrow_mut();
        let net = guard.as_mut().expect("temporal net built above");
        let held = super::temporal::query(net, &ids, rel, x, y, |s| self.holds_instance(s, tp, None));
        if held {
            if let Some(w) = why {
                // The temporal facts (starts/meets/finishes/temporalPart/…)
                // along the entailing endpoint path become proof premises.
                for sid in super::temporal::query_witness(net, &ids, rel, x, y, |s| {
                    self.holds_instance(s, tp, None)
                }) {
                    w.push(Witness { rel, x, y, sid: Some(sid) });
                }
            }
        }
        held
    }

    fn holds_uncached(
        &self,
        rel: SymbolId,
        x:   SymbolId,
        y:   SymbolId,
        mut why: Option<&mut Vec<Witness>>,
    ) -> bool {
        // Best-effort temporal point-network discharge (cross-relation
        // interval composition the transitive-closure path can't reach).
        if self.temporal_entails(rel, x, y, why.as_deref_mut()) {
            return true;
        }
        if rel == self.instance_id {
            return self.holds_instance(x, y, why);
        }
        if rel == self.subclass_id {
            if x == y { return true; }
            return match self.sub_chain(x, y) {
                Some(chain) => {
                    if let Some(w) = why.as_deref_mut() { w.extend(chain); }
                    true
                }
                None => false,
            };
        }

        // Generic relation: direct (possibly subrelation-inherited) edge…
        let below = self.sem.subrel_below(rel, self.scope);
        if let Some(ws) = self.edge_why(&below, rel, x, y) {
            if let Some(w) = why.as_deref_mut() { w.extend(ws); }
            return true;
        }
        // …the REVERSED edge when rel is symmetric (stored facts keep
        // their written argument order; the prover orients only what
        // flows through `make`, so the oracle must close the gap)…
        if x != y && self.is_symmetric(rel) {
            if let Some(ws) = self.edge_why(&below, rel, y, x) {
                if let Some(w) = why.as_deref_mut() {
                    w.extend(ws);
                    w.push(Witness {
                        rel: self.instance_id, x: rel, y: self.symmetric_id,
                        sid: self.symmetric_source(rel),
                    });
                }
                return true;
            }
        }
        // …or reachability when rel is a TransitiveRelation.
        if !self.is_transitive(rel) {
            return false;
        }
        let found = if self.learned.keys().any(|r| below.contains_key(r)) {
            // Learned edges participate: walk manually over base ∪ overlay.
            self.reach_with_learned(&below, x, y)
        } else {
            // Pure base/session edges: ride the memoized reachability cache.
            let reach = self.sem.trans_reach(rel, x, self.scope);
            reach.contains_key(&y).then(|| {
                let mut hops = Vec::new();
                let mut cur = y;
                while cur != x {
                    let (prev, sid) = reach[&cur];
                    hops.push(Witness { rel, x: prev, y: cur, sid: Some(sid) });
                    cur = prev;
                }
                hops.reverse();
                hops
            })
        };
        match found {
            Some(hops) => {
                if let Some(w) = why.as_deref_mut() {
                    w.extend(hops);
                    let (inst, tr) = (self.instance_id, self.transitive_id);
                    w.push(Witness {
                        rel: inst, x: rel, y: tr,
                        // Declared: the declaration fact.  Rule-mined:
                        // the transitivity axiom itself.
                        sid: self.edge_fact_sid(inst, rel, tr)
                            .or_else(|| self.trans_mined.get(&rel).copied().flatten()),
                    });
                }
                true
            }
            None => false,
        }
    }

    /// `(instance x y)`: a direct class, or a direct class with `y` up
    /// its subclass chain.
    fn holds_instance(&self, x: SymbolId, y: SymbolId, mut why: Option<&mut Vec<Witness>>) -> bool {
        let inst = self.instance_id;
        let direct: Vec<SymbolId> = self
            .sem
            .parents_of_scoped(x, self.scope)
            .into_iter()
            .filter(|(_, r)| matches!(r, TaxRelation::Instance))
            .map(|(c, _)| c)
            .chain(self.learned_objects(inst, x).collect::<Vec<_>>())
            .collect();
        if direct.contains(&y) {
            if let Some(w) = why.as_deref_mut() {
                w.push(Witness { rel: inst, x, y, sid: self.edge_fact_sid(inst, x, y) });
            }
            return true;
        }
        for c in direct {
            if let Some(chain) = self.sub_chain(c, y) {
                if let Some(w) = why.as_deref_mut() {
                    w.push(Witness { rel: inst, x, y: c, sid: self.edge_fact_sid(inst, x, c) });
                    w.extend(chain);
                }
                return true;
            }
        }
        false
    }

    /// The subclass-unit chain `c → … → y` as witnesses, or `None` when
    /// `y` is not up `c`'s chain.  Parent-pointer BFS over the scoped
    /// subclass edges ∪ the learned subclass overlay.
    fn sub_chain(&self, c: SymbolId, y: SymbolId) -> Option<Vec<Witness>> {
        if c == y { return Some(Vec::new()); }
        let sub = self.subclass_id;
        let mut par: Map64<SymbolId, SymbolId> = Map64::default();
        par.insert(c, c);
        let mut stack = vec![c];
        while let Some(a) = stack.pop() {
            let parents: Vec<SymbolId> = self
                .sem
                .parents_of_scoped(a, self.scope)
                .into_iter()
                .filter(|(_, r)| matches!(r, TaxRelation::Subclass))
                .map(|(d, _)| d)
                .chain(self.learned_objects(sub, a).collect::<Vec<_>>())
                .collect();
            for d in parents {
                if let std::collections::hash_map::Entry::Vacant(e) = par.entry(d) {
                    e.insert(a);
                    if d == y {
                        let mut chain = Vec::new();
                        let mut cur = y;
                        while par[&cur] != cur {
                            let p = par[&cur];
                            chain.push(Witness {
                                rel: sub, x: p, y: cur,
                                sid: self.edge_fact_sid(sub, p, cur),
                            });
                            cur = p;
                        }
                        chain.reverse();
                        return Some(chain);
                    }
                    stack.push(d);
                }
            }
        }
        None
    }

    /// Witnesses for a direct (possibly subrelation-inherited) edge
    /// `x →[r∈below(rel)] y`: the fact itself plus the `r → rel` chain.
    fn edge_why(
        &self,
        below: &crate::semantics::caches::subrel_lattice::BelowMap,
        rel:   SymbolId,
        x:     SymbolId,
        y:     SymbolId,
    ) -> Option<Vec<Witness>> {
        for (&r, _) in below.iter() {
            // Stored facts…
            let stored = self
                .sem
                .ground_binary_objects(r, x, self.scope)
                .into_iter()
                .find(|(obj, _)| *obj == y)
                .map(|(_, sid)| Some(sid));
            // …or a learned edge.
            let hit = stored.or_else(|| {
                self.learned_objects(r, x).any(|b| b == y).then_some(None)
            });
            let Some(sid) = hit else { continue };
            let mut ws = vec![Witness { rel: r, x, y, sid }];
            // The chain r → … → rel through the lattice's parent pointers.
            let sr = self.subrelation_id;
            let mut cur = r;
            while let Some(Some((up, rule_sid))) = below.get(&cur) {
                ws.push(Witness {
                    rel: sr, x: cur, y: *up,
                    sid: rule_sid.or_else(|| self.edge_fact_sid(sr, cur, *up)),
                });
                cur = *up;
            }
            return Some(ws);
        }
        None
    }

    /// Reachability x →* y over below-set edges ∪ the learned overlay,
    /// with witness hops.  The manual path — only taken when learned
    /// edges could extend the memoized base graph.
    fn reach_with_learned(
        &self,
        below: &crate::semantics::caches::subrel_lattice::BelowMap,
        x:     SymbolId,
        y:     SymbolId,
    ) -> Option<Vec<Witness>> {
        let mut par: Map64<SymbolId, (SymbolId, SymbolId, Option<SentenceId>)> = Map64::default();
        let mut stack = vec![x];
        let mut seen: HashSet<SymbolId> = HashSet::from([x]);
        while let Some(a) = stack.pop() {
            for (&r, _) in below.iter() {
                let stored = self.sem.ground_binary_objects(r, a, self.scope)
                    .into_iter().map(move |(b, sid)| (b, r, Some(sid)));
                let learned = self.learned_objects(r, a)
                    .map(move |b| (b, r, None))
                    .collect::<Vec<_>>();
                for (b, r2, sid) in stored.chain(learned) {
                    if seen.insert(b) {
                        par.insert(b, (a, r2, sid));
                        if b == y {
                            let mut hops = Vec::new();
                            let mut cur = y;
                            while cur != x {
                                let (prev, r3, s3) = par[&cur];
                                hops.push(Witness { rel: r3, x: prev, y: cur, sid: s3 });
                                cur = prev;
                            }
                            hops.reverse();
                            return Some(hops);
                        }
                        stack.push(b);
                    }
                }
            }
        }
        None
    }

    /// Is `rel` a TransitiveRelation (direct, inherited, learned, or
    /// rule-mined)?
    fn is_transitive(&self, rel: SymbolId) -> bool {
        self.trans_mined.contains_key(&rel)
            || self.holds_instance(rel, self.transitive_id, None)
    }

    /// The sid of the stored fact `(head x y)` visible in scope, if any
    /// — what turns a closure step into a file:line-citable witness.
    fn edge_fact_sid(&self, head: SymbolId, x: SymbolId, y: SymbolId) -> Option<SentenceId> {
        self.sem
            .ground_binary_objects(head, x, self.scope)
            .into_iter()
            .find(|(obj, _)| *obj == y)
            .map(|(_, sid)| sid)
    }
}

/// Structural equality of two atom elements (span/var_index-blind) —
/// the equality-reflexivity comparison.
fn element_eq(a: &Element, b: &Element) -> bool {
    match (a, b) {
        (Element::Symbol(x), Element::Symbol(y)) => x == y,
        (Element::Variable { id: x, .. }, Element::Variable { id: y, .. }) => x == y,
        (Element::Literal(x), Element::Literal(y)) => x == y,
        (Element::Sub(x), Element::Sub(y)) => x == y, // content hash == identity
        (Element::Op(x), Element::Op(y)) => x == y,
        _ => false,
    }
}
