// crates/sdk/src/session/ingest.rs
//
// Ingestion logic for sessions

#[cfg(any(feature = "external-prover", feature = "native-prover"))]
use sigmakee_rs_core::Parser;
use sigmakee_rs_core::SourceFile;
#[cfg(any(feature = "external-prover", feature = "native-prover"))]
use sigmakee_rs_core::TestCase;
#[cfg(any(feature = "external-prover", feature = "native-prover"))]
use sigmakee_rs_core::ToDiagnostic;
use sigmakee_rs_core::TopLayer;

use super::super::{SdkError, Source};
use super::Session;

impl<L: TopLayer> Session<L> {
    /// Read a [`Source`], auto-detect its parser, and load and promote each
    /// file it yields under its own source key
    /// ([`KnowledgeBase::load_and_promote`](sigmakee_rs_core::KnowledgeBase::load_and_promote)).
    /// Every file is promoted, whatever its diagnostics; a file whose
    /// promotion is rejected stays loaded as unpromoted session content.
    /// Returns every file's diagnostics.  Works on every backend.
    pub fn ingest(&mut self, src: Source) -> Vec<SdkError> {
        self.ingest_counted(src).1
    }

    /// Same as [`ingest`](Self::ingest), but also returns how many individual
    /// source files were actually read -- a single [`Source`] (e.g. a
    /// directory or a git-repo path list) can expand into many.
    pub fn ingest_counted(&mut self, src: Source) -> (usize, Vec<SdkError>) {
        let sources = match src.read(self.sink().as_ref()).map_err(|e| vec![e]) {
            Err(e) => return (0, e),
            Ok(sources) => sources,
        };
        let count = sources.len();
        let errs = sources
            .into_iter()
            .flat_map(|src| self.load_and_promote(src))
            .collect();
        (count, errs)
    }

    /// Rollback all changes to the KB made during this session. What this does
    /// is:
    /// - flush any session assertions from the KB
    /// - rollback uncommitted changes to existing axiom constituents
    ///
    /// **WARNING**: This is currently untested. The only way to ensure a full
    /// rollback would be to just not persist the change, and just drop the session
    pub fn rollback(&mut self) -> Vec<SdkError> {
        todo!("I have to implement Session::rollback()")
    }

    /// Load and promote one file under its own source key; its diagnostics.
    fn load_and_promote(&mut self, src: SourceFile) -> Vec<SdkError> {
        let session = src.key();
        let r = self.kb.load_and_promote(src, &session);
        r.diagnostics.into_iter().map(SdkError::from).collect()
    }

    /// Convert a test [`Source`] into a [`TestCase`], ingesting any background
    /// theory it carries into the KB as real, promoted axioms.
    ///
    /// Two kinds of background get loaded and promoted, so the prover
    /// SInE-selects them:
    ///   * linked axiom libraries (`.ax` / any non-test source the test pulls in);
    ///   * a TPTP problem's `axiom`-role statements — `from_tptp` hands these back
    ///     separately as `background`, distinct from the `Hypothesis`-role
    ///     statements that stay in `tc.axioms` as force-included support.
    ///
    /// The returned `TestCase`'s `axioms` therefore hold only hypotheses; `kb.ask`
    /// stages those session-scoped. (`include` directives were already spliced by
    /// [`Source::read`].)
    #[cfg(any(feature = "external-prover", feature = "native-prover"))]
    pub(super) fn source_to_test_case(
        &mut self,
        test_src: Source,
    ) -> Result<TestCase, Vec<SdkError>> {
        let sources = test_src.read(self.sink().as_ref()).map_err(|e| vec![e])?;
        let mut errs = vec![];
        let mut tcs = vec![];
        for sf in sources {
            if !sf.parser.is_test() {
                // not a test -> a linked axiom library
                errs.extend(self.load_and_promote(sf));
                continue;
            }
            if matches!(sf.parser, Parser::Tptp { .. }) {
                let (tc, background, parse_errs) = TestCase::from_tptp(&sf.contents, &sf.name);
                if !parse_errs.is_empty() {
                    errs.extend(
                        parse_errs
                            .into_iter()
                            .map(|(_, p)| SdkError::from(p.to_diagnostic())),
                    );
                    continue;
                }
                // Background theory is NOT the test obligation: ingest it as
                // ordinary, promotable axioms — not as `tc.axioms` support.
                if !background.is_empty() {
                    errs.extend(self.load_and_promote(SourceFile {
                        parser: Parser::Kif { options: None },
                        name: sf.name.clone(),
                        path: sf.path.clone(),
                        origin: sf.origin,
                        contents: String::new(),
                        prebuilt: Some(background),
                    }));
                }
                tcs.push(tc);
                continue;
            }
            // `.tq`: bare KIF statements are already `Hypothesis`-role support.
            let (docs, parse_errs) = sf.parser.parse(&sf.contents, &sf.name);
            if !parse_errs.is_empty() {
                // Skip tests with parse errors
                errs.extend(
                    parse_errs
                        .into_iter()
                        .map(|(_, p)| SdkError::from(p.to_diagnostic())),
                );
                continue;
            }
            let (tc, _) = TestCase::from_doc_items(&docs, &sf.name);
            tcs.push(tc);
        }

        if errs.iter().any(|err| err.is_err()) {
            return Err(errs);
        }
        if tcs.is_empty() {
            return Err(vec![SdkError::NoProblem]);
        }
        Ok(tcs.into_iter().next().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sigmakee_rs_core::TranslationLayer;

    // A directory source expands into multiple files; `ingest_counted`
    // should report that per-file count, not "1" for the directory itself.
    #[test]
    fn ingest_counted_reports_per_file_not_per_source() {
        use std::fs;
        let root = std::env::temp_dir().join("sdk-ingest-counted");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.kif"), "(subclass A B)").unwrap();
        fs::write(root.join("b.kif"), "(subclass C D)").unwrap();

        let mut s = Session::<TranslationLayer>::new("ingest-counted-test".into());
        let (n, errs) = s.ingest_counted(Source::Local(vec![root.clone()]));
        assert_eq!(
            n, 2,
            "two files in the directory should count as two sources"
        );
        assert!(
            errs.iter().all(|e| !e.is_err()),
            "unexpected ingest errors: {errs:?}"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn each_file_is_loaded_and_promoted_on_its_own_even_with_parse_errors() {
        use std::fs;
        let root = std::env::temp_dir().join("sdk-ingest-per-file");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("good.kif"), "(subclass A B)").unwrap();
        fs::write(root.join("bad.kif"), "(subclass C D)\n(subclass E").unwrap();

        let mut s = Session::<TranslationLayer>::new("per-file".into());
        let errs = s.ingest(Source::Local(vec![root.clone()]));
        assert!(
            errs.iter().any(|e| e.is_err()),
            "the parse error is reported"
        );
        for (file, sym) in [("good.kif", "A"), ("bad.kif", "C")] {
            let key = root.join(file).to_string_lossy().into_owned();
            assert!(
                s.kb().session_sids(&key).is_empty(),
                "{file} was promoted out of its session"
            );
            assert!(
                s.kb().symbol_id(sym).is_some(),
                "{file}'s formula is in the KB"
            );
        }
        assert!(
            s.kb().session_sids("per-file").is_empty(),
            "no batch session"
        );

        let _ = fs::remove_dir_all(&root);
    }
}
