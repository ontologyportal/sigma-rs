// crates/core/src/prover/saturate/theory.rs
//
// The prover ⇄ theory-module contract — the explicit trait behind what
// used to be an implicit dependency of `NativeProver` on
// `SemanticOracle`.  This is the unification seam of the
// oracle-generalization plan: any engine that can DECIDE ground atoms,
// CITE the facts behind its decisions, absorb FEEDBACK from the
// saturation, and BAIL soundly on everything else can sit behind this
// trait and serve the same discharge sites (`make`'s per-literal
// theory propagation, the join/discharge passes, the
// discharge-and-omit site in `prove.rs`).
//
// The reference implementation is [`SemanticOracle`] (taxonomy /
// subrelation / transitivity closures + equality + FD congruence +
// disjointness + temporal point network).  The intended SECOND
// implementation is a `ModelProgram`-backed engine (the Datalog-ish
// inductive models under `model/`): it would answer `holds` from its
// materialized IDB, cite through evaluation provenance (`cite`), and
// claim `PositiveOnly` coverage for the relations its rules define —
// it can confirm membership in a least model but (unlike the taxonomy
// oracle's disjointness machinery) has no refutation power, so its
// claims must never license negative discharge.
//
// [`SemanticOracle`]: super::oracle::SemanticOracle

use crate::semantics::roles::TaxonomyRoles;
use crate::types::{SentenceId, SymbolId};

use super::oracle::{EqJust, FdDecl, OracleSnapshot, Witness};

/// How completely a theory module decides a relation it claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Coverage {
    /// The module is authoritative for the relation's decidable ground
    /// fragment: a TRUE answer is entailment, and the module also
    /// carries whatever refutation power exists for the relation
    /// (e.g. disjointness-based `refutes_instance`).  Complete claims
    /// are what license treating the relation as
    /// checked-never-enumerated in the join passes.
    Complete,
    /// The module can only CONFIRM positives (membership in a least
    /// model); a FALSE answer means "unknown", never refutation.
    /// Intended for the `ModelProgram`-backed implementation — no
    /// module claims this today.
    #[allow(dead_code)]
    PositiveOnly,
}

/// One per-relation ownership claim: this module decides ground atoms
/// of `rel` (to the strength of `coverage`), so the prover's join
/// passes must CHECK the relation through [`TheoryOracle::holds`]
/// rather than ENUMERATE its facts as a join generator — the
/// generative axioms behind such relations are exactly what the joins
/// are starving.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RelationClaim {
    pub(crate) rel: SymbolId,
    #[allow(dead_code)]
    pub(crate) coverage: Coverage,
}

/// A theory module's declared footprint: which relations it owns, and
/// which stored axioms it supplants.
///
/// * `claims` — the checked-never-enumerated relation set (what the
///   old hardcoded `is_theory_rel` role/temporal lists encoded).
/// * `omitted_axioms` — root sids whose SEMANTICS the module supplies
///   (e.g. the `disjoint`/`partitionN` meaning axioms): the prover may
///   omit them from resolution (discharge-and-omit) because every
///   discharge the module performs still cites real KB facts.
#[derive(Debug, Clone, Default)]
pub(crate) struct CoverageClaim {
    pub(crate) claims: Vec<RelationClaim>,
    pub(crate) omitted_axioms: Vec<SentenceId>,
}

impl CoverageClaim {
    /// Whether some module claim covers `rel` (any coverage strength).
    /// A short linear scan — claim sets are a dozen-odd relations, the
    /// same cost profile as the comparison chain this replaced.
    pub(crate) fn owns(&self, rel: SymbolId) -> bool {
        self.claims.iter().any(|c| c.rel == rel)
    }
}

/// The surface the saturation prover consumes from a theory module.
///
/// The contract, in the order a literal flows through it:
///
/// * **decide** — [`holds`](Self::holds) /
///   [`equal_holds`](Self::equal_holds) answer ground binary
///   entailment; [`refutes_instance`](Self::refutes_instance) /
///   [`ill_sorted`](Self::ill_sorted) answer provable FALSEHOOD.
///   All four are sound one-way checks: TRUE is a theorem of the
///   module's theory, FALSE only means "not decided here".
/// * **cite** — every positive decision can append [`Witness`]es (the
///   unit facts, with store sids, whose closure entails the atom);
///   the prover surfaces them as proof-step premises, so a module
///   must never answer TRUE without being able to justify it.
/// * **feedback** — derived ground knowledge flows back in
///   ([`add_unit`](Self::add_unit) /
///   [`add_neg_unit`](Self::add_neg_unit) /
///   [`add_equality`](Self::add_equality) / `register_*`), growing a
///   per-problem learned overlay MONOTONICALLY.  [`epoch`](Self::epoch)
///   versions that growth: TRUE answers are stable forever, FALSE
///   answers are valid only at the epoch they were computed — external
///   memos (e.g. the prover's `sym_swap_memo`) key negative results on
///   it.  Feedback may derive NEW facts/equalities (exhaustiveness
///   case-elimination, FD congruence); the prover drains those via
///   [`take_pending_facts`](Self::take_pending_facts) /
///   [`take_pending_eq`](Self::take_pending_eq) and surfaces them as
///   activated unit clauses.  The two drains are deliberately separate
///   (not one `drain_pending`): making the drained FACTS feeds
///   `add_unit` back into the module, which can enqueue new pending
///   EQUALITIES that the later eq-drain must still see.
/// * **bail** — anything outside the module's decidable fragment
///   (non-ground, non-binary, unclaimed relations) returns
///   FALSE/`None` and escalates to ordinary resolution.  Atom
///   admission (`SemanticOracle::decode`) stays module-internal: the
///   prover hands over `(rel, x, y)` symbol triples it has already
///   shaped, never raw literals.
/// * **coverage** — [`coverage`](Self::coverage) declares the module's
///   footprint up front: owned relations (checked-never-enumerated in
///   the join passes) and stored axioms whose semantics the module
///   supplies (licensed for omission from resolution).
///
/// Rehydration (`SemanticOracle::from_snapshot`) is constructor-shaped
/// — it re-supplies the semantic-layer borrow — so it stays on the
/// concrete type; the trait only captures the freeze half
/// ([`snapshot`](Self::snapshot)).  `OracleSnapshot` is currently the
/// `SemanticOracle`'s owned state; a second implementation will need
/// either its own snapshot channel or `OracleSnapshot` growing into a
/// sum type.
///
/// The prover's `oracle` field keeps the concrete type (generics would
/// infect every `NativeProver` signature for no gain); the trait earns
/// its keep at the consumer seams and as the compiler-checked contract
/// a second module must satisfy.  It is dyn-compatible by design (see
/// the assertion below) so seams can take `&dyn TheoryOracle` where
/// that costs nothing.
pub(crate) trait TheoryOracle {
    // -- decide (sound one-way checks; `why` collects citations) ------

    /// Entailment of the ground binary atom `(rel x y)`.  On success
    /// with `why = Some`, appends the witnessing facts.
    fn holds(&self, rel: SymbolId, x: SymbolId, y: SymbolId, why: Option<&mut Vec<Witness>>)
        -> bool;

    /// Temporal point-network entailment of `(rel x y)`, bypassing the
    /// global `SIGMA_TEMPORAL` gate — for callers (the join passes)
    /// that scope the decision themselves.
    fn temporal_holds(
        &self,
        rel: SymbolId,
        x: SymbolId,
        y: SymbolId,
        why: Option<&mut Vec<Witness>>,
    ) -> bool;

    /// Entailment of ground `(equal x y)` at the equality-class level
    /// (reflexivity / congruence closure / subclass antisymmetry).
    fn equal_holds(&self, x: SymbolId, y: SymbolId, why: Option<&mut Vec<Witness>>) -> bool;

    /// Provable FALSEHOOD of `(instance x c)` via class disjointness.
    /// `rel` must be the module's `instance` role or the answer is
    /// FALSE.
    fn refutes_instance(
        &self,
        rel: SymbolId,
        x: SymbolId,
        c: SymbolId,
        why: Option<&mut Vec<Witness>>,
    ) -> bool;

    /// Whether `(rel a1 … an)` is provably ill-sorted (an argument's
    /// class disjoint from the position's declared domain).  Never
    /// TRUE on merely-unproven typing.
    fn ill_sorted(&self, rel: SymbolId, args: &[Option<SymbolId>]) -> bool;

    /// Is `rel` symmetric (declared or rule-mined)?
    fn is_symmetric(&self, rel: SymbolId) -> bool;

    /// The citable source of `rel`'s symmetry (declaration fact or
    /// mined axiom sid).
    fn symmetric_source(&self, rel: SymbolId) -> Option<SentenceId>;

    /// The proof-DAG source clause of a learned edge, when recorded —
    /// how a discharge against fed-back knowledge cites the derivation.
    fn learned_src(&self, rel: SymbolId, x: SymbolId, y: SymbolId) -> Option<u32>;

    // -- equality closure ---------------------------------------------

    /// The representative of `s`'s equality class (itself if
    /// unconstrained).  The prover normalizes ground arguments to
    /// representatives before indexing.
    fn eq_rep(&self, s: SymbolId) -> SymbolId;

    /// The proof-forest labels connecting `a` and `b`: (fact sids,
    /// deriving clause ids) — the premises behind an equality
    /// discharge.
    fn eq_explain(&self, a: SymbolId, b: SymbolId) -> (Vec<SentenceId>, Vec<u32>);

    /// Whether any ground equality has been registered (gates the
    /// normalization pass).
    fn has_equalities(&self) -> bool;

    /// Whether any class-disjointness is declared (gates the
    /// sorted-relation filter).
    fn has_disjointness(&self) -> bool;

    // -- feedback (monotone; every mutation bumps the epoch) ----------

    /// Feed back a derived positive ground unit `(rel x y)`; `src` is
    /// the deriving clause id (proof-DAG anchor), upgraded but never
    /// downgraded on re-registration.
    fn add_unit(&mut self, rel: SymbolId, x: SymbolId, y: SymbolId, src: Option<u32>);

    /// Feed back a derived NEGATIVE ground unit `¬(rel x y)` — the
    /// exclusions exhaustiveness case-elimination works against.
    fn add_neg_unit(&mut self, rel: SymbolId, x: SymbolId, y: SymbolId, src: Option<u32>);

    /// Union two ground equality-class keys (smallest id becomes
    /// representative).  `src`, when given, is the deriving clause id —
    /// recorded so `eq_explain` can cite it as a proof-DAG parent the next
    /// time this merge causes a `normalize_eq` rewrite.
    fn add_equality(&mut self, a: SymbolId, b: SymbolId, src: Option<u32>);

    /// Union with a FORCED root — the literal-preference path (numeric
    /// literals stay representatives so rewriting moves toward
    /// numbers).  `src` as in [`Self::add_equality`].
    fn add_equality_rooted(&mut self, root: u64, child: u64, src: Option<u32>);

    /// Declare a functional dependency on `rel` (mined uniqueness
    /// axioms / `SingleValuedRelation` declarations).
    fn register_fd(&mut self, rel: SymbolId, decl: FdDecl);

    /// Register a rule-mined symmetric relation (schema channel).
    fn register_symmetric(&mut self, rel: SymbolId, sid: Option<SentenceId>);

    /// Register a rule-mined transitive relation (schema channel).
    fn register_transitive(&mut self, rel: SymbolId, sid: Option<SentenceId>);

    /// The current knowledge epoch — bumped by every feedback call.
    /// TRUE decisions are stable across epochs; FALSE decisions are
    /// valid only at the epoch they were computed.
    fn epoch(&self) -> u64;

    // -- taxonomy roles -----------------------------------------------

    /// The taxonomy-role ids currently in force (recognized or the
    /// English-name defaults).
    fn roles(&self) -> TaxonomyRoles;

    /// Install shape-recognized taxonomy-role ids
    /// (`Strategy.recognize_roles`); no-op-safe with the defaults.
    fn set_roles(&mut self, roles: TaxonomyRoles);

    // -- pending-derivation drains (see the trait doc for why two) ----

    /// Positive facts derived by feedback (exhaustiveness
    /// case-elimination) since the last drain.
    fn take_pending_facts(&mut self) -> Vec<(SymbolId, SymbolId, SymbolId, EqJust)>;

    /// Equalities derived by feedback (FD congruence) since the last
    /// drain.
    fn take_pending_eq(&mut self) -> Vec<(SymbolId, SymbolId, EqJust)>;

    // -- coverage -------------------------------------------------------

    /// The module's declared footprint: owned relations + axioms
    /// licensed for omission.  Must be stable within an epoch and
    /// reflect the roles currently in force.
    fn coverage(&self) -> CoverageClaim;

    // -- snapshot -------------------------------------------------------

    /// Capture the module's owned state for the frozen-background
    /// cache (rehydration is `SemanticOracle::from_snapshot`).
    fn snapshot(&self) -> OracleSnapshot;
}

// The contract must stay dyn-compatible — a second module behind
// `&dyn TheoryOracle` is the whole point of the seam.
const _: Option<&dyn TheoryOracle> = None;
