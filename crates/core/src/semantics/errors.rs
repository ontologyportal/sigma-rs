//! Error types and warning-control flags for the `SemanticLayer`.

use std::{collections::HashSet, sync::{RwLock, atomic::{AtomicBool, Ordering}}};

use once_cell::sync::Lazy;
use thiserror::Error;

use crate::{Diagnostic, SentenceId, Severity, Span, ToDiagnostic};

/// Semantic errors: non-fatal during KB construction, fatal during `tell()`.
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

    /// A term has no `documentation` axiom.
    #[error("term '{sym}' has no documentation axiom")]
    MissingDocumentation { sym: String },

    /// A term has more than one `documentation` axiom.
    #[error("term '{sym}' has {count} documentation axioms (expected 1)")]
    MultipleDocumentation { sym: String, count: usize },

    /// A variable appears exactly once in its enclosing formula -- almost
    /// always a typo.
    #[error("variable '{var}' is used only once -- likely a typo")]
    SingleUseVariable { sid: SentenceId, var: String },

    /// A variable in the consequent of an implication is not bound by the
    /// antecedent or an enclosing quantifier.
    #[error("variable '{var}' in consequent is not bound by antecedent or quantifier")]
    FreeVarInConsequent { sid: SentenceId, var: String },

    /// An existential quantifier appears under the antecedent of an
    /// implication; the witness can't be used in the consequent.
    #[error("existential quantifier in implication antecedent: any witness will not be available in the consequent")]
    ExistentialInAntecedent { sid: SentenceId },

    /// A variable appears in a quantifier's variable list but is never used in
    /// the quantified body.
    #[error("variable '{var}' is bound by a quantifier but never used in the body")]
    QuantifierVacuous { sid: SentenceId, var: String },

    /// A symbol is a subclass of a `partition` head but does not appear in the
    /// partition's member list.
    #[error("'{sym}' is a subclass of partitioned class '{partition_class}' but is not listed in the partition")]
    PartitionViolation { sym: String, partition_class: String },

    /// An instance of a class with `exhaustiveDecomposition` does not match any
    /// of the partition's listed sub-classes.
    #[error("'{sym}' is an instance of '{partition_class}' but does not match any partition member")]
    PartitionNonMember { sym: String, partition_class: String },

    /// A term is referenced nowhere in the antecedent or consequent of an
    /// implication/biconditional. Advisory only.
    #[error("term '{sym}' does not appear in any rule (implication or biconditional)")]
    TermNoRule { sym: String },

    /// A loaded constituent references a symbol whose declaration lives in a
    /// constituent that hasn't been loaded.
    #[error("constituent '{current}' references '{sym}' but its declaration lives in unloaded constituent '{defining_constituent}'")]
    MissingConstituentDep { sym: String, current: String, defining_constituent: String },

    /// Two constituents reference each other's terms.
    #[error("constituents '{a}' and '{b}' mutually reference each other's terms")]
    MutualConstituentDep { a: String, b: String },

    /// Other error. Use this sparingly
    #[error("{msg}")]
    Other { msg: String },
}

impl SemanticError {
    /// The severity this error currently resolves to, honouring the global
    /// `-Wall` / `-Werror=<code>` promotion flags.
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

    /// `true` if this error currently classifies as a warning.
    pub fn is_warn(&self) -> bool {
        self.current_level() == log::Level::Warn
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
            Self::MissingDocumentation { .. }     => "W018",
            Self::MultipleDocumentation { .. }    => "W019",
            Self::SingleUseVariable { .. }        => "W020",
            Self::FreeVarInConsequent { .. }      => "W021",
            Self::ExistentialInAntecedent { .. }  => "W022",
            Self::QuantifierVacuous { .. }        => "E023",
            // W024 is unused: `Object`/`object` case collisions are by design in SUMO.
            Self::PartitionViolation { .. }       => "E025",
            Self::PartitionNonMember { .. }       => "E026",
            Self::TermNoRule { .. }               => "W027",
            Self::MissingConstituentDep { .. }    => "E028",
            Self::MutualConstituentDep { .. }     => "W029",
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
            Self::MissingDocumentation { .. }     => "missing-documentation",
            Self::MultipleDocumentation { .. }    => "multiple-documentation",
            Self::SingleUseVariable { .. }        => "single-use-variable",
            Self::FreeVarInConsequent { .. }      => "free-var-in-consequent",
            Self::ExistentialInAntecedent { .. }  => "existential-in-antecedent",
            Self::QuantifierVacuous { .. }        => "quantifier-vacuous",
            Self::PartitionViolation { .. }       => "partition-violation",
            Self::PartitionNonMember { .. }       => "partition-non-member",
            Self::TermNoRule { .. }               => "term-no-rule",
            Self::MissingConstituentDep { .. }    => "missing-constituent-dep",
            Self::MutualConstituentDep { .. }     => "mutual-constituent-dep",
        }
    }

}

/// A semantic-validation pass's findings, classified by severity.
///
/// Each entry is paired with the [`SentenceId`] the finding fired against so
/// consumers can map back to source spans via `kb.sentence(sid)`.
///
/// Classification follows [`SemanticError::is_warn`], which honours the
/// `-Wall` / `-W <code>` / `-q` global flags, so switching a flag between two
/// passes reclassifies identical inputs accordingly.
#[derive(Debug, Clone, Default)]
pub struct Findings {
    /// Hard errors (`is_warn() == false`). Loading commands should treat a
    /// non-empty `errors` list as an abort condition.
    pub errors:   Vec<(SentenceId, SemanticError)>,
    /// Warnings (`is_warn() == true`). Advisory; consumers may render, suppress
    /// (via `-q`), or ignore them.
    pub warnings: Vec<(SentenceId, SemanticError)>,
}

impl Findings {
    /// `true` iff no hard errors were reported. Warnings don't unset
    /// cleanliness; check `warnings.is_empty()` separately for a stricter test.
    pub fn is_clean(&self) -> bool {
        self.errors.is_empty()
    }

    /// Total finding count (errors + warnings).
    pub fn total(&self) -> usize {
        self.errors.len() + self.warnings.len()
    }

    /// Push a finding, classifying it by [`SemanticError::is_warn`].
    pub fn push(&mut self, sid: SentenceId, error: SemanticError) {
        if error.is_warn() {
            self.warnings.push((sid, error));
        } else {
            self.errors.push((sid, error));
        }
    }
}

// -- Global warning-control flags ---------------------------------------------

/// Treat all ignorable semantic errors as fatal (mimics -Wall).
static ALL_ERRORS: AtomicBool = AtomicBool::new(false);

/// Specific error codes or names promoted to errors (mimics -Werror=<code>).
static PROMOTED_TO_ERROR: Lazy<RwLock<HashSet<String>>> =
    Lazy::new(|| RwLock::new(HashSet::new()));

/// Enable or disable treating all ignorable semantic errors as fatal (`-Wall`).
pub fn set_all_errors(val: bool) {
    ALL_ERRORS.store(val, Ordering::SeqCst);
}

/// Promote the given error code or kebab-case name to a hard error
/// (`-Werror=<code>`).
pub fn promote_to_error(code_or_name: &str) {
    if let Ok(mut set) = PROMOTED_TO_ERROR.write() {
        set.insert(code_or_name.to_string());
    }
}

/// Clear the promoted-error set installed by [`promote_to_error`].
pub fn clear_promoted_errors() {
    if let Ok(mut set) = PROMOTED_TO_ERROR.write() {
        set.clear();
    }
}

impl ToDiagnostic for SemanticError {
    fn to_diagnostic(&self) -> Diagnostic {
        let severity = match self.current_level() {
            log::Level::Error => Severity::Error,
            log::Level::Warn  => Severity::Warning,
            log::Level::Info  => Severity::Info,
            _                 => Severity::Hint,
        };
        let (sids, highlight_arg): (Vec<SentenceId>, i32) = match self {
            SemanticError::HeadNotRelation { sid, .. }
            | SemanticError::HeadInvalid   { sid, .. }
            | SemanticError::SingleArity   { sid, .. } => (vec![*sid], 0),
            SemanticError::NonLogicalArg   { sid, arg, .. }
            | SemanticError::DomainMismatch { sid, arg, .. } => (vec![*sid], *arg as i32),
            SemanticError::ArityMismatch   { sid, .. } => (vec![*sid], -1),
            SemanticError::DisjointInstance { sid, .. }
            | SemanticError::DisjointSubclass { sid, .. } => (sid.clone(), -1),
            // Symbol-level errors with no specific sentence anchor.
            _ => (Vec::new(), -1),
        };
        let highlight_var = match self {
            SemanticError::FreeVarInConsequent { var, .. }
            | SemanticError::QuantifierVacuous { var, .. }
            | SemanticError::SingleUseVariable { var, .. } => Some(var.clone()),
            _ => None,
        };
        Diagnostic {
            kind:     "semantic",
            range:    Span::default(),  // filled by caller from Sentence.span
            severity,
            code:     self.name(),
            message:  self.to_string(),
            related:  Vec::new(),
            sids,
            highlight_arg,
            highlight_var,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn semantic_warning_maps_to_warning_severity() {
        let err = SemanticError::FunctionCase { sym: "foo".into() };
        let d   = err.to_diagnostic();
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.kind,     "semantic");
        assert_eq!(d.code,     "function-case");
    }

    #[test]
    fn semantic_error_carries_sid_for_source_context() {
        let err = SemanticError::ArityMismatch {
            sid:      77,
            rel:      "instance".into(),
            expected: 2,
            got:      3,
        };
        let d = err.to_diagnostic();
        assert_eq!(d.sids,          vec![77]);
        assert_eq!(d.highlight_arg, -1);
    }

    #[test]
    fn render_without_source_context_includes_code_and_message() {
        let err = SemanticError::FunctionCase { sym: "Foo".into() };
        let d   = err.to_diagnostic();
        let s   = d.render(None);
        assert!(s.contains("[semantic/function-case]"));
        assert!(s.contains("uppercase"));
    }
}