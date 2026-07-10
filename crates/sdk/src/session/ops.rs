// crates/sdk/src/session/ops.rs
//
// Non-proving session ops: validate / translate / load / open.

#[cfg(feature = "persist")]
use sigmakee_rs_core::DynSink;
#[cfg(feature = "ask")]
use sigmakee_rs_core::ExternalOpts;
use sigmakee_rs_core::{Diagnostic, HasTranslation, TopLayer, TptpLang, TptpOptions};
#[cfg(feature = "persist")]
use sigmakee_rs_core::TranslationLayer;

#[cfg(feature = "ask")]
use crate::Source;

use super::Session;
use super::super::{SdkError, SdkResult};

impl<L: TopLayer> Session<L> {
    /// Run semantic validation over the whole KB and return the findings.
    /// Available on every backend (validation is layer-agnostic).  An empty
    /// vec means clean.
    pub fn validate(&self) -> Vec<Diagnostic> {
        self.kb.validate_all()
    }

    /// Validate one inline KIF formula: parse it into a scratch session, run the
    /// semantic checks over just those sentences, then flush the session so the
    /// KB is left untouched.  Parse failures come back as diagnostics in the
    /// returned vec (not as `Err`).  Works on every backend.
    pub fn validate_formula(&mut self, kif: &str) -> SdkResult<Vec<Diagnostic>> {
        const TAG: &str = "__session:validate_formula()__";
        let r = self.kb.tell(kif, TAG);
        if !r.ok {
            self.kb.flush_session(TAG);
            return Ok(r.diagnostics); // parse failures are findings
        }
        let sids = self.kb.session_sids(TAG);
        let mut diags = Vec::new();
        for sid in sids {
            diags.extend(self.kb.validate_sentence(sid));
        }
        self.kb.flush_session(TAG);
        Ok(diags)
    }

    /// Open an LMDB-backed KB from disk as a translation-only session.  Proving
    /// requires reloading the axioms into a prover-backed [`Session::new`].
    ///
    /// This is concrete to [`TranslationLayer`] because the core's public
    /// [`KnowledgeBase::open`](sigmakee_rs_core::KnowledgeBase::open) returns a
    /// `KnowledgeBase<TranslationLayer>` (the layer-generic opener is
    /// crate-private).  For a prover-backed open, see the per-layer variants
    /// (`open_native`).
    #[cfg(feature = "persist")]
    pub fn open(path: impl AsRef<std::path::Path>, session: String, sink: Option<DynSink>) -> SdkResult<Session<TranslationLayer>> {
        let kb = sigmakee_rs_core::KnowledgeBase::open(path.as_ref(), sink).map_err(SdkError::Kb)?;
        Ok(Session { kb, name: session })
    }

    /// Store an open session to the LMDB backend at the given path. Importantly:
    /// this does NOT create a new backend if it does not exist. You must first 
    /// call [`Session::open()`] to create the backend, then you can use this 
    /// method. The path is the same path the LMDB was opened from.
    #[cfg(feature = "persist")]
    pub fn persist(&self) -> SdkResult<()> {
        self.kb.persist().map_err(|e| SdkError::Kb(e))
    }
}

impl<L: HasTranslation> Session<L> {
    /// Emit the KB as a TPTP problem in `lang` (FOF / TFF / …).  Only the
    /// `TranslationOnly` backend can translate — the native prover has no
    /// translation layer, and the external backend's inner translation layer is
    /// not exposed for direct emission.
    pub fn translate(&mut self, opts: TptpOptions) -> SdkResult<String> {
        Ok(self.kb.to_tptp(&opts, None))
    }

    /// Translate one inline KIF formula to TPTP in `lang`, rendering each parsed
    /// sentence on its own line.  Like [`translate`](Session::translate), only
    /// the `TranslationOnly` backend can emit TPTP.  A parse failure bubbles out
    /// as `Err`.
    pub fn translate_formula(&mut self, kif: &str, lang: TptpLang) -> SdkResult<String> {
        const TAG: &str = "sdk::translate-inline";
        let r = self.kb.tell(kif, TAG);
        if !r.ok {
            self.kb.flush_session(TAG);
            let first = r.diagnostics.into_iter().find(|d| d.is_err());
            return Err(first.map(SdkError::Kb).unwrap_or_else(|| {
                SdkError::Config("inline translate: formula failed to parse".into())
            }));
        }
        let opts = TptpOptions { lang, ..TptpOptions::default() };
        let mut out = String::new();
        for sid in self.kb.session_sids(TAG) {
            out.push_str(&self.kb.format_sentence_tptp(sid, &opts));
            out.push('\n');
        }
        self.kb.flush_session(TAG);
        Ok(out)
    }

    /// Translate a [`TestCase`] into TPTP
    #[cfg(feature = "ask")]
    pub fn translate_test(&mut self, src: Source, opts: TptpOptions, prover_opts: ExternalOpts) -> Result<String, Vec<SdkError>> {
        let tc = self.source_to_test_case(src)?;
        Ok(self.kb.tc_to_tptp(
            tc, 
            &opts, 
            Some(&self.name), 
            Some(prover_opts)
        ).map_err(|e| -> Vec<SdkError> { e.into_iter().map(|e| SdkError::Kb(e)).collect() })?)
    }
}

#[cfg(test)]
mod tests {
    use super::Session;
    use crate::Source;
    use sigmakee_rs_core::{TptpLang, TptpOptions, TranslationLayer};

    fn reader(name: &str, kif: &str) -> Source {
        Source::Reader { name: name.into(), reader: Box::new(std::io::Cursor::new(Vec::from(kif))) }
    }

    #[test]
    fn validate_reports_no_errors_on_clean_kb() {
        let mut s = Session::<TranslationLayer>::new("ops-validate".into());
        s.ingest(reader("t.kif", "(subclass Dog Mammal)"), true);
        assert!(s.validate().iter().all(|d| !d.is_err()),
            "a well-formed taxonomy should have no error-severity findings");
    }

    #[test]
    fn validate_formula_returns_parse_findings_not_err() {
        let mut s = Session::<TranslationLayer>::new("ops-vf".into());
        // A parse failure is a finding (diagnostic in the vec), never `Err`.
        let diags = s.validate_formula("(broken (").unwrap();
        assert!(!diags.is_empty(), "malformed formula should yield diagnostics");
    }

    #[test]
    fn translate_emits_tptp() {
        let mut s = Session::<TranslationLayer>::new("ops-translate".into());
        s.ingest(reader("t.kif", "(subclass Dog Mammal)"), true);
        let tptp = s.translate(TptpOptions { lang: TptpLang::Fof, ..TptpOptions::default() }).unwrap();
        assert!(tptp.contains("fof"), "expected FOF output, got: {tptp}");
    }

    #[test]
    fn translate_formula_emits_a_line() {
        let mut s = Session::<TranslationLayer>::new("ops-tf".into());
        // `translate_formula` renders the bare TPTP term per sentence (not a full
        // `fof(name, role, …)` statement like whole-KB `translate`).
        let line = s.translate_formula("(instance Rex Dog)", TptpLang::Fof).unwrap();
        let low = line.to_lowercase();
        assert!(low.contains("instance") && low.contains("rex"),
            "expected the rendered relation, got: {line}");
    }

    #[cfg(feature = "persist")]
    #[test]
    fn open_rejects_a_non_lmdb_path() {
        // A regular file is not an LMDB environment directory → `open` must error.
        let f = std::env::temp_dir().join("sdk-open-not-an-lmdb");
        std::fs::write(&f, b"not a database").unwrap();
        assert!(Session::<TranslationLayer>::open(&f, "x".into(), None).is_err());
    }
}
