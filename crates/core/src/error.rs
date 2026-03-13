use std::sync::atomic::Ordering;
use std::{collections::HashSet, sync::atomic::AtomicBool};
use std::sync::RwLock;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use log;
use inline_colorization::*;

use crate::{KifStore, SentenceDisplay, store::SentenceId};

/// If true, all ignorable errors are treated as fatal (mimics -Wall)
static ALL_ERRORS: AtomicBool = AtomicBool::new(false);

/// Specific codes promoted to Errors (mimics -Werror=code)
static PROMOTED_TO_ERROR: Lazy<RwLock<HashSet<String>>> = Lazy::new(|| {
    RwLock::new(HashSet::new())
});

pub fn set_all_errors(val: bool) {
    ALL_ERRORS.store(val, Ordering::SeqCst);
}

pub fn promote_to_error(code_or_name: &str) {
    if let Ok(mut set) = PROMOTED_TO_ERROR.write() {
        set.insert(code_or_name.to_string());
    }
}

/// Location in source text (1-based line and column).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Span {
    pub file: String,
    pub line: u32,
    pub col:  u32,
    pub offset: usize
}

impl std::fmt::Display for Span {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}:{}", self.file, self.line, self.col)
    }
}

/// Hard tokenizer / parser / syntax errors that prevent acceptance.
#[derive(Debug, Clone, Error)]
pub enum ParseError {
    #[error("unterminated string literal")]
    UnterminatedString { span: Span },

    #[error("unexpected character '{ch}'")]
    UnexpectedChar { ch: char, span: Span },

    #[error("sentence with no terms encountered")]
    EmptySentence { span: Span },

    #[error("unexpected end of input")]
    UnexpectedEof { span: Span },

    #[error("unbalanced parentheses")]
    UnbalancedParens { span: Span },

    #[error("operator '{op}' outside first-term position")]
    OperatorOutOfPosition { op: String, span: Span },

    #[error("quantifier operators' first argument must be a sentence comprised only of variables")]
    QuantiferArg { span: Span },

    #[error("the first term of a sentence must be an operator, symbol, or non-row variable")]
    FirstTerm { span: Span },

    #[error("{msg}")]
    Syntax { msg: String, span: Span },

    #[error("{msg}")]
    Other { msg: String },

}

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
    NonLogicalArg { sid : SentenceId, arg: usize },

    #[error("arity mismatch for '{rel}': expected {expected}, got {got}")]
    ArityMismatch { sid : SentenceId, rel: String, expected: usize, got: usize },

    #[error("domain mismatch for '{rel}' argument #{arg}: expected '{domain}'")]
    DomainMismatch { sid : SentenceId, rel: String, arg: usize, domain: String },

    #[error("function '{sym}' has multiple range declarations")]
    DoubleRange { sym: String },

    #[error("function '{sym}' has no range declaration")]
    MissingRange { sym: String },

    #[error("relation '{sym}' is missing inheritance from a specific arity stating class (i.e. BinaryRelation)")]
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
    
    #[error("'{sym}' is a subcass of disjoint classes ({class1} and {class2})")]
    DisjointSubclass { sid: Vec<SentenceId>, sym: String, class1: String, class2: String },

    #[error("{msg}")]
    Other { msg: String },
}

impl SemanticError {
    pub fn current_level(&self) -> log::Level {
        // Check if -Wall is active
        if ALL_ERRORS.load(Ordering::SeqCst) {
            return log::Level::Error;
        }

        // Check if this specific code/name was promoted to an error
        let promoted = PROMOTED_TO_ERROR.read().expect("Lock poisoned");
        if promoted.contains(self.code()) || promoted.contains(self.name()) {
            log::Level::Error
        } else {
            // 4. GCC Default: Everything else is a Warning
            log::Level::Warn
        }
    }

    pub fn is_warn(&self) -> bool {
        return self.current_level() == log::Level::Warn;
    }

    pub fn handle(&self, store: &KifStore) -> Result<(), Self> {
        if self.is_warn() {
            log::warn!("Semantic Warning:\n");
            self.pretty_print(store, log::Level::Warn);
            Ok(())
        } else {
            Err(self.clone())
        }
    }

    /// Short alphanumeric code that can be passed to `-W` / `--warning`.
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

    /// Kebab-case name that can be passed to `--warning=<name>`.
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

    fn level_color(&self, level: log::Level) -> &str {
        match level {
            log::Level::Error => color_red,
            log::Level::Warn  => color_yellow,
            log::Level::Info  => color_cyan,
            log::Level::Debug => color_blue,
            log::Level::Trace => color_white,
        }
    }

    pub fn pretty_print(&self, store: &KifStore, level: log::Level) {
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
                log::log!(
                    target: "clean",
                    level,
                    "{}\t{}{color_reset}",
                    self.level_color(level),
                    self);
            },
            SemanticError::HeadNotRelation { sid, .. }
            | SemanticError::HeadInvalid { sid, .. } => {
                let dis = SentenceDisplay {
                    sid: *sid, store,
                    indent: 0, show_gutter: true,
                    highlight_arg: 0
                };
                log::log!(
                    target: "clean", 
                    level, 
                    "{}\n\n{}\t{}{color_reset}", 
                    dis,
                    self.level_color(level),
                    self);
            },
            | SemanticError::NonLogicalArg { sid, arg, .. }
            | SemanticError::DomainMismatch { sid, arg, .. } => {
                let idx: i32 = *arg as i32; 
                let dis = SentenceDisplay {
                    sid: *sid, store,
                    indent: 0, show_gutter: true,
                    highlight_arg: idx
                };
                log::log!(
                    target: "clean", 
                    level,
                    "{}\n\n{}\t{}{color_reset}",
                    dis,
                    self.level_color(level),
                    self);
            },
            SemanticError::ArityMismatch { sid, .. } => {
                let dis = SentenceDisplay {
                    sid: *sid, store,
                    indent: 0, show_gutter: true,
                    highlight_arg: -1
                };
                log::log!(
                    target: "clean",
                    level,
                    "{}\n\n{}\t{}{color_reset}",
                    dis,
                    self.level_color(level),
                    self);
            },
            SemanticError::DisjointInstance { sid, .. }
            | SemanticError::DisjointSubclass { sid, .. } => {
                for s in sid {
                    let dis = SentenceDisplay {
                        sid: *s, store,
                        indent: 0, show_gutter: true,
                        highlight_arg: -1
                    };
                    log::log!(
                        target: "clean",
                        level,
                        "{}",
                        dis);
                }
                log::log!(
                    target: "clean",
                    level,
                    "\n{}\t{}{color_reset}",
                    self.level_color(level),
                    self);
            }
        }
    }
}

/// Any error produced by this library.
#[derive(Debug, Clone, Error)]
pub enum KifError {
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error(transparent)]
    Semantic(#[from] SemanticError),
}
