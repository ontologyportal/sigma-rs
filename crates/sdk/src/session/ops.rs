// crates/sdk/src/session/ops.rs
//
// Non-proving session ops: validate / translate / load / open.

#[cfg(feature = "persist")]
use sigmakee_rs_core::DynSink;
#[cfg(feature = "ask")]
use sigmakee_rs_core::ExternalOpts;
#[cfg(feature = "persist")]
use sigmakee_rs_core::TranslationLayer;
use sigmakee_rs_core::{Diagnostic, HasTranslation, TopLayer, TptpLang, TptpOptions};

#[cfg(feature = "ask")]
use crate::Source;

use super::super::{SdkError, SdkResult};
use super::Session;

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
            // Session scope: symbols the scratch input itself declares are
            // only visible in the session overlay.
            diags.extend(self.kb.validate_sentence_in_session(sid, TAG));
        }
        self.kb.flush_session(TAG);
        Ok(diags)
    }

    /// Validate an Ask/Tell pair in ONE scratch session against the live KB:
    /// the assertions are told first, then the query is validated with those
    /// declarations in scope.  Returns `(assertion diagnostics, query
    /// diagnostics)`; parse failures come back as findings, and the scratch
    /// session is flushed either way.
    pub fn validate_scratch(
        &mut self,
        assertions: &str,
        query: &str,
    ) -> (Vec<Diagnostic>, Vec<Diagnostic>) {
        const TAG: &str = "__session:validate_scratch()__";

        let collect = |kif: &str, kb: &mut sigmakee_rs_core::KnowledgeBase<L>| -> Vec<Diagnostic> {
            if kif.trim().is_empty() {
                return Vec::new();
            }
            let before: std::collections::HashSet<_> = kb.session_sids(TAG).into_iter().collect();
            let r = kb.tell(kif, TAG);
            if !r.ok {
                return r.diagnostics;
            }
            kb.session_sids(TAG)
                .into_iter()
                .filter(|sid| !before.contains(sid))
                .flat_map(|sid| kb.validate_sentence_in_session(sid, TAG))
                .collect()
        };

        self.kb.flush_session(TAG);
        let a_diags = collect(assertions, &mut self.kb);
        let q_diags = collect(query, &mut self.kb);
        self.kb.flush_session(TAG);
        (a_diags, q_diags)
    }

    /// Revalidate an edited buffer for `file` **with full KB context**,
    /// updating the live KB to match the buffer.
    ///
    /// Staging the buffer under the file's own name is a diff, so only the
    /// lines that changed are processed, and the changed sentences are
    /// validated against the whole KB, so symbol resolution works.  The
    /// change is committed and left in place; the KB tracks the editor.
    /// Returns whole-file findings (not just the changed sentences'), in
    /// session scope -- buffer-committed sentences are not yet promoted, so
    /// their declarations are visible only in the file's own session overlay.
    ///
    /// Prefer [`Session::validate_formula`] for scratch input that belongs to
    /// no file.
    pub fn validate_buffer(&mut self, file: &str, text: &str) -> Vec<Diagnostic> {
        use sigmakee_rs_core::SourceFile;

        // A syntactically broken buffer must never touch the KB: it parses to
        // zero forms, which the source cache treats as "the file is now
        // empty" and retracts every sentence the file previously contributed.
        // Vet the syntax first (parse-only, milliseconds) and report just
        // those findings.
        let parse_diags = sigmakee_rs_core::kif_parse_diagnostics(text, file);
        if !parse_diags.is_empty() {
            return parse_diags;
        }

        // Diff the buffer into the file's own session and commit it live: the
        // KB simply tracks what the editor holds.  No restore step, so a pure
        // addition emits no FormulaRemoved and cannot trigger the symbol
        // prune.
        let staged = self
            .kb
            .stage(SourceFile::kif(file.into(), text.to_string()), file);
        self.kb.commit(file);
        if !staged.ok {
            return staged.diagnostics;
        }

        self.kb.validate_file_in_session(file, file)
    }

    /// Cap the number of concurrent tasks the KB's reactive cache router may
    /// fan out into.  `0` is clamped to `1` (fully serial).  Shared with every
    /// facade on the same KB.
    ///
    /// The default is `std::thread::available_parallelism()`, which is
    /// unavailable on wasm32 and falls back to `1`; a threads-enabled wasm
    /// embedder (wasm-bindgen-rayon) must call this after spinning up its
    /// thread pool, seeded from `navigator.hardwareConcurrency`, or the pool
    /// will run fully serial.
    pub fn set_max_threads(&self, n: usize) {
        self.kb.cache_config().set_max_threads(n);
    }

    /// Freeze the entire KB into a self-contained byte buffer (heed-free; the
    /// browser freeze/thaw seam -- stash the bytes in IndexedDB / a file and
    /// thaw later with [`Session::restore_bytes`] instead of re-ingesting).
    #[cfg(feature = "snapshot")]
    pub fn snapshot_bytes(&self) -> SdkResult<Vec<u8>> {
        self.kb.snapshot_bytes().map_err(SdkError::from)
    }

    /// Thaw a KB previously frozen by [`Session::snapshot_bytes`], replacing
    /// this session's KB contents in place.  Replacing *inside* the session
    /// (rather than swapping the session out) keeps any `Arc<RwLock<Session>>`
    /// sharing intact: every other facade on the same session sees the
    /// restored KB.
    #[cfg(feature = "snapshot")]
    pub fn restore_bytes(&mut self, bytes: &[u8]) -> SdkResult<()> {
        self.kb =
            sigmakee_rs_core::KnowledgeBase::restore_from_bytes(bytes).map_err(SdkError::from)?;
        Ok(())
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
    pub fn open(
        path: impl AsRef<std::path::Path>,
        session: String,
        sink: Option<DynSink>,
    ) -> SdkResult<Session<TranslationLayer>> {
        let kb =
            sigmakee_rs_core::KnowledgeBase::open(path.as_ref(), sink).map_err(SdkError::from)?;
        Ok(Session { kb, name: session })
    }

    /// Store an open session to the LMDB backend at the given path. Importantly:
    /// this does NOT create a new backend if it does not exist. You must first
    /// call [`Session::open()`] to create the backend, then you can use this
    /// method. The path is the same path the LMDB was opened from.
    #[cfg(feature = "persist")]
    pub fn persist(&self) -> SdkResult<()> {
        self.kb.persist().map_err(SdkError::from)
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
            return Err(first.map(SdkError::from).unwrap_or_else(|| {
                SdkError::Config("inline translate: formula failed to parse".into())
            }));
        }
        let opts = TptpOptions {
            lang,
            ..TptpOptions::default()
        };
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
    pub fn translate_test(
        &mut self,
        src: Source,
        opts: TptpOptions,
        prover_opts: ExternalOpts,
    ) -> Result<String, Vec<SdkError>> {
        let tc = self.source_to_test_case(src)?;
        self.kb
            .tc_to_tptp(tc, &opts, Some(&self.name), Some(prover_opts))
            .map_err(|e| -> Vec<SdkError> { e.into_iter().map(SdkError::from).collect() })
    }

    /// Build a standalone TPTP problem for an external prover: whole-KB
    /// axioms, `assertions_kif` folded in as `hypothesis`-role support (a
    /// scratch session, flushed before returning -- never left in the live
    /// KB), and `query_kif` appended as the `conjecture`.
    ///
    /// Empty `assertions_kif` is fine (no session created).  Errors when the
    /// assertions or query fail to parse, or the query produces no sentence.
    ///
    /// `select_all = false` SInE-selects a query-relevant axiom subset
    /// (seeded from BOTH the assertions and the query, so an assertion's own
    /// vocabulary can pull in axioms it needs); `true` emits the whole
    /// promoted KB, unfiltered.  `selection_tolerance_pct` tunes the SInE
    /// budget (ignored under `select_all`; `None` uses the engine default).
    /// This is the FINAL budget: a one-shot external engine has no feedback
    /// retry, unlike the native backend's autoscaling loop.
    #[cfg(any(feature = "ask", feature = "native-prover"))]
    pub fn tptp_for_ask(
        &mut self,
        assertions_kif: &str,
        query_kif: &str,
        select_all: bool,
        selection_tolerance_pct: Option<f64>,
    ) -> Result<String, Vec<SdkError>> {
        // Assertions and the query go into SEPARATE session tags (rather
        // than one shared tag) so the query's own sids don't need to be
        // set-differenced out of the assertions' -- `session_sids(QUERY_TAG)`
        // is exactly the query's sentences, nothing more.
        const ASSERT_TAG: &str = "__session:tptp_for_ask_assertions__";
        const QUERY_TAG: &str = "__session:tptp_for_ask_query__";
        let kb = &mut self.kb;
        if !assertions_kif.trim().is_empty() {
            let r = kb.tell(assertions_kif, ASSERT_TAG);
            if !r.ok {
                kb.flush_session(ASSERT_TAG);
                return Err(r.diagnostics.into_iter().map(SdkError::from).collect());
            }
        }
        let query_tell = kb.tell(query_kif, QUERY_TAG);
        if !query_tell.ok {
            kb.flush_session(ASSERT_TAG);
            kb.flush_session(QUERY_TAG);
            return Err(query_tell
                .diagnostics
                .into_iter()
                .map(SdkError::from)
                .collect());
        }
        let query_sids = kb.session_sids(QUERY_TAG);
        if query_sids.is_empty() {
            kb.flush_session(ASSERT_TAG);
            kb.flush_session(QUERY_TAG);
            return Err(vec![SdkError::from(
                sigmakee_rs_core::Diagnostic::new_error(
                    "tptp_for_ask",
                    "query",
                    "No query sentence parsed".to_string(),
                ),
            )]);
        }

        let kb_opts = TptpOptions {
            hide_numbers: true,
            ..TptpOptions::default()
        };
        let mut tptp = if select_all {
            kb.to_tptp(&kb_opts, Some(ASSERT_TAG))
        } else {
            // Seed relevance from BOTH the assertions and the query -- same
            // shape the native prover's own seed-building uses (support
            // hypotheses + conjecture).
            let mut seed_sids = kb.session_sids(ASSERT_TAG);
            seed_sids.extend(query_sids.iter().copied());
            kb.to_tptp_selected(
                &kb_opts,
                &seed_sids,
                Some(ASSERT_TAG),
                None,
                selection_tolerance_pct,
            )
        };

        let q_opts = TptpOptions {
            query: true,
            hide_numbers: true,
            ..TptpOptions::default()
        };
        for (i, &sid) in query_sids.iter().enumerate() {
            let conj = kb.format_sentence_tptp(sid, &q_opts);
            tptp.push_str(&format!("\nfof(query_{}, conjecture, ({})).\n", i, conj));
        }

        kb.flush_session(ASSERT_TAG);
        kb.flush_session(QUERY_TAG);
        Ok(tptp)
    }
}

#[cfg(test)]
mod tests {
    use super::Session;
    use crate::Source;
    use sigmakee_rs_core::{TptpLang, TptpOptions, TranslationLayer};

    fn reader(name: &str, kif: &str) -> Source {
        Source::Reader {
            name: name.into(),
            reader: Box::new(std::io::Cursor::new(Vec::from(kif))),
        }
    }

    #[cfg(feature = "snapshot")]
    #[test]
    fn snapshot_bytes_roundtrips_through_restore() {
        let mut s = Session::<TranslationLayer>::new("ops-snap".into());
        s.ingest(reader("t.kif", "(subclass Dog Mammal)"), true);
        assert!(s.kb().symbol_id("Dog").is_some(), "fixture loaded");
        let bytes = s.snapshot_bytes().unwrap();

        let mut thawed = Session::<TranslationLayer>::new("ops-thaw".into());
        thawed.restore_bytes(&bytes).unwrap();
        assert!(
            thawed.kb().symbol_id("Dog").is_some(),
            "restored KB should contain the frozen symbol"
        );
        assert!(
            !thawed.kb().lookup("(subclass Dog Mammal)").is_empty(),
            "restored KB should contain the frozen sentence"
        );
    }

    #[test]
    fn set_max_threads_reaches_the_cache_config() {
        let s = Session::<TranslationLayer>::new("ops-threads".into());
        s.set_max_threads(7);
        assert_eq!(s.kb().cache_config().max_threads(), 7);
        // 0 is clamped to serial, not accepted verbatim.
        s.set_max_threads(0);
        assert_eq!(s.kb().cache_config().max_threads(), 1);
    }

    #[test]
    fn validate_reports_no_errors_on_clean_kb() {
        // `instance`/`subclass` are foundational SUMO primitives normally
        // pre-declared by loading Merge.kif; a synthetic single-line fixture
        // that doesn't load it can't fully bootstrap them (declaring
        // `subclass` a relation requires `instance`, which itself would need
        // bootstrapping) — that gap is inherent to the fixture, not the
        // `(subclass Dog Mammal)` snippet under test. So this checks the
        // snippet's OWN taxonomy is clean, not that the whole diagnostic set
        // (which necessarily also covers the unbootstrapped primitives) is.
        let mut s = Session::<TranslationLayer>::new("ops-validate".into());
        // Mammal must reach Entity: argument symbols are entity-checked too
        // (E001), so an unclosed chain would correctly flag Dog and Mammal.
        s.ingest(
            reader("t.kif", "(subclass Dog Mammal)\n(subclass Mammal Entity)"),
            true,
        );
        let bad: Vec<_> = s
            .validate()
            .into_iter()
            .filter(|d| d.is_err() && (d.message.contains("Dog") || d.message.contains("Mammal")))
            .collect();
        assert!(
            bad.is_empty(),
            "Dog/Mammal's own taxonomy should be error-free; got {bad:?}"
        );
    }

    #[test]
    fn validate_formula_returns_parse_findings_not_err() {
        let mut s = Session::<TranslationLayer>::new("ops-vf".into());
        // A parse failure is a finding (diagnostic in the vec), never `Err`.
        let diags = s.validate_formula("(broken (").unwrap();
        assert!(
            !diags.is_empty(),
            "malformed formula should yield diagnostics"
        );
    }

    #[test]
    fn translate_emits_tptp() {
        let mut s = Session::<TranslationLayer>::new("ops-translate".into());
        s.ingest(reader("t.kif", "(subclass Dog Mammal)"), true);
        let tptp = s
            .translate(TptpOptions {
                lang: TptpLang::Fof,
                ..TptpOptions::default()
            })
            .unwrap();
        assert!(tptp.contains("fof"), "expected FOF output, got: {tptp}");
    }

    #[test]
    fn translate_formula_emits_a_line() {
        let mut s = Session::<TranslationLayer>::new("ops-tf".into());
        // `translate_formula` renders the bare TPTP term per sentence (not a full
        // `fof(name, role, …)` statement like whole-KB `translate`).
        let line = s
            .translate_formula("(instance Rex Dog)", TptpLang::Fof)
            .unwrap();
        let low = line.to_lowercase();
        assert!(
            low.contains("instance") && low.contains("rex"),
            "expected the rendered relation, got: {line}"
        );
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
