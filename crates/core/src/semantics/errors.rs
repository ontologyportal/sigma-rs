//! Error types for the `SemanticLayer`.
//!
//! A semantic finding is any type implementing [`SemanticError`]. Each one
//! carries its own intrinsic [`Severity`] via [`SemanticError::severity`] -- a
//! pure function of *what kind of problem this is*, not of any global mutable
//! state. Structural findings (arity, domain, taxonomy) default to `Error`;
//! advisory findings (naming conventions, single-use variables) default to
//! `Warning`; documentation-completeness findings default to `Hint`. A caller
//! wanting `-Wall`-style promotion (warnings become errors) applies that as an
//! explicit, stateless transform over the resulting `Diagnostic`s -- see
//! `crates/cli`'s `apply_severity_overrides` -- never by mutating how a
//! `SemanticError` classifies itself.
//!
//! A finding lives with the code that raises it: in the validator's own file
//! under `validate/validators/`, in the cache reactor, or in the whole-KB
//! completeness pass. What remains here is the trait itself, the
//! [`semantic_error!`] macro that implements it, and the findings that have no
//! emitter yet -- declared vocabulary awaiting the checks that will raise
//! them.

use std::error::Error;

use crate::{Diagnostic, SentenceId, Severity, Span, ToDiagnostic};

const SEMANTIC_DIAGNOSTIC: &str = "semantic";

/// Semantic errors: non-fatal during KB construction, fatal during `tell()`.
pub trait SemanticError: Error + Send + Sync {
    /// This error's intrinsic severity -- a pure function of the type, not of
    /// any global promotion state. See the module doc comment for the
    /// classification: structural findings are `Error`, advisory findings are
    /// `Warning`, documentation-completeness findings are `Hint`.
    fn severity(&self) -> Severity;

    /// Short alphanumeric code for use with `-W` / `--warning`.
    fn code(&self) -> &'static str;

    /// Kebab-case name for use with `--warning=<name>`.
    fn name(&self) -> &'static str;

    /// The sentence(s) this finding anchors to, and which argument to
    /// highlight (`-1` for none). Symbol-level findings have no anchor.
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (Vec::new(), -1)
    }

    /// The variable to highlight, for the variable-scoped lints.
    fn highlight_var(&self) -> Option<String> {
        None
    }

    /// Uniform Diagnostic construction. Validators never override this.
    fn diagnostic(&self) -> Diagnostic {
        let (sids, highlight_arg) = self.anchors();
        Diagnostic {
            kind: SEMANTIC_DIAGNOSTIC,
            range: Span::default(), // filled by caller from Sentence.span
            severity: self.severity(),
            code: self.name(),
            message: self.to_string(), // Display, via the Error supertrait
            related: Vec::new(),
            sids,
            highlight_arg,
            highlight_var: self.highlight_var(),
        }
    }
}

/// A boxed finding, the carrier every validator's output is erased into.
pub type BoxedError = Box<dyn SemanticError>;

/// `Box<dyn SemanticError>` is not automatically an `Error`, and the
/// `SemanticError: Error` supertrait needs it to be. Mirrors the same
/// forwarding impl `parse::error` writes for `Box<dyn ParseError>`.
impl Error for BoxedError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        (**self).source()
    }
}

/// Lets a validator that raises more than one kind of finding declare
/// `type Error = BoxedError` and still satisfy the `Error: SemanticError`
/// bound. Forwards every method to the inner value.
impl SemanticError for BoxedError {
    fn severity(&self) -> Severity {
        (**self).severity()
    }
    fn code(&self) -> &'static str {
        (**self).code()
    }
    fn name(&self) -> &'static str {
        (**self).name()
    }
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (**self).anchors()
    }
    fn highlight_var(&self) -> Option<String> {
        (**self).highlight_var()
    }
}

impl ToDiagnostic for dyn SemanticError {
    fn to_diagnostic(&self) -> Diagnostic {
        self.diagnostic()
    }
}

impl ToDiagnostic for BoxedError {
    fn to_diagnostic(&self) -> Diagnostic {
        (**self).diagnostic()
    }
}

// -- The finding vocabulary ------------------------------------------------
//
// Each type is defined beside the code that raises it; they are re-exported
// here so the rest of the semantic layer has a single import path for the
// whole vocabulary regardless of where any one finding lives.

pub use crate::kb::semantics::{
    MissingDocumentation, MissingFormatString, MissingTermFormat, MultipleDocumentation,
};
pub use crate::semantics::caches::range::DoubleRange;
pub use crate::semantics::validate::validators::arity::ArityMismatch;
pub use crate::semantics::validate::validators::camel_case::TermCamelCase;
pub use crate::semantics::validate::validators::domain::DomainMismatch;
pub use crate::semantics::validate::validators::entity_ancestor::NoEntityAncestor;
pub use crate::semantics::validate::validators::free_var_in_consequent::FreeVarInConsequent;
pub use crate::semantics::validate::validators::head_is_relation::HeadNotRelation;
pub use crate::semantics::validate::validators::iff_shape::ExistentialInIff;
pub use crate::semantics::validate::validators::implies_shape::ExistentialInAntecedent;
pub use crate::semantics::validate::validators::non_logical_arg::NonLogicalArg;
pub use crate::semantics::validate::validators::only_rel::TooGeneralRel;
pub use crate::semantics::validate::validators::quantifier_vacuous::QuantifierVacuous;
pub use crate::semantics::validate::validators::relation_metadata::{
    MissingArity, MissingDomain, MissingRange,
};
pub use crate::semantics::validate::validators::single_arity::SingleArity;
pub use crate::semantics::validate::validators::single_use_variable::SingleUseVariable;
pub use crate::semantics::validate::validators::symbol_case::{
    FunctionCase, PredicateCase, TermCase,
};
pub use crate::semantics::validate::Other;

/// Implement [`SemanticError`] for a finding type.
///
/// The trailing items are spliced into the generated `impl`, so a finding that
/// anchors to a sentence or highlights a variable supplies its own `anchors` /
/// `highlight_var` there; omitting them takes the symbol-level defaults.
macro_rules! semantic_error {
    ($ty:ty, $code:literal, $name:literal, $sev:ident $(, $extra:item)* $(,)?) => {
        impl $crate::semantics::errors::SemanticError for $ty {
            fn code(&self) -> &'static str { $code }
            fn name(&self) -> &'static str { $name }
            fn severity(&self) -> $crate::Severity { $crate::Severity::$sev }
            $($extra)*
        }
    };
}
pub(crate) use semantic_error;

// -- Shared findings -------------------------------------------------------
//
// Raised from more than one place, so they cannot live inside any single
// validator file without inverting the layer ordering.

use thiserror::Error;

/// The sentence head is not a symbol.
#[derive(Debug, Clone, Error)]
#[error("sentence head is not a symbol")]
pub struct HeadInvalid {
    pub sid: SentenceId,
}
semantic_error!(
    HeadInvalid,
    "E003",
    "head-invalid",
    Error,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (vec![self.sid], 0)
    },
);

/// Symbols cannot be both a class and an instance.
#[derive(Debug, Clone, Error)]
#[error("'{sym}' is declared as both an instance and a class (instance and subclass are disjoint)")]
pub struct InstanceSubclassConflict {
    pub sym: String,
}
semantic_error!(
    InstanceSubclassConflict,
    "E013",
    "instance-subclass-conflict",
    Error
);

/// A symbol is an instance of two disjoint classes.
#[derive(Debug, Clone, Error)]
#[error("'{sym}' is an instance of disjoint classes ({class1} and {class2})")]
pub struct DisjointInstance {
    pub sid: Vec<SentenceId>,
    pub sym: String,
    pub class1: String,
    pub class2: String,
}
semantic_error!(
    DisjointInstance,
    "E014",
    "disjoint-instance",
    Error,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (self.sid.clone(), -1)
    },
);

/// A symbol is a subclass of two disjoint classes.
#[derive(Debug, Clone, Error)]
#[error("'{sym}' is a subclass of disjoint classes ({class1} and {class2})")]
pub struct DisjointSubclass {
    pub sid: Vec<SentenceId>,
    pub sym: String,
    pub class1: String,
    pub class2: String,
}
semantic_error!(
    DisjointSubclass,
    "E015",
    "disjoint-subclass",
    Error,
    fn anchors(&self) -> (Vec<SentenceId>, i32) {
        (self.sid.clone(), -1)
    },
);

/// A subclass of a partitioned class is absent from the partition.
#[derive(Debug, Clone, Error)]
#[error("'{sym}' is a subclass of partitioned class '{partition_class}' but is not listed in the partition")]
pub struct PartitionViolation {
    pub sym: String,
    pub partition_class: String,
}
semantic_error!(PartitionViolation, "E025", "partition-violation", Error);

/// An instance of an exhaustively decomposed class matches no partition member.
#[derive(Debug, Clone, Error)]
#[error("'{sym}' is an instance of '{partition_class}' but does not match any partition member")]
pub struct PartitionNonMember {
    pub sym: String,
    pub partition_class: String,
}
semantic_error!(PartitionNonMember, "E026", "partition-non-member", Error);

/// A term appears in no rule.
#[derive(Debug, Clone, Error)]
#[error("term '{sym}' does not appear in any rule (implication or biconditional)")]
pub struct TermNoRule {
    pub sym: String,
}
semantic_error!(TermNoRule, "W027", "term-no-rule", Warning);

/// A referenced term is declared in an unloaded constituent.
#[derive(Debug, Clone, Error)]
#[error("constituent '{current}' references '{sym}' but its declaration lives in unloaded constituent '{defining_constituent}'")]
pub struct MissingConstituentDep {
    pub sym: String,
    pub current: String,
    pub defining_constituent: String,
}
semantic_error!(
    MissingConstituentDep,
    "E028",
    "missing-constituent-dep",
    Error
);

/// Two constituents reference each other's terms.
#[derive(Debug, Clone, Error)]
#[error("constituents '{a}' and '{b}' mutually reference each other's terms")]
pub struct MutualConstituentDep {
    pub a: String,
    pub b: String,
}
semantic_error!(
    MutualConstituentDep,
    "W029",
    "mutual-constituent-dep",
    Warning
);

// -- Whole-KB completeness findings (see `kb::semantics`) ------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantics::validate::validators::arity::ArityMismatch;
    use crate::semantics::validate::validators::symbol_case::FunctionCase;

    #[test]
    fn advisory_findings_map_to_warning_severity() {
        let err = FunctionCase {
            sid: 1,
            index: 0,
            sym: "foo".into(),
        };
        let d = err.diagnostic();
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.kind, "semantic");
        assert_eq!(d.code, "function-case");
    }

    #[test]
    fn anchors_carry_the_sid_for_source_context() {
        let err = ArityMismatch {
            sid: 77,
            rel: "instance".into(),
            expected: 2,
            got: 3,
        };
        let d = err.diagnostic();
        assert_eq!(d.sids, vec![77]);
        assert_eq!(d.highlight_arg, -1);
    }

    #[test]
    fn render_without_source_context_includes_code_and_message() {
        let err = FunctionCase {
            sid: 1,
            index: 0,
            sym: "Foo".into(),
        };
        let s = err.diagnostic().render(None);
        assert!(s.contains("[semantic/function-case]"), "got {s}");
        assert!(s.contains("uppercase"), "got {s}");
    }

    #[test]
    fn symbol_level_findings_default_to_no_anchor() {
        // The default `anchors` impl: no sentence, no highlighted argument.
        let err = TermNoRule { sym: "Foo".into() };
        let (sids, arg) = err.anchors();
        assert!(sids.is_empty());
        assert_eq!(arg, -1);
    }

    #[test]
    fn severity_is_a_pure_function_of_the_finding() {
        // No global state to consult -- two identical findings always agree,
        // and there is nothing to reset between test runs.
        let a = TermNoRule {
            sym: "likes".into(),
        };
        let b = TermNoRule {
            sym: "likes".into(),
        };
        assert_eq!(a.severity(), b.severity());
        assert_eq!(a.severity(), Severity::Warning);
    }

    #[test]
    fn boxed_error_forwards_every_method_to_the_inner_finding() {
        // The forwarding impl is what lets a multi-finding validator declare
        // `type Error = BoxedError` and still satisfy the trait bound.
        let inner = ArityMismatch {
            sid: 5,
            rel: "instance".into(),
            expected: 2,
            got: 3,
        };
        let boxed: BoxedError = Box::new(inner.clone());
        assert_eq!(boxed.code(), inner.code());
        assert_eq!(boxed.name(), inner.name());
        assert_eq!(boxed.severity(), inner.severity());
        assert_eq!(boxed.anchors(), inner.anchors());
        assert_eq!(boxed.to_string(), inner.to_string());
    }
}
