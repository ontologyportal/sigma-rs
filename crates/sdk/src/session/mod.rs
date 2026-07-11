//! The SDK's single entry point: a [`Session`] over a chosen [`Backend`].
//!
//! A `Session` owns one `KnowledgeBase` whose top layer is picked at
//! construction: the native saturation prover, an external subprocess prover
//! (Vampire/E or a custom [`ProverRunner`](sigmakee_rs_core::ProverRunner)), or
//! translation-only.  Every op dispatches on the layer; ops that don't apply to
//! the chosen backend (e.g. proving on [`Backend::TranslationOnly`]) return an
//! [`SdkError`](crate::SdkError) rather than silently no-op'ing.
//!
//! Ingestion accepts a local path or directory, an arbitrary reader (e.g.
//! stdin), or — behind the `http` / `git` features — a remote URL or
//! repository.  The parser is auto-detected from each file's name and content
//! ([`Parser::from_filename`](sigmakee_rs_core::Parser::from_filename) /
//! [`from_contents`](sigmakee_rs_core::Parser::from_contents)), and a parse
//! failure is a hard error.

mod ingest;
// Non-proving ops: validate / translate / load / open.
mod ops;
// Symbol introspection: `Session::manpage` + the `ManPageView` projection.
pub mod man;
// Proving ops (`ask`/`tell`/`audit`/`test`) currently take `NativeOpts`, so the
// module is native-prover-specific (the external arm errors as "unwired").
#[cfg(feature = "native-prover")]
mod ask;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(feature = "native-prover")]
pub use ask::{szs_status, ExpectedOutcome, OpenSession, SzsStatus, TestCaseOutcome, TestOutcome};

use sigmakee_rs_core::{DynSink, KnowledgeBase, TranslationLayer};
#[cfg(feature = "native-prover")]
use sigmakee_rs_core::ProverLayer;
#[cfg(feature = "ask")]
use sigmakee_rs_core::{ExternalProverLayer, Prover};
use sigmakee_rs_core::{TopLayer};

/// Which top layer (and prover) backs a [`Session`].
pub enum Backend {
    /// The native saturation prover (`KnowledgeBase<ProverLayer>`).
    #[cfg(feature = "native-prover")]
    Native,
    /// An external subprocess prover — built-in Vampire/E or, via the
    /// `Prover` selector, a custom [`ProverRunner`](sigmakee_rs_core::ProverRunner).
    #[cfg(feature = "ask")]
    External(Prover),
    /// Parsing / translation / validation only.  Proving ops error.
    TranslationOnly,
}

/// The unified, backend-agnostic entry point.  Construct with a [`Backend`],
/// then ingest / assert / prove / translate / validate against it.
pub struct Session<L: TopLayer> {
    kb: KnowledgeBase<L>,
    name: String,
}

#[cfg(feature = "native-prover")]
impl Session<ProverLayer> {
    /// Open a new session of a KB using the native prover backend
    pub fn new(session: String) -> Self {
        Self { kb: KnowledgeBase::new_native(), name: session }
    }
}

#[cfg(feature = "ask")]
impl Session<ExternalProverLayer> {
    /// Open a new session of a KB using the external prover backend
    pub fn new(session: String, prover: Prover) -> Self {
        Self { kb: KnowledgeBase::new_external(prover), name: session }
    }

    /// Override the prover backend on an external session — e.g. after
    /// [`from_kb`](Session::from_kb) on an opened DB, whose layer carries the
    /// default runner.
    pub fn set_runner(&mut self, prover: Prover) {
        self.kb.set_prover(prover);
    }
}

impl Session<TranslationLayer> {
    /// Open a new session of a KB without a prover backend
    pub fn new(session: String) -> Self {
        Self { kb: KnowledgeBase::new(), name: session }
    }
}

impl<L: TopLayer> Session<L> {
    /// Install a progress sink to the current session. See [`DynSink`]
    /// for more information on progress sinks
    pub fn set_progress_sink(&mut self, sink: DynSink) -> &Self {
        self.kb.set_progress_sink(sink);
        self
    }

    pub(crate) fn sink(&self)-> Option<DynSink> {
        self.kb.progress_sink().cloned()
    }

    /// Borrow the underlying [`KnowledgeBase`] — e.g. for a consumer that
    /// renders proofs / man pages against the same loaded KB the session proved
    /// against.
    pub fn kb(&self) -> &KnowledgeBase<L> {
        &self.kb
    }

    /// Borrow the underlying [`KnowledgeBase`] — e.g. for a consumer that
    /// renders proofs / man pages against the same loaded KB the session proved
    /// against.
    pub fn kb_mut(&mut self) -> &mut KnowledgeBase<L> {
        &mut self.kb
    }

    /// Wrap an already-constructed [`KnowledgeBase`] in a session — e.g. one
    /// returned by [`KnowledgeBase::open`](sigmakee_rs_core::KnowledgeBase) on a
    /// persistent DB.  The layer-specific `Session::new` constructors build an
    /// *empty* KB; this is the generic "I already have a loaded KB" entry point.
    pub fn from_kb(kb: KnowledgeBase<L>, name: Option<String>) -> Self {
        let session_name = name.unwrap_or_else(|| {
            Self::uuid()
        });
        Self { kb, name: session_name }
    }

    /// Fork an independent, in-memory copy of this session for isolated work —
    /// e.g. running one test without its ingested axioms leaking into the next.
    ///
    /// Snapshots the KB's caches into memory and thaws them into a detached
    /// clone (see [`KnowledgeBase::snapshot_clone`]).  The fork keeps this
    /// session's name, progress sink, and — for an external backend — its
    /// configured prover, but carries no DB handle, so ingesting / promoting /
    /// proving on it leaves *this* session untouched.  Requires `persist`.
    #[cfg(feature = "persist")]
    pub fn fork(&self) -> crate::SdkResult<Self> {
        let kb = self.kb.snapshot_clone().map_err(crate::SdkError::Kb)?;
        Ok(Self { kb, name: self.name.clone() })
    }

    fn uuid() -> String {
        let d = SystemTime::now().duration_since(UNIX_EPOCH);
        format!("{:x}", d.unwrap().as_nanos())
    }
}

#[cfg(all(test, feature = "native-prover"))]
mod tests {
    use super::*;
    use super::super::Source;
    use sigmakee_rs_core::NativeOpts;

    static SESSION: &str = "test-session";

    fn reader(name: &str, kif: &str) -> Source {
        Source::Reader { name: name.into(), reader: Box::new(std::io::Cursor::new(Vec::from(kif))) }
    }
    fn fast() -> NativeOpts { NativeOpts { time_limit_secs: 10, ..Default::default() } }

    #[test]
    fn native_session_ingests_then_proves() {
        let mut s = Session::<ProverLayer>::new(SESSION.to_string());
        s.ingest(reader("t.kif",
            "(subclass Dog Mammal) (subclass Mammal Animal) (instance Rex Dog)"), true);
        let r = s.ask("(instance Rex Animal)", Some(fast())).unwrap();
        assert_eq!(r.status, sigmakee_rs_core::ProverStatus::Proved, "{}", r.raw_output);
    }

    #[test]
    fn doxastic_ask_closes_belief_context_and_stays_read_only() {
        let mut s = Session::<ProverLayer>::new(SESSION.to_string());
        s.ingest(reader("dox.kif",
            "(domain believes 2 Formula)\n\
             (believes John (p a))\n\
             (believes John (=> (p a) (q a)))"), true);
        // Full closure inside the context: modus ponens over the beliefs.
        let r = s.doxastic_ask("John", "(q a)", Some(fast())).unwrap();
        assert_eq!(r.status, sigmakee_rs_core::ProverStatus::Proved, "{}", r.raw_output);
        // Outer control: `(believes John (q a))` is NOT derivable outside
        // the projection — inner conclusions are never fed back.
        let outer = s.ask("(believes John (q a))", Some(fast())).unwrap();
        assert_ne!(outer.status, sigmakee_rs_core::ProverStatus::Proved,
            "guardrail: outer ask must stay unproven: {}", outer.raw_output);
        // Consistency surface.
        let c = s.doxastic_consistent("John", Some(fast())).unwrap();
        assert_eq!(c.status, sigmakee_rs_core::ProverStatus::Consistent, "{}", c.raw_output);
    }

    #[test]
    fn tell_accumulates_hypotheses_then_ask() {
        // No base axioms — the chained hypotheses alone must discharge the goal.
        let mut s = Session::<ProverLayer>::new(SESSION.to_string());
        let r = s.tell("(=> (p ?x) (q ?x))").unwrap()
                 .tell("(p a)").unwrap()
                 .ask("(q a)", Some(fast())).unwrap();
        assert_eq!(r.status, sigmakee_rs_core::ProverStatus::Proved, "{}", r.raw_output);
    }

    #[test]
    fn test_runs_a_tq_file() {
        let dir = std::env::temp_dir().join("sdk_session_tq");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.kif.tq");
        std::fs::write(&p, "(subclass A B) (instance x A) (query (instance x B))").unwrap();
        let mut s = Session::<ProverLayer>::new(SESSION.to_string());
        let r = s.test(Source::Local(vec![p]), None).unwrap();
        assert_eq!(r.outcome, TestOutcome::Passed, "{}", r.result.raw_output);
    }

    #[test]
    fn test_solves_tptp_problem_with_cross_file_include() {
        // A `.p` problem whose axioms live in a sibling `.ax`, pulled in by an
        // `include(...)` directive — exercises the cross-file handler end to end:
        // `Source::read` splices the include, `test` routes the TPTP problem
        // through `tptp::solve`, and the conjecture is proved from the included
        // theory.
        let dir = std::env::temp_dir().join("sdk_session_tptp_include");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("zoo.ax"), "\
fof(a1, axiom, ![X] : (dog(X) => mammal(X))).\n\
fof(a2, axiom, ![X] : (mammal(X) => animal(X))).\n\
fof(a3, axiom, dog(rex)).\n").unwrap();
        let prob = dir.join("zoo.p");
        std::fs::write(&prob, "\
include('zoo.ax').\n\
fof(goal, conjecture, animal(rex)).\n").unwrap();

        let mut s = Session::<ProverLayer>::new(SESSION.to_string());
        let r = s.test(Source::Local(vec![prob]), None).unwrap();
        assert_eq!(r.outcome, TestOutcome::Passed, "{}", r.result.raw_output);
    }

    // End-to-end of the `run_test` machinery: fork the master, run a
    // self-contained TPTP problem on the fork (its `axiom`-role statements are
    // background → ingested + bulk-promoted → the conjecture proves), and
    // confirm none of that leaked back into the master.
    #[test]
    #[cfg(feature = "persist")] // `fork` rides on the persist-gated snapshot/restore
    fn fork_runs_a_tptp_test_in_isolation() {
        let master = Session::<ProverLayer>::new("master".to_string());

        let mut fork = master.fork().expect("fork the master");
        let problem = "\
fof(a1, axiom, ![X] : (dog(X) => mammal(X))).\n\
fof(a2, axiom, ![X] : (mammal(X) => animal(X))).\n\
fof(a3, axiom, dog(rex)).\n\
fof(goal, conjecture, animal(rex)).\n";
        let r = fork.test(reader("zoo.p", problem), Some(fast())).expect("test ran");
        assert_eq!(r.outcome, TestOutcome::Passed,
            "fork proves animal(rex) from its promoted background: {}", r.result.raw_output);

        // Isolation: the fork's promoted axioms never reached the master.
        let m = master.ask("(animal rex)", Some(fast())).expect("ask ran");
        assert_ne!(m.status, sigmakee_rs_core::ProverStatus::Proved,
            "master must not see the fork's axioms; got {:?}", m.status);
    }

    #[test]
    fn parser_autodetect_picks_tptp_by_extension() {
        // A `.p` source routes through the TPTP parser (KIF would reject `fof`).
        let mut s = Session::<ProverLayer>::new(SESSION.to_string());
        s.ingest(reader("p.p", "fof(a, axiom, mammal(rex))."), true);
    }


    #[test]
    fn undetectable_source_is_an_error() {
        let mut s = Session::<ProverLayer>::new(SESSION.to_string());
        // No extension, no `(`/`fof(` head → detection fails → hard error.
        assert!(s.ingest(reader("noext", "just some prose"), true).iter().any(|e| e.is_err()));
    }

    #[test]
    fn check_consistency_passes_a_clean_kb() {
        let mut s = Session::<ProverLayer>::new(SESSION.to_string());
        s.ingest(reader("c.kif", "(instance Rex Dog)\n(=> (instance ?X Dog) (barks ?X))"), true);
        let r = s.check_consistency(fast()).unwrap();
        assert_eq!(r.status, sigmakee_rs_core::ProverStatus::Consistent, "{}", r.raw_output);
    }

    #[test]
    fn check_consistency_flags_a_contradiction() {
        let mut s = Session::<ProverLayer>::new(SESSION.to_string());
        s.ingest(reader("c.kif", "(barks Rex)\n(not (barks Rex))"), true);
        let r = s.check_consistency(fast()).unwrap();
        assert_eq!(r.status, sigmakee_rs_core::ProverStatus::Inconsistent, "{}", r.raw_output);
    }

    #[test]
    fn audit_enumerates_contradictions() {
        let mut s = Session::<ProverLayer>::new(SESSION.to_string());
        s.ingest(reader("c.kif", "(barks Rex)\n(not (barks Rex))"), true);
        let r = s.audit(fast(), 4).unwrap();
        assert_eq!(r.status, sigmakee_rs_core::ProverStatus::Inconsistent, "{}", r.raw_output);
    }

    #[test]
    fn validate_formula_on_native_session_flags_parse_error() {
        let mut s = Session::<ProverLayer>::new(SESSION.to_string());
        let diags = s.validate_formula("(broken (").unwrap();
        assert!(!diags.is_empty());
    }

    #[test]
    fn tell_then_ask_is_disproved_without_the_hypothesis() {
        // Guards the session-scoping fix: the goal must NOT prove when the
        // enabling hypothesis was never told.
        let mut s = Session::<ProverLayer>::new(SESSION.to_string());
        let r = s.tell("(p a)").unwrap()
                 .ask("(q a)", Some(fast())).unwrap();
        assert_ne!(r.status, sigmakee_rs_core::ProverStatus::Proved, "{}", r.raw_output);
    }
}
