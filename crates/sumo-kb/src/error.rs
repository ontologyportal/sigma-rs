// crates/sumo-kb/src/error.rs
//
// All error and result types for sumo-kb.
// Ports sumo-parser-core/src/error.rs and adds the new types needed by the
// unified API (KbError, TellResult, PromoteError, etc.).

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

use once_cell::sync::Lazy;
use thiserror::Error;
use inline_colorization::*;

use crate::types::SentenceId;

// ── Global warning-control flags ─────────────────────────────────────────────

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

pub fn suppress_warnings(whether: bool) {
    NO_WARNINGS.store(whether, Ordering::SeqCst);
}

// ── Span and ParseError ───────────────────────────────────────────────────────
// Defined in parse::kif; re-exported here for backward compatibility.
pub use crate::parse::kif::{Span, ParseError};

// ── SemanticError ─────────────────────────────────────────────────────────────

/// Semantic errors — non-fatal during KB construction, fatal during tell().
#[derive(Debug, Clone, Error)]
pub enum SemanticError {
    #[error("symbol '{sym}' must have a valid derivation to Entity")]
    NoEntityAncestor { sym: String },

    #[error("sentence head '{sym}' is not a declared relation")]
    HeadNotRelation { sid: SentenceId, sym: String },

    #[error("sentence head is not a symbol")]
    HeadInvalid { sid: SentenceId },

    #[error("argument {arg} of operator sentence must be logical (predicate or operator) sentence")]
    NonLogicalArg { sid: SentenceId, arg: usize },

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

    /// Dispatch: log as warning or return as error, depending on severity config.
    /// `store` is used for pretty-printing the offending sentence.
    pub(crate) fn handle(&self, store: &crate::kif_store::KifStore) -> Result<(), Self> {
        if self.is_warn() {
            if !NO_WARNINGS.load(Ordering::SeqCst) {
                log::warn!(target: "sumo_kb::semantic", "semantic warning ({}) {}", self.code(), self);
                self.pretty_print(store, log::Level::Warn);
            }
            Ok(())
        } else {
            log::error!(target: "sumo_kb::semantic", "semantic error [{}]: {}", self.code(), self);
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

// ── KbError ───────────────────────────────────────────────────────────────────

/// Top-level error type for all sumo-kb operations.
#[derive(Debug, Error)]
pub enum KbError {
    #[error(transparent)]
    Parse(#[from] ParseError),

    #[error(transparent)]
    Semantic(#[from] SemanticError),

    #[cfg(feature = "persist")]
    #[error("database error: {0}")]
    Db(String),

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

// ── tell() result types ───────────────────────────────────────────────────────

/// Result returned by `KnowledgeBase::tell()` and `load_kif()`.
#[derive(Debug, Default)]
pub struct TellResult {
    /// True if the call succeeded (parse + semantic checks passed).
    /// Duplicate-skipped formulas do NOT make this false.
    pub ok: bool,
    /// Hard errors (parse failures, fatal semantic errors).
    pub errors: Vec<String>,
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

// ── promote_assertions() result types ────────────────────────────────────────

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
