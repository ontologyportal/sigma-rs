//! KIF → TPTP translation.
//!
//! Replaces `cli::translate::run_translate`.  The builder accepts
//! either a target of "the whole KB" or "this inline formula"; it
//! returns the rendered TPTP plus a per-sentence breakout when the
//! input was inline.

use sigmakee_rs_core::{KnowledgeBase, TptpLang, TptpOptions};

use crate::error::{SdkError, SdkResult};
use crate::report::{TranslateReport, TranslatedSentence};

/// What to translate.
pub enum TranslateTarget {
    /// Render the whole KB (optionally restricted to a session).  See
    /// [`TranslateOp::session`] to scope.
    Kb,
    /// Translate one inline KIF formula.  `tag` becomes the session
    /// the formula is loaded into.
    Formula {
        /// Synthetic file identifier the formula is ingested under.
        tag: String,
        /// The KIF source text to translate.
        text: String,
    },
}

/// Builder for a TPTP translation pass.
pub struct TranslateOp<'a> {
    kb:      &'a mut KnowledgeBase,
    target:  TranslateTarget,
    options: TptpOptions,
    session: Option<String>,
}

impl<'a> TranslateOp<'a> {
    /// Translate every sentence in the KB.
    pub fn kb(kb: &'a mut KnowledgeBase) -> Self {
        Self {
            kb,
            target:  TranslateTarget::Kb,
            options: TptpOptions::default(),
            session: None,
        }
    }

    /// Translate one inline KIF formula.
    pub fn formula(
        kb:   &'a mut KnowledgeBase,
        tag:  impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            kb,
            target:  TranslateTarget::Formula {
                tag:  tag.into(),
                text: text.into(),
            },
            options: TptpOptions::default(),
            session: None,
        }
    }

    /// Output dialect (`Fof` or `Tff`).  Default is `Fof`.
    pub fn lang(mut self, l: TptpLang) -> Self {
        self.options.lang = l;
        self
    }

    /// Show numeric IDs alongside formulas.  Default is `false`.
    pub fn show_numbers(mut self, yes: bool) -> Self {
        self.options.hide_numbers = !yes;
        self
    }

    /// Emit a `% <kif>` comment line above each translated formula.
    /// Default is `false`.
    pub fn show_kif_comments(mut self, yes: bool) -> Self {
        self.options.show_kif_comment = yes;
        self
    }

    /// For [`TranslateTarget::Kb`]: restrict translation to the named
    /// session.  Has no effect on inline-formula translation (the tag
    /// is the session).
    pub fn session(mut self, s: impl Into<String>) -> Self {
        self.session = Some(s.into());
        self
    }

    /// Replace the entire `TptpOptions` struct in one go.  Use this
    /// if you need to set fields the convenience setters above don't
    /// expose.  Note this overwrites any previously-set options.
    pub fn options(mut self, opts: TptpOptions) -> Self {
        self.options = opts;
        self
    }

    /// Run the translation.
    pub fn run(self) -> SdkResult<TranslateReport> {
        let TranslateOp { kb, target, options, session } = self;
        match target {
            TranslateTarget::Kb       => Ok(translate_kb(kb, &options, session.as_deref())),
            TranslateTarget::Formula { tag, text } => translate_formula(kb, &tag, &text, &options),
        }
    }
}

fn translate_kb(
    kb:      &KnowledgeBase,
    options: &TptpOptions,
    session: Option<&str>,
) -> TranslateReport {
    // Validate-all is a warning surface; never fatal.  Mirrors the CLI
    // which prints semantic warnings then renders TPTP regardless.
    let semantic_warnings = kb.validate_all();
    let tptp = kb.to_tptp(options, session);
    TranslateReport {
        tptp,
        sentences: Vec::new(),
        semantic_warnings,
        session: session.map(str::to_string),
    }
}

fn translate_formula(
    kb:      &mut KnowledgeBase,
    tag:     &str,
    text:    &str,
    options: &TptpOptions,
) -> SdkResult<TranslateReport> {
    let mut report = TranslateReport {
        session: Some(tag.to_string()),
        ..Default::default()
    };

    let result = kb.load_kif(text, tag, Some(tag));
    if !result.ok {
        // For translate, parse failures are infrastructural — we
        // can't translate text we couldn't parse.  Bubble out.
        if let Some(first) = result.errors.into_iter().next() {
            return Err(SdkError::Kb(first));
        }
        return Err(SdkError::Config(format!(
            "load_kif reported failure for '{}' but produced no errors",
            tag
        )));
    }

    let sids = kb.session_sids(tag);
    if sids.is_empty() {
        return Err(SdkError::Config(format!(
            "no sentences were parsed from formula tagged '{}'",
            tag
        )));
    }

    // Surface semantic warnings without aborting — TPTP for invalid
    // KIF is still produced (the prover will tell you it's nonsense).
    for &sid in &sids {
        if let Err(e) = kb.validate_sentence(sid) {
            report.semantic_warnings.push((sid, e));
        }
    }

    let mut combined = String::new();
    for &sid in &sids {
        let kif  = kb.sentence_kif_str(sid);
        let tptp = kb.format_sentence_tptp(sid, options);
        if options.show_kif_comment {
            combined.push_str("% ");
            combined.push_str(&kif);
            combined.push('\n');
        }
        combined.push_str(&tptp);
        combined.push('\n');
        report.sentences.push(TranslatedSentence { sid, kif, tptp });
    }
    report.tptp = combined;
    Ok(report)
}
