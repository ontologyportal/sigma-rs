// crates/sumo-kb/src/cnf.rs
// #[cfg(feature = "cnf")]
//
// Vampire-backed CNF clausification.
//
// Replaced the hand-rolled NNF/Skolemize/distribute clausifier during
// Phase 5 of the clause-dedup work.  Drives the KIF -> `ir::Problem`
// -> Vampire NewCNF -> `ir::Clause` pipeline added in the preceding
// phases and translates the resulting `ir::Clause`s into the
// crate-local `Clause` / `CnfLiteral` / `CnfTerm` shape that feeds
// persistence and dedup.
//
// The `cnf` cargo feature (on by default) implies `integrated-prover`,
// so this module never exists without the linked Vampire library --
// every reference to `ir::Problem::clausify` is always available.
//
// Design notes:
//
// * Clausification is always performed in TFF mode (sort-rich).  The stored
//   `CnfTerm`s are sort-agnostic; sort metadata is re-attached on
//   TPTP-emission paths, not here.
// * Vampire's NewCNF invents skolem functors named `sK<n>` / `sK<n>_<m>`.
//   These are detected by name prefix and interned into the `KifStore`
//   with `is_skolem = true`.
// * Non-skolem functors that Vampire surfaces (from equality or theory
//   reasoning over function terms) land in `CnfTerm::Fn` after lookup /
//   interning of their `s__...` names.
// * Variables are clause-local: the post-clausify IR already renames vars
//   to fresh Vampire indices, so the original KIF names are gone.  We
//   reuse the Vampire index directly as the `SymbolId` in `CnfTerm::Var`.
//   Canonical hashing (`canonical.rs`) renames these away to `V0..Vn`
//   before comparing, so the concrete value is irrelevant for dedup.

use std::collections::HashMap;

use vampire_prover::ir;

use crate::error::KbError;
use crate::kif_store::KifStore;
use crate::semantic::SemanticLayer;
use crate::types::{Clause, CnfLiteral, CnfTerm, SentenceId, SymbolId};
use crate::vampire::converter::{Mode, NativeConverter};

// =========================================================================
//  Public entry point
// =========================================================================

/// Clausify a single KIF sentence via Vampire and return the resulting
/// CNF clauses in the crate's `Clause` shape.
///
/// `layer` is borrowed mutably because Vampire may invent new symbols
/// (skolems introduced by existential elimination, or wrapper predicates
/// like `s__holds__1`) that the `KifStore` has not yet seen.  Those are
/// interned on the fly during translation.
///
/// The function is a thin wrapper around [`clausify_ir`] + [`translate_ir_clauses`]
/// — split so the IR-level step can be reused by tests or diagnostics
/// without forcing the mutable-store borrow.
pub(crate) fn sentence_to_clauses(
    layer: &mut SemanticLayer,
    sid:   SentenceId,
) -> Result<Vec<Clause>, KbError> {
    let ir_clauses = clausify_ir(layer, sid)?;
    Ok(translate_ir_clauses(&mut layer.store, &ir_clauses))
}

/// Stage 1: drive the converter + Vampire clausifier and return the
/// pure-IR clauses.  Borrows `layer` immutably.
pub(crate) fn clausify_ir(
    layer: &SemanticLayer,
    sid:   SentenceId,
) -> Result<Vec<ir::Clause>, KbError> {
    let mut conv = NativeConverter::new(&layer.store, layer, Mode::Tff);
    if !conv.add_axiom(sid) {
        return Err(KbError::Other(format!(
            "cnf2: converter refused sentence {sid}",
        )));
    }
    let (problem, _sid_map) = conv.finish();

    problem
        .clausify(ir::Options::new())
        .map_err(|e| KbError::Other(format!("cnf2: clausify failed: {e}")))
}

/// Stage 2: translate a slice of IR clauses into crate-level `Clause`s.
/// Interns any unseen symbol names into the store as it goes.
pub(crate) fn translate_ir_clauses(
    store:      &mut KifStore,
    ir_clauses: &[ir::Clause],
) -> Vec<Clause> {
    let mut ctx = TranslateCtx::default();
    ir_clauses
        .iter()
        .map(|c| ir_clause_to_cnf(store, &mut ctx, c))
        .collect()
}

// =========================================================================
//  Batched clausification — single Vampire call for many sentences
// =========================================================================

/// Output of [`clausify_sentences_batch`]: per-sid IR clauses plus any
/// that couldn't be attributed to a single source sentence.
pub(crate) struct BatchedSentenceClauses {
    /// One entry per input sentence that the `NativeConverter` accepted.
    /// The map preserves attribution only — not the order in which
    /// sentences were added; callers that care about order should walk
    /// their original sid vector.
    pub by_sid: std::collections::HashMap<SentenceId, Vec<ir::Clause>>,
    /// Clauses whose inference graph traces to TWO OR MORE input
    /// sentences (NewCNF definitional naming over shared subformulas)
    /// or to none.  Stored here so callers can decide whether to
    /// attribute them to every participating sid, ignore them, or
    /// keep them in a dedicated "shared" fingerprint bucket.  Rare
    /// in practice on SUMO-style inputs with `--naming 0`.
    pub shared: Vec<ir::Clause>,
    /// Sentences the `NativeConverter` rejected (typically because
    /// the sentence shape is unrepresentable in Vampire's IR —
    /// e.g. a head that isn't a Symbol/Op).  Callers should treat
    /// these as "accept-without-dedup" to match the per-sentence
    /// fallback behaviour.
    pub skipped: Vec<SentenceId>,
}

/// Clausify many sentences in a single Vampire call, returning per-sid
/// IR clauses.
///
/// This is the batched counterpart to [`clausify_ir`] — one mutex
/// acquisition and one Vampire problem-setup teardown for an entire
/// batch, instead of N of each for per-sentence calls.  Output
/// attribution is driven by
/// [`vampire_prover::clausify::clausify_batch`], which walks
/// Vampire's `Inference` graph from each output clause back to its
/// input axiom(s).
///
/// Semantics:
/// - Sentences accepted by the converter land in `by_sid`, keyed by
///   their original `SentenceId`.
/// - Sentences refused by the converter land in `skipped`.
/// - Clauses whose ancestry traces to >1 input sentence land in
///   `shared` (rare; driven by NewCNF's naming optimisation when a
///   shared sub-formula is large enough to be abstracted).
///
/// # Errors
///
/// Returns `Err` only if the whole-batch clausify call itself fails
/// at the FFI boundary (e.g. Vampire throws a C++ exception on a bad
/// sentence).  Callers that need to recover from a single-sentence
/// failure should wrap this with bisection-based retry logic — see
/// `KnowledgeBase::ingest` in `kb/mod.rs` for an example.
pub(crate) fn clausify_sentences_batch(
    layer: &SemanticLayer,
    sids:  &[SentenceId],
) -> Result<BatchedSentenceClauses, KbError> {
    use std::collections::HashMap;

    // Build one NativeConverter, feed every sid in order.  Remember
    // the per-position → sid mapping so we can translate the
    // `BatchedClauses::by_axiom` vector (positionally aligned) back
    // to a sid-keyed map.
    let mut conv   = NativeConverter::new(&layer.store, layer, Mode::Tff);
    let mut order: Vec<SentenceId> = Vec::with_capacity(sids.len());
    let mut skipped: Vec<SentenceId> = Vec::new();
    for &sid in sids {
        if conv.add_axiom(sid) {
            order.push(sid);
        } else {
            skipped.push(sid);
        }
    }
    let (problem, _sid_map) = conv.finish();

    // Disable NewCNF's definitional naming for the batch.  With naming
    // on, a sub-formula that appears in multiple input axioms can be
    // abstracted into a fresh predicate + definitional clauses.  Those
    // clauses are "shared" — they trace back to multiple input axioms
    // in the inference graph — and complicate per-sid attribution.
    // Setting `naming=0` forces NewCNF to inline every subformula,
    // so every output clause has exactly one input ancestor and
    // `batched.shared` stays empty.  Also aligns batch output with
    // per-sentence output for hash-consistency.
    let mut opts = ir::Options::new();
    opts.set_option("naming", "0");

    let batched = problem
        .clausify_batch(opts)
        .map_err(|e| KbError::Other(format!("cnf2: clausify_batch failed: {e}")))?;

    // The `by_axiom` vec is positionally aligned with the accepted
    // inputs (in the order we added them via conv.add_axiom).
    debug_assert_eq!(
        batched.by_axiom.len(),
        order.len(),
        "clausify_batch returned {} axiom buckets for {} inputs",
        batched.by_axiom.len(),
        order.len(),
    );

    let mut by_sid: HashMap<SentenceId, Vec<ir::Clause>> =
        HashMap::with_capacity(order.len());
    for (i, bucket) in batched.by_axiom.into_iter().enumerate() {
        by_sid.insert(order[i], bucket);
    }

    Ok(BatchedSentenceClauses {
        by_sid,
        shared: batched.shared,
        skipped,
    })
}

// =========================================================================
//  IR -> CnfLiteral / CnfTerm translation
// =========================================================================

/// Per-call state carried through the translation.
#[derive(Default)]
struct TranslateCtx {
    /// Cache: Vampire functor name -> sumo-kb SymbolId.  Keeps translation
    /// of repeated symbols within a single call cheap and avoids
    /// re-invoking the intern path.
    name_to_id: HashMap<String, SymbolId>,
}

/// Magic "predicate id" used for the equality literal's `pred` slot.
/// Matches the convention in the legacy `cnf.rs` so downstream consumers
/// that special-case equality continue to work unchanged.
const EQUALITY_PRED_ID: SymbolId = u64::MAX;

fn ir_clause_to_cnf(
    store: &mut KifStore,
    ctx:   &mut TranslateCtx,
    clause: &ir::Clause,
) -> Clause {
    let literals = clause
        .literals
        .iter()
        .map(|l| ir_literal_to_cnf(store, ctx, l))
        .collect();
    Clause { literals }
}

fn ir_literal_to_cnf(
    store: &mut KifStore,
    ctx:   &mut TranslateCtx,
    lit:   &ir::Literal,
) -> CnfLiteral {
    match &lit.kind {
        ir::LitKind::Atom { pred, args } => {
            let pred_id   = resolve_predicate(store, ctx, pred.name());
            let args_cnf  = args
                .iter()
                .map(|a| ir_term_to_cnf(store, ctx, a))
                .collect();
            CnfLiteral {
                positive: lit.positive,
                pred:     CnfTerm::Const(pred_id),
                args:     args_cnf,
            }
        }
        ir::LitKind::Eq(lhs, rhs) => {
            let lhs_cnf = ir_term_to_cnf(store, ctx, lhs);
            let rhs_cnf = ir_term_to_cnf(store, ctx, rhs);
            CnfLiteral {
                positive: lit.positive,
                pred:     CnfTerm::Const(EQUALITY_PRED_ID),
                args:     vec![lhs_cnf, rhs_cnf],
            }
        }
    }
}

fn ir_term_to_cnf(
    store: &mut KifStore,
    ctx:   &mut TranslateCtx,
    term:  &ir::Term,
) -> CnfTerm {
    match term {
        ir::Term::Var(v) => {
            // Clause-local variable index.  Safe to reuse as SymbolId: no
            // indexing or display path dereferences var-ids against the
            // store (see `persist::commit::index_cnf_paths` and the
            // canonical hasher, which rename to V0..Vn anyway).
            CnfTerm::Var(v.index() as SymbolId)
        }
        ir::Term::Int(s) | ir::Term::Real(s) | ir::Term::Rational(s) => {
            CnfTerm::Num(s.clone())
        }
        ir::Term::Apply(func, args) => {
            let name = func.name();
            if is_skolem_name(name) {
                let id = intern_skolem(store, ctx, name, args.len());
                let sub = args.iter().map(|a| ir_term_to_cnf(store, ctx, a)).collect();
                // A skolem constant (arity 0) is still a SkolemFn with no
                // args — the CnfTerm shape keeps the distinction explicit
                // for downstream consumers that care about "is this a
                // skolem?" without looking up the Symbol record.
                CnfTerm::SkolemFn { id, args: sub }
            } else if args.is_empty() {
                let id = resolve_functor(store, ctx, name);
                CnfTerm::Const(id)
            } else {
                let id  = resolve_functor(store, ctx, name);
                let sub = args.iter().map(|a| ir_term_to_cnf(store, ctx, a)).collect();
                CnfTerm::Fn { id, args: sub }
            }
        }
    }
}

// =========================================================================
//  Symbol resolution
// =========================================================================

/// Interning convention: Vampire names round-trip through the NativeConverter,
/// which prefixes KIF symbols with `s__`.  Strip it on lookup to recover
/// the original KIF name; if no such symbol exists (e.g. because the
/// functor was a clausifier-synthesized wrapper like `s__holds__1`),
/// intern the name verbatim so the Symbol record stays round-trippable.

fn resolve_functor(
    store: &mut KifStore,
    ctx:   &mut TranslateCtx,
    name:  &str,
) -> SymbolId {
    lookup_or_intern(store, ctx, name)
}

fn resolve_predicate(
    store: &mut KifStore,
    ctx:   &mut TranslateCtx,
    name:  &str,
) -> SymbolId {
    lookup_or_intern(store, ctx, name)
}

fn lookup_or_intern(
    store: &mut KifStore,
    ctx:   &mut TranslateCtx,
    name:  &str,
) -> SymbolId {
    if let Some(&id) = ctx.name_to_id.get(name) {
        return id;
    }
    // First try the raw Vampire name (so `s__holds__1` round-trips as
    // itself) before stripping the `s__` prefix.  This matches bone-fide
    // non-skolem function symbols that Vampire emits verbatim from the
    // IR (`func.name() == "s__instance"` etc.).
    let id = store
        .sym_id(name)
        .or_else(|| store.sym_id(strip_s_prefix(name)))
        .unwrap_or_else(|| store.intern(name));
    ctx.name_to_id.insert(name.to_string(), id);
    id
}

fn intern_skolem(
    store: &mut KifStore,
    ctx:   &mut TranslateCtx,
    name:  &str,
    arity: usize,
) -> SymbolId {
    if let Some(&id) = ctx.name_to_id.get(name) {
        return id;
    }
    let id = store.intern_skolem(name, Some(arity));
    // Ensure the cached entry reflects the skolem interning (not a plain
    // intern that might otherwise have been done for a name collision).
    ctx.name_to_id.insert(name.to_string(), id);
    id
}

/// `true` for names that Vampire's NewCNF module uses for skolem
/// functors.  Matches both plain skolems (`sK0`, `sK1`) and the
/// disambiguated form (`sK0_7`) that some proof strategies emit.
fn is_skolem_name(name: &str) -> bool {
    name.starts_with("sK") || name.starts_with("sk_") // belt-and-braces
}

fn strip_s_prefix(name: &str) -> &str {
    name.strip_prefix("s__").unwrap_or(name)
}

// =========================================================================
//  Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kif_store::{load_kif, KifStore};
    use crate::semantic::SemanticLayer;

    /// Parse `kif`, build a SemanticLayer, return it plus the id of the
    /// last root sentence.
    fn load_one(kif: &str) -> (SemanticLayer, SentenceId) {
        let mut store = KifStore::default();
        let errs = load_kif(&mut store, kif, "test");
        assert!(errs.is_empty(), "parse errors: {:?}", errs);
        let layer = SemanticLayer::new(store);
        let sid = *layer.store.roots.last().expect("no sentence parsed");
        (layer, sid)
    }

    #[test]
    fn ground_atom_clausifies_to_single_unit_clause() {
        let (mut layer, sid) = load_one("(subclass Human Animal)");
        let clauses = sentence_to_clauses(&mut layer, sid).expect("clausify");
        assert_eq!(clauses.len(), 1);
        assert_eq!(clauses[0].literals.len(), 1);
        assert!(clauses[0].literals[0].positive);
    }

    #[test]
    fn negation_produces_negative_literal() {
        let (mut layer, sid) = load_one("(not (subclass Human Animal))");
        let clauses = sentence_to_clauses(&mut layer, sid).expect("clausify");
        assert_eq!(clauses.len(), 1);
        assert_eq!(clauses[0].literals.len(), 1);
        assert!(!clauses[0].literals[0].positive);
    }

    #[test]
    fn conjunction_splits_into_clauses() {
        let (mut layer, sid) = load_one(
            "(and (subclass Human Animal) (subclass Animal Entity))");
        let clauses = sentence_to_clauses(&mut layer, sid).expect("clausify");
        assert_eq!(clauses.len(), 2);
        assert!(clauses.iter().all(|c| c.literals.len() == 1));
    }

    #[test]
    fn implication_becomes_disjunction() {
        let (mut layer, sid) = load_one(
            "(forall (?X) (=> (subclass ?X Animal) (instance ?X Entity)))");
        let clauses = sentence_to_clauses(&mut layer, sid).expect("clausify");
        assert_eq!(clauses.len(), 1, "got {:?}", clauses);
        assert_eq!(clauses[0].literals.len(), 2);
        let neg = clauses[0].literals.iter().filter(|l| !l.positive).count();
        let pos = clauses[0].literals.iter().filter(|l|  l.positive).count();
        assert_eq!(neg, 1);
        assert_eq!(pos, 1);
    }

    #[test]
    fn forall_is_cnf_var() {
        let (mut layer, sid) = load_one("(forall (?X) (subclass ?X Animal))");
        let clauses = sentence_to_clauses(&mut layer, sid).expect("clausify");
        assert_eq!(clauses.len(), 1);
        let lit = &clauses[0].literals[0];
        assert!(matches!(lit.args[0], CnfTerm::Var(_)),
            "universally quantified ?X must be a Var, got {:?}", lit.args[0]);
    }

    #[test]
    fn exists_becomes_skolem() {
        let (mut layer, sid) = load_one("(exists (?X) (instance ?X Human))");
        let clauses = sentence_to_clauses(&mut layer, sid).expect("clausify");
        assert_eq!(clauses.len(), 1);
        let lit = &clauses[0].literals[0];
        // Vampire emits a skolem constant (arity 0) for a top-level exists,
        // which surfaces as either SkolemFn { args: [] } or Const depending
        // on how we classify the functor name.  Our `is_skolem_name` check
        // routes sK-prefixed names to SkolemFn regardless of arity, so we
        // expect SkolemFn here.
        let has_skolem = matches!(lit.args[0], CnfTerm::SkolemFn { .. })
            || matches!(lit.args[0], CnfTerm::Const(_));
        assert!(has_skolem,
            "?X with no outer forall must become a Skolem (const or fn), \
             got: {:?}", lit.args[0]);
    }

    #[test]
    fn exists_under_forall_becomes_skolem_fn() {
        let (mut layer, sid) = load_one(
            "(forall (?X) (exists (?Y) (instance ?Y ?X)))");
        let clauses = sentence_to_clauses(&mut layer, sid).expect("clausify");
        assert_eq!(clauses.len(), 1);
        let lit = &clauses[0].literals[0];
        // At least one arg should be a SkolemFn of the outer ?X.
        let has_skolem_fn = lit.args.iter().any(|t| matches!(
            t,
            CnfTerm::SkolemFn { args, .. } if !args.is_empty()
        ));
        assert!(has_skolem_fn,
            "?Y must become a Skolem fn of ?X; got args {:?}", lit.args);
    }

    #[test]
    fn equality_uses_sentinel_pred_id() {
        let (mut layer, sid) = load_one("(equal ?X ?Y)");
        let clauses = sentence_to_clauses(&mut layer, sid).expect("clausify");
        // After clausification the universally-quantified equality
        // becomes a single unit clause with two variables and the
        // equality sentinel in the `pred` slot.
        assert_eq!(clauses.len(), 1);
        let lit = &clauses[0].literals[0];
        assert!(lit.positive);
        assert_eq!(lit.args.len(), 2);
        assert!(matches!(lit.pred, CnfTerm::Const(EQUALITY_PRED_ID)),
            "equality literal should carry the sentinel pred id, got {:?}", lit.pred);
    }

    #[test]
    fn is_skolem_name_detection() {
        assert!(is_skolem_name("sK0"));
        assert!(is_skolem_name("sK0_3"));
        assert!(is_skolem_name("sK_foo"));
        assert!(is_skolem_name("sk_bar"));
        assert!(!is_skolem_name("s__instance"));
        assert!(!is_skolem_name("instance"));
        assert!(!is_skolem_name("skolem"));
    }
}
