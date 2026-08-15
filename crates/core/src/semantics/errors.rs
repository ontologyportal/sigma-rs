//! Error types for the `SemanticLayer`.
//!
//! Every [`SemanticError`] variant carries its own intrinsic [`Severity`] via
//! [`SemanticError::severity`] — a pure function of *what kind of problem
//! this is*, not of any global mutable state. Structural findings (arity,
//! domain, taxonomy) default to `Error`; advisory findings (naming
//! conventions, single-use variables) default to `Warning`; documentation-
//! completeness findings default to `Hint`. A caller wanting `-Wall`-style
//! promotion (warnings become errors) applies that as an explicit, stateless
//! transform over the resulting `Diagnostic`s — see `crates/cli`'s
//! `apply_severity_overrides` — never by mutating how `SemanticError` itself
//! classifies findings.

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
    #[error(
        "argument {arg} of the operator, {op}, must be logical (predicate or operator) sentence"
    )]
    NonLogicalArg {
        sid: SentenceId,
        arg: usize,
        op: String,
    },

    /// A given symbol expected a certain arity but did not receive it
    #[error("arity mismatch for '{rel}': expected {expected}, got {got}")]
    ArityMismatch {
        sid: SentenceId,
        rel: String,
        expected: usize,
        got: usize,
    },

    /// A given relation symbol expect an argument of a given type, but did not receive it
    #[error("domain mismatch for '{rel}' argument #{arg}: expected '{domain}'")]
    DomainMismatch {
        sid: SentenceId,
        rel: String,
        arg: usize,
        domain: String,
    },

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
    #[error(
        "'{sym}' is declared as both an instance and a class (instance and subclass are disjoint)"
    )]
    InstanceSubclassConflict { sym: String },

    /// A symbol belongs to disjoint classes
    #[error("'{sym}' is an instance of disjoint classes ({class1} and {class2})")]
    DisjointInstance {
        sid: Vec<SentenceId>,
        sym: String,
        class1: String,
        class2: String,
    },

    /// A symbol cannot be derived from disjoint classes
    #[error("'{sym}' is a subclass of disjoint classes ({class1} and {class2})")]
    DisjointSubclass {
        sid: Vec<SentenceId>,
        sym: String,
        class1: String,
        class2: String,
    },

    /// An and / or operator got only a single argument
    #[error("only one argument was passed to an conjunctive/disjunctive operator. Not technically incorrect, but meaningless")]
    SingleArity { sid: SentenceId },

    /// A term has no `documentation` axiom in any language.
    #[error("term '{sym}' has no documentation axiom")]
    MissingDocumentation { sym: String },

    /// A term has more than one `documentation` axiom in the SAME language
    /// (documented in several distinct languages is normal and not flagged).
    #[error("term '{sym}' has {count} documentation axioms in {language} (expected 1)")]
    MultipleDocumentation {
        sym: String,
        language: String,
        count: usize,
    },

    /// A term has no `termFormat` axiom in any language.
    #[error("term '{sym}' has no termFormat axiom")]
    MissingTermFormat { sym: String },

    /// A relation symbol has no `format` axiom in any language.
    #[error("relation '{sym}' has no format axiom")]
    MissingFormatString { sym: String },

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
    PartitionViolation {
        sym: String,
        partition_class: String,
    },

    /// An instance of a class with `exhaustiveDecomposition` does not match any
    /// of the partition's listed sub-classes.
    #[error(
        "'{sym}' is an instance of '{partition_class}' but does not match any partition member"
    )]
    PartitionNonMember {
        sym: String,
        partition_class: String,
    },

    /// A term is referenced nowhere in the antecedent or consequent of an
    /// implication/biconditional. Advisory only.
    #[error("term '{sym}' does not appear in any rule (implication or biconditional)")]
    TermNoRule { sym: String },

    /// A loaded constituent references a symbol whose declaration lives in a
    /// constituent that hasn't been loaded.
    #[error("constituent '{current}' references '{sym}' but its declaration lives in unloaded constituent '{defining_constituent}'")]
    MissingConstituentDep {
        sym: String,
        current: String,
        defining_constituent: String,
    },

    /// Two constituents reference each other's terms.
    #[error("constituents '{a}' and '{b}' mutually reference each other's terms")]
    MutualConstituentDep { a: String, b: String },

    /// Other error. Use this sparingly
    #[error("{msg}")]
    Other { msg: String },
}

impl SemanticError {
    /// This error's intrinsic severity — a pure function of the variant, not
    /// of any global promotion state. See the module doc comment for the
    /// classification: structural findings are `Error`, advisory findings
    /// are `Warning`, documentation-completeness findings are `Hint`.
    pub fn severity(&self) -> Severity {
        match self {
            Self::NoEntityAncestor { .. }
            | Self::HeadNotRelation { .. }
            | Self::HeadInvalid { .. }
            | Self::NonLogicalArg { .. }
            | Self::ArityMismatch { .. }
            | Self::DomainMismatch { .. }
            | Self::DoubleRange { .. }
            | Self::MissingRange { .. }
            | Self::MissingDomain { .. }
            | Self::InstanceSubclassConflict { .. }
            | Self::DisjointInstance { .. }
            | Self::DisjointSubclass { .. }
            | Self::Other { .. }
            | Self::SingleArity { .. }
            | Self::QuantifierVacuous { .. }
            | Self::PartitionViolation { .. }
            | Self::PartitionNonMember { .. }
            | Self::MissingConstituentDep { .. } => Severity::Error,

            Self::MissingArity { .. }
            | Self::FunctionCase { .. }
            | Self::PredicateCase { .. }
            | Self::SingleUseVariable { .. }
            | Self::FreeVarInConsequent { .. }
            | Self::ExistentialInAntecedent { .. }
            | Self::TermNoRule { .. }
            | Self::MutualConstituentDep { .. } => Severity::Warning,

            Self::MissingDocumentation { .. }
            | Self::MultipleDocumentation { .. }
            | Self::MissingTermFormat { .. }
            | Self::MissingFormatString { .. } => Severity::Hint,
        }
    }

    /// Short alphanumeric code for use with `-W` / `--warning`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoEntityAncestor { .. } => "E001",
            Self::HeadNotRelation { .. } => "E002",
            Self::HeadInvalid { .. } => "E003",
            Self::NonLogicalArg { .. } => "E004",
            Self::ArityMismatch { .. } => "E005",
            Self::DomainMismatch { .. } => "E006",
            Self::DoubleRange { .. } => "E007",
            Self::MissingRange { .. } => "E008",
            Self::MissingArity { .. } => "W009",
            Self::MissingDomain { .. } => "E010",
            Self::FunctionCase { .. } => "W011",
            Self::PredicateCase { .. } => "W012",
            Self::InstanceSubclassConflict { .. } => "E013",
            Self::DisjointInstance { .. } => "E014",
            Self::DisjointSubclass { .. } => "E015",
            Self::Other { .. } => "E016",
            Self::SingleArity { .. } => "E017",
            // W018/W019 are retired: MissingDocumentation/MultipleDocumentation
            // moved to the Hint-tier H0xx series below (their severity no
            // longer matches the W-prefix convention).
            Self::SingleUseVariable { .. } => "W020",
            Self::FreeVarInConsequent { .. } => "W021",
            Self::ExistentialInAntecedent { .. } => "W022",
            Self::QuantifierVacuous { .. } => "E023",
            // W024 is unused: `Object`/`object` case collisions are by design in SUMO.
            Self::PartitionViolation { .. } => "E025",
            Self::PartitionNonMember { .. } => "E026",
            Self::TermNoRule { .. } => "W027",
            Self::MissingConstituentDep { .. } => "E028",
            Self::MutualConstituentDep { .. } => "W029",
            // H0xx: Hint-tier documentation-completeness findings (whole-KB
            // pass, see KnowledgeBase::completeness_findings).
            Self::MissingDocumentation { .. } => "H001",
            Self::MultipleDocumentation { .. } => "H002",
            Self::MissingTermFormat { .. } => "H003",
            Self::MissingFormatString { .. } => "H004",
        }
    }

    /// Kebab-case name for use with `--warning=<name>`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::NoEntityAncestor { .. } => "no-entity-ancestor",
            Self::HeadNotRelation { .. } => "head-not-relation",
            Self::HeadInvalid { .. } => "head-invalid",
            Self::NonLogicalArg { .. } => "non-logical-arg",
            Self::ArityMismatch { .. } => "arity-mismatch",
            Self::DomainMismatch { .. } => "domain-mismatch",
            Self::DoubleRange { .. } => "double-range",
            Self::MissingRange { .. } => "missing-range",
            Self::MissingArity { .. } => "missing-arity",
            Self::MissingDomain { .. } => "missing-domain",
            Self::FunctionCase { .. } => "function-case",
            Self::PredicateCase { .. } => "predicate-case",
            Self::InstanceSubclassConflict { .. } => "instance-subclass-conflict",
            Self::DisjointInstance { .. } => "disjoint-instance",
            Self::DisjointSubclass { .. } => "disjoint-subclass",
            Self::Other { .. } => "other",
            Self::SingleArity { .. } => "single-arity",
            Self::SingleUseVariable { .. } => "single-use-variable",
            Self::FreeVarInConsequent { .. } => "free-var-in-consequent",
            Self::ExistentialInAntecedent { .. } => "existential-in-antecedent",
            Self::QuantifierVacuous { .. } => "quantifier-vacuous",
            Self::PartitionViolation { .. } => "partition-violation",
            Self::PartitionNonMember { .. } => "partition-non-member",
            Self::TermNoRule { .. } => "term-no-rule",
            Self::MissingConstituentDep { .. } => "missing-constituent-dep",
            Self::MutualConstituentDep { .. } => "mutual-constituent-dep",
            Self::MissingDocumentation { .. } => "missing-documentation",
            Self::MultipleDocumentation { .. } => "multiple-documentation",
            Self::MissingTermFormat { .. } => "missing-term-format",
            Self::MissingFormatString { .. } => "missing-format-string",
        }
    }
}

impl ToDiagnostic for SemanticError {
    fn to_diagnostic(&self) -> Diagnostic {
        let severity = self.severity();
        let (sids, highlight_arg): (Vec<SentenceId>, i32) = match self {
            SemanticError::HeadNotRelation { sid, .. }
            | SemanticError::HeadInvalid { sid, .. }
            | SemanticError::SingleArity { sid, .. } => (vec![*sid], 0),
            SemanticError::NonLogicalArg { sid, arg, .. }
            | SemanticError::DomainMismatch { sid, arg, .. } => (vec![*sid], *arg as i32),
            SemanticError::ArityMismatch { sid, .. } => (vec![*sid], -1),
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
            kind: "semantic",
            range: Span::default(), // filled by caller from Sentence.span
            severity,
            code: self.name(),
            message: self.to_string(),
            related: Vec::new(),
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
        let d = err.to_diagnostic();
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.kind, "semantic");
        assert_eq!(d.code, "function-case");
    }

    #[test]
    fn semantic_error_carries_sid_for_source_context() {
        let err = SemanticError::ArityMismatch {
            sid: 77,
            rel: "instance".into(),
            expected: 2,
            got: 3,
        };
        let d = err.to_diagnostic();
        assert_eq!(d.sids, vec![77]);
        assert_eq!(d.highlight_arg, -1);
    }

    #[test]
    fn render_without_source_context_includes_code_and_message() {
        let err = SemanticError::FunctionCase { sym: "Foo".into() };
        let d = err.to_diagnostic();
        let s = d.render(None);
        assert!(s.contains("[semantic/function-case]"));
        assert!(s.contains("uppercase"));
    }

    #[test]
    fn structural_findings_default_to_error_severity() {
        // No global flag needed: NoEntityAncestor is intrinsically Error,
        // matching its E001 code — the exact case that used to be Warning
        // by default under the old promotion-only model.
        let err = SemanticError::NoEntityAncestor { sym: "Foo".into() };
        assert_eq!(err.severity(), Severity::Error);
        assert_eq!(err.to_diagnostic().severity, Severity::Error);
    }

    #[test]
    fn documentation_completeness_findings_are_hints() {
        for err in [
            SemanticError::MissingDocumentation { sym: "Foo".into() },
            SemanticError::MultipleDocumentation {
                sym: "Foo".into(),
                language: "EnglishLanguage".into(),
                count: 2,
            },
            SemanticError::MissingTermFormat { sym: "Foo".into() },
            SemanticError::MissingFormatString {
                sym: "likes".into(),
            },
        ] {
            assert_eq!(
                err.severity(),
                Severity::Hint,
                "{} should be Hint-severity",
                err.name()
            );
            assert_eq!(err.to_diagnostic().severity, Severity::Hint);
        }
    }

    #[test]
    fn severity_is_a_pure_function_of_the_variant() {
        // No global state left to consult — two identical errors always
        // agree, and there is nothing left to reset between test runs.
        let a = SemanticError::MissingArity {
            sym: "likes".into(),
        };
        let b = SemanticError::MissingArity {
            sym: "likes".into(),
        };
        assert_eq!(a.severity(), b.severity());
        assert_eq!(a.severity(), Severity::Warning);
    }
}
