// crates/core/src/semantics/errors.rs
//
// Error handling for the `SemanticLayer`

use std::{cell::RefCell, collections::HashSet, sync::{RwLock, atomic::{AtomicBool, Ordering}}};

use once_cell::sync::Lazy;
use thiserror::Error;

use crate::SentenceId;

/// Semantic errors -- non-fatal during KB construction, fatal during tell().
#[derive(Debug, Clone, Error)]
pub enum SemanticError {
    /// The given symbol does not have a taxonomical derivation to Entity
    #[error("symbol '{sym}' must have a valid derivation to Entity")]
    NoEntityAncestor { sym: String },

    /// The sentence starts with a symbol which is not a relation
    #[error("sentence head '{sym}' is not a declared relation")]
    HeadNotRelation { sid: SentenceId, sym: String },

    /// The sentence head is not a valid symbol
    #[error("sentence head is not a symbol")]
    HeadInvalid { sid: SentenceId },

    /// Operator passed symbolic value (and not a truth value relation or operator) as an argument
    #[error("argument {arg} of the operator, {op}, must be logical (predicate or operator) sentence")]
    NonLogicalArg { sid: SentenceId, arg: usize, op: String },

    /// A given symbol expected a certain arity but did not receive it
    #[error("arity mismatch for '{rel}': expected {expected}, got {got}")]
    ArityMismatch { sid: SentenceId, rel: String, expected: usize, got: usize },

    /// A given relation symbol expect an argument of a given type, but did not receive it
    #[error("domain mismatch for '{rel}' argument #{arg}: expected '{domain}'")]
    DomainMismatch { sid: SentenceId, rel: String, arg: usize, domain: String },

    /// There are multiple range declarations for a single symbol
    #[error("function '{sym}' has multiple range declarations")]
    DoubleRange { sym: String },

    /// A functional relation lacks a range
    #[error("function '{sym}' has no range declaration")]
    MissingRange { sym: String },

    /// A symbol is a relation but does not derive from a relation class which states an arity 
    ///  constraint
    #[error("relation '{sym}' is missing inheritance from a specific arity stating class (e.g. BinaryRelation)")]
    MissingArity { sym: String },

    /// A relation has a given arity constraint, but lacks a domain relation for one of its
    /// arguments
    #[error("symbol '{sym}' is missing a domain declaration for argument {idx}")]
    MissingDomain { sym: String, idx: usize },

    /// Functions should start with a capital
    #[error("function '{sym}' should start with an uppercase letter")]
    FunctionCase { sym: String },

    /// Predicates should start with a lowercase
    #[error("predicate '{sym}' should start with a lowercase letter")]
    PredicateCase { sym: String },

    /// Symbols cannot be both a class and an instance
    #[error("'{sym}' is declared as both an instance and a class (instance and subclass are disjoint)")]
    InstanceSubclassConflict { sym: String },

    /// A symbol belongs to disjoint classes
    #[error("'{sym}' is an instance of disjoint classes ({class1} and {class2})")]
    DisjointInstance { sid: Vec<SentenceId>, sym: String, class1: String, class2: String },

    /// A symbol cannot be derived from disjoint classes
    #[error("'{sym}' is a subclass of disjoint classes ({class1} and {class2})")]
    DisjointSubclass { sid: Vec<SentenceId>, sym: String, class1: String, class2: String },

    /// An and / or operator got only a single argument
    #[error("only one argument was passed to an conjunctive/disjunctive operator. Not technically incorrect, but meaningless")]
    SingleArity { sid: SentenceId },

    /// Other error. Use this sparingly
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
    /// `sigmakee-rs-core` is data-only here — the global `NO_WARNINGS` /
    /// `ALL_ERRORS` / `PROMOTED_TO_ERROR` flags only affect
    /// classification, never side effects.
    ///
    /// `store` is unused in the new design but retained on the
    /// signature so `semantic.rs`'s ~30 call sites don't churn.  A
    /// follow-up can drop the parameter.
    pub(crate) fn handle(&self, _store: &crate::syntactic::SyntacticLayer) -> Result<(), Self> {
        COLLECTOR.with(|c: &RefCell<Option<Vec<SemanticError>>>| {
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
            Self::DisjointInstance { .. }         => "E014",
            Self::DisjointSubclass { .. }         => "E015",
            Self::Other { .. }                    => "E016",
            Self::SingleArity { .. }              => "E017",
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
            Self::SingleArity { .. }              => "single-arity",
        }
    }

}

// `SemanticError::pretty_print` previously rendered ANSI output here
// directly via `log::log!`.  That path is now folded into the
// unified [`crate::Diagnostic`] pipeline: the renderer in
// `diagnostic.rs` reads `sids` / `highlight_arg` populated by
// `<SemanticError as ToDiagnostic>::to_diagnostic` and asks a
// [`crate::diagnostic::DiagnosticSource`] (typically the
// `KnowledgeBase`) for source-line context.

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
/// harvest semantic findings.  `sigmakee-rs-core` itself no longer prints
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