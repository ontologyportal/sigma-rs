//! Validation: parse-checking and semantic-checking either the whole
//! KB or one inline formula.
//!
//! Replaces the CLI's `cli::validate::run_validate` /
//! `validate_single_formula` / `validate_all_roots` triplet with a
//! single builder + structured report.  No printing, no exit codes;
//! the report is returned and the caller decides how to render it.

use sigmakee_rs_core::KnowledgeBase;

use crate::error::{SdkError, SdkResult};
use crate::report::ValidationReport;

/// What [`ValidateOp`] is targeting.
pub enum ValidateTarget {
    /// Walk every sentence currently in the KB and run semantic checks.
    All,
    /// Parse `text` against the KB (loaded into a session named `tag`),
    /// then validate the resulting sentences.  The KB itself is NOT
    /// validated unless the caller leaves `skip_kb_check` at its
    /// default `false`.
    Formula {
        /// Synthetic file identifier the formula is loaded under
        /// (e.g. `"<inline>"`, `"buffer-id-42"`).  Reused as the
        /// session name and recorded on `ValidationReport.session`.
        tag: String,
        /// The KIF source text to parse and validate.
        text: String,
    },
}

/// Builder for a validation pass.
pub struct ValidateOp<'a> {
    kb:            &'a mut KnowledgeBase,
    target:        ValidateTarget,
    parse_only:    bool,
    skip_kb_check: bool,
}

impl<'a> ValidateOp<'a> {
    /// Validate every sentence currently in the KB.
    pub fn all(kb: &'a mut KnowledgeBase) -> Self {
        Self {
            kb,
            target:        ValidateTarget::All,
            parse_only:    false,
            skip_kb_check: false,
        }
    }

    /// Validate one inline KIF formula.  `tag` becomes the session
    /// the formula is loaded into and is what `report.session` holds
    /// after `run` returns.
    pub fn formula(
        kb:   &'a mut KnowledgeBase,
        tag:  impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            kb,
            target:        ValidateTarget::Formula {
                tag:  tag.into(),
                text: text.into(),
            },
            parse_only:    false,
            skip_kb_check: false,
        }
    }

    /// Skip semantic validation; only verify the input parses.  When
    /// targeting [`ValidateTarget::All`] this short-circuits to a no-op
    /// success because the KB cannot hold un-parsed sentences.
    pub fn parse_only(mut self, yes: bool) -> Self {
        self.parse_only = yes;
        self
    }

    /// When validating an inline formula, skip the whole-KB validation
    /// pass that normally runs first.  Has no effect on
    /// [`ValidateTarget::All`].  Mirrors the CLI's `--no-kb-check`.
    pub fn skip_kb_check(mut self, yes: bool) -> Self {
        self.skip_kb_check = yes;
        self
    }

    /// Run the validation.  Returns `Ok(report)` even when findings
    /// are present — `Err` is reserved for infrastructure failures.
    pub fn run(self) -> SdkResult<ValidationReport> {
        match self.target {
            ValidateTarget::All => Ok(validate_all(self.kb, self.parse_only)),
            ValidateTarget::Formula { tag, text } => validate_formula(
                self.kb,
                &tag,
                &text,
                self.parse_only,
                self.skip_kb_check,
            ),
        }
    }
}

fn validate_all(kb: &KnowledgeBase, parse_only: bool) -> ValidationReport {
    if parse_only {
        // KB sentences are already parsed; nothing to do.  Inspected
        // count is left at 0 to signal "no semantic pass ran".
        return ValidationReport::default();
    }
    // Use the classified-findings entry point so we surface BOTH
    // hard errors and warnings.  `sigmakee-rs-core` no longer auto-prints
    // either one; the SDK's report is the canonical handoff.
    let findings = kb.validate_all_findings();
    let inspected = findings.errors.len() + findings.warnings.len();
    ValidationReport {
        semantic_errors:   findings.errors,
        semantic_warnings: findings.warnings,
        parse_errors:      Vec::new(),
        inspected,
        session:           None,
    }
}

fn validate_formula(
    kb:            &mut KnowledgeBase,
    tag:           &str,
    text:          &str,
    parse_only:    bool,
    skip_kb_check: bool,
) -> SdkResult<ValidationReport> {
    let mut report = ValidationReport {
        session: Some(tag.to_string()),
        ..Default::default()
    };

    // Optionally validate the existing KB first.  Mirrors the CLI's
    // default-on behaviour; consumers who already trust their KB can
    // skip this with `.skip_kb_check(true)`.
    if !parse_only && !skip_kb_check {
        let pre = kb.validate_all_findings();
        report.semantic_errors.extend(pre.errors);
        report.semantic_warnings.extend(pre.warnings);
    }

    let result = kb.load_kif(text, tag, Some(tag));
    if !result.ok {
        // Surface every parse / semantic error from the load itself.
        // Don't bail with `Err` — the report carries the findings.
        report.parse_errors = result.errors;
        return Ok(report);
    }

    if parse_only {
        return Ok(report);
    }

    let sids = kb.session_sids(tag);
    if sids.is_empty() {
        return Err(SdkError::Config(format!(
            "no sentences were parsed from formula tagged '{}'",
            tag
        )));
    }
    report.inspected = sids.len();
    // Per-sentence findings, classified.  `validate_sentence_findings`
    // wraps `with_collector` so warnings are surfaced alongside
    // hard errors instead of being silently consumed.
    for sid in sids {
        let f = kb.validate_sentence_findings(sid);
        report.semantic_errors.extend(f.errors);
        report.semantic_warnings.extend(f.warnings);
    }
    Ok(report)
}
