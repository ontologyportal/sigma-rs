// crates/sumo-kb/src/error.rs
//
// All error and result types for sumo-kb.
// Ports sumo-parser-core/src/error.rs and adds the new types needed by the
// unified API (KbError, TellResult, PromoteError, etc.).

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

use once_cell::sync::Lazy;
use thiserror::Error;
use inline_colorization::*;

use crate::types::SentenceId;

// -- Thread-local diagnostic collector ---------------------------------------
//
// The LSP needs every `SemanticError` the validator raises -- hard
// errors *and* warnings -- so it can turn each into an LSP
// diagnostic.  The existing `SemanticError::handle()` swallows
// warnings (`Ok(())`) and hard errors short-circuit the `?`
// propagation in `validate_sentence` / `validate_element`.  That
// means `validate_sentence` at best returns the *first* hard
// error it hits and at worst returns nothing.
//
// To keep `validate_sentence`'s control flow untouched (it's used
// by `tell()` and the CLI with the existing severity semantics)
// we install a thread-local collector.  When set, `handle()`
// pushes into the collector and returns `Ok(())` regardless of
// severity -- the caller's `?` chain therefore never short-
// circuits, and every check runs.  The LSP drains the collector
// at the end of the validation call.

thread_local! {
    static COLLECTOR: RefCell<Option<Vec<SemanticError>>> = const { RefCell::new(None) };
}

/// Run `f` with a diagnostic collector installed on the current
/// thread.  Every `SemanticError::handle()` call inside `f` pushes
/// its error into the collector.  Hard errors still short-circuit
/// `f`'s `?` chain (so `validate_sentence` returns at the first
/// fatal finding); warnings are pushed and execution continues.
/// Returns the collected findings alongside whatever `f` returned.
///
/// Re-entrant-safe via a drop guard — if `f` panics or a nested
/// `with_collector` is installed, the thread-local is restored to
/// its previous value on unwind.
///
/// This is the sole supported way for SDK / CLI / LSP consumers to
/// harvest semantic findings.  `sumo-kb` itself no longer prints
/// warnings or hard errors — `handle()` is now a pure routing
/// function (push-to-collector + Result discrimination).  Consumers
/// render via [`crate::KnowledgeBase::pretty_print_error`] or by
/// building their own presentation on top of [`SemanticError::code`]
/// / [`SemanticError::is_warn`].
pub fn with_collector<F, R>(f: F) -> (R, Vec<SemanticError>)
where
    F: FnOnce() -> R,
{
    struct Guard(Option<Vec<SemanticError>>);
    impl Drop for Guard {
        fn drop(&mut self) {
            // Restore the previous collector (usually `None`).  Even
            // when that previous value was `Some`, this preserves any
            // errors we collected on behalf of an outer scope.
            let prev = self.0.take();
            COLLECTOR.with(|c| *c.borrow_mut() = prev);
        }
    }

    let prev = COLLECTOR.with(|c| c.borrow_mut().replace(Vec::new()));
    let guard = Guard(prev);
    let result = f();
    let collected = COLLECTOR
        .with(|c| c.borrow_mut().take())
        .unwrap_or_default();
    drop(guard);
    (result, collected)
}

// -- Global warning-control flags ---------------------------------------------

/// Treat all ignorable semantic errors as fatal (mimics -Wall).
static ALL_ERRORS: AtomicBool = AtomicBool::new(false);

/// Suppress all warnings from being logged.
static NO_WARNINGS: AtomicBool = AtomicBool::new(false);

/// Specific error codes or names promoted to errors (mimics -Werror=<code>).
static PROMOTED_TO_ERROR: Lazy<RwLock<HashSet<String>>> =
    Lazy::new(|| RwLock::new(HashSet::new()));

pub fn set_all_errors(val: bool) {
    ALL_ERRORS.store(val, Ordering::SeqCst);
}

pub fn promote_to_error(code_or_name: &str) {
    if let Ok(mut set) = PROMOTED_TO_ERROR.write() {
        set.insert(code_or_name.to_string());
    }
}

/// Clear the promoted-error set installed by [`promote_to_error`].
///
/// Useful for tests that mutate the global classification table and
/// need to leave it pristine for subsequent tests, and for long-
/// running consumers (LSP, daemons) that want to apply a fresh
/// promotion list when the user's config changes.
pub fn clear_promoted_errors() {
    if let Ok(mut set) = PROMOTED_TO_ERROR.write() {
        set.clear();
    }
}

pub fn suppress_warnings(whether: bool) {
    NO_WARNINGS.store(whether, Ordering::SeqCst);
}

/// Query whether warnings are currently being suppressed (set via [`suppress_warnings`]).
///
/// The CLI flips this on for `-q` / `--quiet`. Call sites that emit non-semantic
/// warnings (e.g. `log::warn!` for duplicate axioms) should gate on this so
/// `-q` silences them in addition to semantic warnings.
pub fn warnings_suppressed() -> bool {
    NO_WARNINGS.load(Ordering::SeqCst)
}

// -- Span and ParseError -------------------------------------------------------
// Defined in parse::kif; re-exported here for backward compatibility.
pub use crate::parse::ParseError;
pub use crate::parse::ast::Span;

// -- SemanticError -------------------------------------------------------------

/// Semantic errors -- non-fatal during KB construction, fatal during tell().
#[derive(Debug, Clone, Error)]
pub enum SemanticError {
    #[error("symbol '{sym}' must have a valid derivation to Entity")]
    NoEntityAncestor { sym: String },

    #[error("sentence head '{sym}' is not a declared relation")]
    HeadNotRelation { sid: SentenceId, sym: String },

    #[error("sentence head is not a symbol")]
    HeadInvalid { sid: SentenceId },

    #[error("argument {arg} of the operator, {op}, must be logical (predicate or operator) sentence")]
    NonLogicalArg { sid: SentenceId, arg: usize, op: String },

    #[error("arity mismatch for '{rel}': expected {expected}, got {got}")]
    ArityMismatch { sid: SentenceId, rel: String, expected: usize, got: usize },

    #[error("domain mismatch for '{rel}' argument #{arg}: expected '{domain}'")]
    DomainMismatch { sid: SentenceId, rel: String, arg: usize, domain: String },

    #[error("function '{sym}' has multiple range declarations")]
    DoubleRange { sym: String },

    #[error("function '{sym}' has no range declaration")]
    MissingRange { sym: String },

    #[error("relation '{sym}' is missing inheritance from a specific arity stating class (e.g. BinaryRelation)")]
    MissingArity { sym: String },

    #[error("symbol '{sym}' is missing a domain declaration for argument {idx}")]
    MissingDomain { sym: String, idx: usize },

    #[error("function '{sym}' should start with an uppercase letter")]
    FunctionCase { sym: String },

    #[error("predicate '{sym}' should start with a lowercase letter")]
    PredicateCase { sym: String },

    #[error("'{sym}' is declared as both an instance and a class (instance and subclass are disjoint)")]
    InstanceSubclassConflict { sym: String },

    #[error("'{sym}' is an instance of disjoint classes ({class1} and {class2})")]
    DisjointInstance { sid: Vec<SentenceId>, sym: String, class1: String, class2: String },

    #[error("'{sym}' is a subclass of disjoint classes ({class1} and {class2})")]
    DisjointSubclass { sid: Vec<SentenceId>, sym: String, class1: String, class2: String },

    #[error("{msg}")]
    Other { msg: String },
}

impl SemanticError {
    pub fn current_level(&self) -> log::Level {
        if ALL_ERRORS.load(Ordering::SeqCst) {
            return log::Level::Error;
        }
        let promoted = PROMOTED_TO_ERROR.read().expect("lock poisoned");
        if promoted.contains(self.code()) || promoted.contains(self.name()) {
            log::Level::Error
        } else {
            log::Level::Warn
        }
    }

    pub fn is_warn(&self) -> bool {
        self.current_level() == log::Level::Warn
    }

    /// Dispatch: classify the finding and route it to the caller.
    ///
    /// - If a thread-local collector is installed (see
    ///   [`with_collector`]), the finding is pushed into it.
    /// - If the finding is a warning (per [`Self::is_warn`]), returns
    ///   `Ok(())` so the caller's `?` chain continues — every check
    ///   in `validate_sentence` / `validate_element` runs to
    ///   completion.
    /// - If the finding is a hard error, returns `Err(self.clone())`
    ///   so the caller's `?` chain short-circuits.
    ///
    /// **No printing happens here.**  Presentation is the
    /// consumer's job: the CLI's `semantic_warning!` / `semantic_error!`
    /// macros call [`crate::KnowledgeBase::pretty_print_error`]; the
    /// LSP turns each finding into an LSP diagnostic; the SDK
    /// surfaces them via [`crate::Findings`] on its `ValidationReport`.
    /// `sumo-kb` is data-only here — the global `NO_WARNINGS` /
    /// `ALL_ERRORS` / `PROMOTED_TO_ERROR` flags only affect
    /// classification, never side effects.
    ///
    /// `store` is unused in the new design but retained on the
    /// signature so `semantic.rs`'s ~30 call sites don't churn.  A
    /// follow-up can drop the parameter.
    pub(crate) fn handle(&self, _store: &crate::kif_store::KifStore) -> Result<(), Self> {
        COLLECTOR.with(|c| {
            if let Some(vec) = c.borrow_mut().as_mut() {
                vec.push(self.clone());
            }
        });
        if self.is_warn() {
            Ok(())
        } else {
            Err(self.clone())
        }
    }

    /// Short alphanumeric code for use with `-W` / `--warning`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoEntityAncestor { .. }         => "E001",
            Self::HeadNotRelation { .. }          => "E002",
            Self::HeadInvalid { .. }              => "E003",
            Self::NonLogicalArg { .. }            => "E004",
            Self::ArityMismatch { .. }            => "E005",
            Self::DomainMismatch { .. }           => "E006",
            Self::DoubleRange { .. }              => "E007",
            Self::MissingRange { .. }             => "E008",
            Self::MissingArity { .. }             => "W009",
            Self::MissingDomain { .. }            => "E010",
            Self::FunctionCase { .. }             => "W011",
            Self::PredicateCase { .. }            => "W012",
            Self::InstanceSubclassConflict { .. } => "E013",
            Self::DisjointInstance { .. }         => "disjoint-instance",
            Self::DisjointSubclass { .. }         => "disjoint-subclass",
            Self::Other { .. }                    => "E015",
        }
    }

    /// Kebab-case name for use with `--warning=<name>`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::NoEntityAncestor { .. }         => "no-entity-ancestor",
            Self::HeadNotRelation { .. }          => "head-not-relation",
            Self::HeadInvalid { .. }              => "head-invalid",
            Self::NonLogicalArg { .. }            => "non-logical-arg",
            Self::ArityMismatch { .. }            => "arity-mismatch",
            Self::DomainMismatch { .. }           => "domain-mismatch",
            Self::DoubleRange { .. }              => "double-range",
            Self::MissingRange { .. }             => "missing-range",
            Self::MissingArity { .. }             => "missing-arity",
            Self::MissingDomain { .. }            => "missing-domain",
            Self::FunctionCase { .. }             => "function-case",
            Self::PredicateCase { .. }            => "predicate-case",
            Self::InstanceSubclassConflict { .. } => "instance-subclass-conflict",
            Self::DisjointInstance { .. }         => "disjoint-instance",
            Self::DisjointSubclass { .. }         => "disjoint-subclass",
            Self::Other { .. }                    => "other",
        }
    }

    /// Pretty-print the error with source context using colorized output.
    pub(crate) fn pretty_print(&self, store: &crate::kif_store::KifStore, level: log::Level) {
        use crate::kif_store::SentenceDisplay;
        let color = match level {
            log::Level::Error => color_red,
            log::Level::Warn  => color_yellow,
            log::Level::Info  => color_cyan,
            log::Level::Debug => color_blue,
            log::Level::Trace => color_white,
        };
        match self {
            SemanticError::NoEntityAncestor { .. }
            | SemanticError::DoubleRange { .. }
            | SemanticError::MissingRange { .. }
            | SemanticError::MissingArity { .. }
            | SemanticError::MissingDomain { .. }
            | SemanticError::FunctionCase { .. }
            | SemanticError::PredicateCase { .. }
            | SemanticError::InstanceSubclassConflict { .. }
            | SemanticError::Other { .. } => {
                log::log!(target: "clean", level, "{}\t{}{color_reset}", color, self);
            }
            SemanticError::HeadNotRelation { sid, .. }
            | SemanticError::HeadInvalid { sid, .. } => {
                let dis = SentenceDisplay { sid: *sid, store, indent: 0, show_gutter: true, highlight_arg: 0 };
                log::log!(target: "clean", level, "{}\n\n{}\t{}{color_reset}", dis, color, self);
            }
            SemanticError::NonLogicalArg { sid, arg, .. }
            | SemanticError::DomainMismatch { sid, arg, .. } => {
                let dis = SentenceDisplay { sid: *sid, store, indent: 0, show_gutter: true, highlight_arg: *arg as i32 };
                log::log!(target: "clean", level, "{}\n\n{}\t{}{color_reset}", dis, color, self);
            }
            SemanticError::ArityMismatch { sid, .. } => {
                let dis = SentenceDisplay { sid: *sid, store, indent: 0, show_gutter: true, highlight_arg: -1 };
                log::log!(target: "clean", level, "{}\n\n{}\t{}{color_reset}", dis, color, self);
            }
            SemanticError::DisjointInstance { sid, .. }
            | SemanticError::DisjointSubclass { sid, .. } => {
                for s in sid {
                    let dis = SentenceDisplay { sid: *s, store, indent: 0, show_gutter: true, highlight_arg: -1 };
                    log::log!(target: "clean", level, "{}", dis);
                }
                log::log!(target: "clean", level, "\n{}\t{}{color_reset}", color, self);
            }
        }
    }
}

// -- Findings ----------------------------------------------------------------

/// A semantic-validation pass's findings, classified by severity.
///
/// Returned from the `_findings` family of validation methods on
/// [`crate::KnowledgeBase`] and from [`crate::Findings`]-bearing
/// SDK reports.  Each entry is paired with the [`SentenceId`] the
/// finding fired against so consumers can map back to source spans
/// via `kb.sentence(sid)`.
///
/// Classification follows [`SemanticError::is_warn`], which honours
/// the `-Wall` / `-W <code>` / `-q` global flags.  Switching a flag
/// between two passes will reclassify identical inputs accordingly.
#[derive(Debug, Clone, Default)]
pub struct Findings {
    /// Hard errors (`is_warn() == false`).  Loading commands like
    /// `sumo load` should treat a non-empty `errors` list as an
    /// abort condition.
    pub errors:   Vec<(SentenceId, SemanticError)>,
    /// Warnings (`is_warn() == true`).  Advisory; consumers may
    /// render them, suppress them (via `-q` / `warnings_suppressed`),
    /// or ignore them entirely.
    pub warnings: Vec<(SentenceId, SemanticError)>,
}

impl Findings {
    /// `true` iff no hard errors were reported.  Warnings are
    /// advisories and don't unset cleanliness — match on
    /// `warnings.is_empty()` separately if you need a stricter
    /// "perfectly clean" check.
    pub fn is_clean(&self) -> bool {
        self.errors.is_empty()
    }

    /// Total finding count (errors + warnings).
    pub fn total(&self) -> usize {
        self.errors.len() + self.warnings.len()
    }

    /// Push a finding, classifying by [`SemanticError::is_warn`].
    /// Useful when collecting findings outside the validate_*
    /// methods (e.g. inside SDK ops).
    pub fn push(&mut self, sid: SentenceId, error: SemanticError) {
        if error.is_warn() {
            self.warnings.push((sid, error));
        } else {
            self.errors.push((sid, error));
        }
    }
}

// -- KbError -------------------------------------------------------------------

/// Top-level error type for all sumo-kb operations.
#[derive(Debug, Error)]
pub enum KbError {
    #[error(transparent)]
    Parse(#[from] Box<dyn ParseError>),

    #[error(transparent)]
    Semantic(#[from] SemanticError),

    #[cfg(feature = "persist")]
    #[error("database error: {0}")]
    Db(String),

    /// The on-disk LMDB schema was created by an older build of
    /// `sumo-kb` and is not compatible with the current one.  There is
    /// no auto-migration pre-1.0 — the caller must delete the DB and
    /// re-import, or downgrade to a compatible build.  The `String`
    /// gives a short human-readable description of what was detected.
    #[cfg(feature = "persist")]
    #[error("schema migration required: {0}")]
    SchemaMigrationRequired(String),

    #[cfg(feature = "ask")]
    #[error("prover error: {0}")]
    Prover(String),

    #[error("{0}")]
    Other(String),
}

#[cfg(feature = "persist")]
impl From<heed::Error> for KbError {
    fn from(e: heed::Error) -> Self {
        KbError::Db(e.to_string())
    }
}

// -- tell() result types -------------------------------------------------------

/// Result returned by `KnowledgeBase::tell()` and `load_kif()`.
#[derive(Debug, Default)]
pub struct TellResult {
    /// True if the call succeeded (parse + semantic checks passed).
    /// Duplicate-skipped formulas do NOT make this false.
    pub ok: bool,
    /// Hard errors (parse failures, fatal semantic errors).
    pub errors: Vec<KbError>,
    /// Non-fatal notices (semantic warnings, duplicates skipped).
    pub warnings: Vec<TellWarning>,
}

#[derive(Debug)]
pub enum TellWarning {
    /// Formula already present as an axiom in the DB.
    DuplicateAxiom {
        existing_id: SentenceId,
        /// Short human-readable preview of the formula (first ~60 chars).
        formula_preview: String,
    },
    /// Formula already present as an assertion in a session.
    DuplicateAssertion {
        existing_id: SentenceId,
        existing_session: String,
        formula_preview: String,
    },
    /// Non-fatal semantic issue (arity warning, case convention, etc.).
    Semantic(SemanticError),
}

impl std::fmt::Display for TellWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TellWarning::DuplicateAxiom { formula_preview, .. } =>
                write!(f, "duplicate axiom (skipped): {}", formula_preview),
            TellWarning::DuplicateAssertion { existing_session, formula_preview, .. } =>
                write!(f, "duplicate of assertion in session '{}' (skipped): {}",
                    existing_session, formula_preview),
            TellWarning::Semantic(e) =>
                write!(f, "semantic warning [{}]: {}", e.code(), e),
        }
    }
}

// -- promote_assertions() result types ----------------------------------------

/// Successful result from `KnowledgeBase::promote_assertions*()`.
#[derive(Debug, Default)]
pub struct PromoteReport {
    /// SentenceIds successfully promoted to axioms.
    pub promoted: Vec<SentenceId>,
    /// Formulas removed from the session as duplicates before promotion.
    pub duplicates_removed: Vec<DuplicateInfo>,
}

#[derive(Debug)]
pub struct DuplicateInfo {
    pub sentence_id: SentenceId,
    pub duplicate_of: SentenceId,
    pub source: DuplicateSource,
    /// Short human-readable preview of the formula.
    pub formula_preview: String,
}

#[derive(Debug)]
pub enum DuplicateSource {
    /// Duplicate of an existing axiom in the DB.
    Axiom,
    /// Duplicate of an assertion in another in-memory session.
    Session(String),
}

/// Error returned by `KnowledgeBase::promote_assertions()`.
#[derive(Debug, Error)]
pub enum PromoteError {
    /// The prover showed the session assertions make the KB inconsistent.
    #[error("promotion rejected: session '{session}' makes the KB inconsistent")]
    Inconsistent {
        session: String,
        /// Raw prover output explaining the inconsistency.
        explanation: String,
        /// Assertion SentenceIds implicated (best-effort extraction).
        conflicting: Vec<SentenceId>,
    },

    /// The prover could not determine consistency (timeout or unknown result).
    /// Promotion is conservatively rejected.
    #[error("promotion rejected: prover could not determine consistency ({reason})")]
    ProverUncertain { reason: String },

    /// Hard semantic errors in the session prevented promotion.
    #[error("promotion rejected: {count} semantic error(s) in session")]
    Semantic {
        count: usize,
        errors: Vec<(SentenceId, SemanticError)>,
    },

    #[cfg(feature = "persist")]
    #[error("database write failed: {0}")]
    Db(KbError),
}
