use serde::{Deserialize, Serialize};
use thiserror::Error;

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

    #[error("unexpected end of input")]
    UnexpectedEof { span: Span },

    #[error("unbalanced parentheses")]
    UnbalancedParens { span: Span },

    #[error("operator '{op}' outside first-term position")]
    OperatorOutOfPosition { op: String, span: Span },

    #[error("quantifier operators' first argument must be a sentence comprised only of variables")]
    QuantiferArg { span: Span },

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
    HeadNotRelation { sym: String },

     #[error("sentence head is not a symbol")]
    HeadInvalid,

    #[error("operator arguments must be logical (predicate or operator) sentences")]
    NonLogicalArg,

    #[error("arity mismatch for '{rel}': expected {expected}, got {got}")]
    ArityMismatch { rel: String, expected: usize, got: usize },

    #[error("domain mismatch for '{rel}' argument #{arg}: expected '{domain}'")]
    DomainMismatch { rel: String, arg: usize, domain: String },

    #[error("function '{sym}' has multiple range declarations")]
    DoubleRange { sym: String },

    #[error("function '{sym}' has no range declaration")]
    MissingRange { sym: String },

    #[error("relation {sym} is missing inheritance from a specific arity stating class (i.e. BinaryRelation)")]
    MissingArity { sym: String },

    #[error("symbol '{sym}' is missing a domain declaration for argument {idx}")]
    MissingDomain { sym: String, idx: usize },

    #[error("function '{sym}' should start with an uppercase letter")]
    FunctionCase { sym: String },

    #[error("predicate '{sym}' should start with a lowercase letter")]
    PredicateCase { sym: String },

    #[error("'{sym}' is declared as both an instance and a class (instance and subclass are disjoint)")]
    InstanceSubclassConflict { sym: String },

    #[error("'{sym}' is declared as both a function and a predicate (function and predicate are disjoint)")]
    FunctionPredicateConflict { sym: String },

    #[error("{msg}")]
    Other { msg: String },
}

impl SemanticError {
    /// Short alphanumeric code that can be passed to `-W` / `--warning`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoEntityAncestor { .. }         => "E001",
            Self::HeadNotRelation { .. }          => "E002",
            Self::HeadInvalid                     => "E003",
            Self::NonLogicalArg                   => "E004",
            Self::ArityMismatch { .. }            => "E005",
            Self::DomainMismatch { .. }           => "E006",
            Self::DoubleRange { .. }              => "E007",
            Self::MissingRange { .. }             => "E008",
            Self::MissingArity { .. }             => "E009",
            Self::MissingDomain { .. }            => "E010",
            Self::FunctionCase { .. }             => "E011",
            Self::PredicateCase { .. }            => "E012",
            Self::InstanceSubclassConflict { .. } => "E013",
            Self::FunctionPredicateConflict { .. }=> "E014",
            Self::Other { .. }                    => "E015",
        }
    }

    /// Kebab-case name that can be passed to `--warning=<name>`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::NoEntityAncestor { .. }         => "no-entity-ancestor",
            Self::HeadNotRelation { .. }          => "head-not-relation",
            Self::HeadInvalid                     => "head-invalid",
            Self::NonLogicalArg                   => "non-logical-arg",
            Self::ArityMismatch { .. }            => "arity-mismatch",
            Self::DomainMismatch { .. }           => "domain-mismatch",
            Self::DoubleRange { .. }              => "double-range",
            Self::MissingRange { .. }             => "missing-range",
            Self::MissingArity { .. }             => "missing-arity",
            Self::MissingDomain { .. }            => "missing-domain",
            Self::FunctionCase { .. }             => "function-case",
            Self::PredicateCase { .. }            => "predicate-case",
            Self::InstanceSubclassConflict { .. } => "instance-subclass-conflict",
            Self::FunctionPredicateConflict { .. }=> "function-predicate-conflict",
            Self::Other { .. }                    => "other",
        }
    }

    /// Whether this error can be suppressed via `-W` / `--warning`.
    ///
    /// Errors that represent fundamental type-system violations are never
    /// ignorable because allowing them would produce an incoherent KB.
    pub fn is_ignorable(&self) -> bool {
        !matches!(
            self,
            Self::HeadInvalid
            | Self::InstanceSubclassConflict { .. }
            | Self::FunctionPredicateConflict { .. }
        )
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
