// crates/sdk/src/session/ingest.rs
//
// Ingestion logic for sessions

#[cfg(any(feature = "ask", feature = "native-prover"))]
use sigmakee_rs_core::Parser;
use sigmakee_rs_core::PromoteError;
use sigmakee_rs_core::SourceFile;
#[cfg(any(feature = "ask", feature = "native-prover"))]
use sigmakee_rs_core::TestCase;
use sigmakee_rs_core::ToDiagnostic;
use sigmakee_rs_core::TopLayer;

use crate::SdkResult;

use super::super::{SdkError, Source};
use super::Session;

impl<L: TopLayer> Session<L> {
    /// Read a [`Source`], auto-detect its parser, ingest it, and promote it to
    /// axioms.  Errors if the format can't be detected or the source fails to
    /// parse.  Works on every backend (ingestion is layer-agnostic).
    pub fn ingest(&mut self, src: Source, abort: bool) -> Vec<SdkError> {
        self.ingest_counted(src, abort).1
    }

    /// Same as [`ingest`](Self::ingest), but also returns how many individual
    /// source files were actually read -- a single [`Source`] (e.g. a
    /// directory or a git-repo path list) can expand into many.
    pub fn ingest_counted(&mut self, src: Source, abort: bool) -> (usize, Vec<SdkError>) {
        let sources = match src.read(self.sink().as_ref()).map_err(|e| vec![e]) {
            Err(e) => return (0, e),
            Ok(sources) => sources,
        };
        let count = sources.len();
        let mut errs = vec![];
        // Load (reconcile) + promote.  `load` is layer-agnostic; promotion is
        // per-layer (the prover layers take the 1-arg `make_session_axiomatic`,
        // the translation layer the consistency-gated 4-arg form).
        for src in sources {
            errs.extend(self.ingest_inner(src));
        }
        if abort && errs.iter().any(|e| e.is_err()) {
            return (count, errs);
        }
        let Err(e) = self.after_ingest() else {
            return (count, errs);
        };
        errs.push(e);
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

    pub(super) fn ingest_inner(&mut self, src: SourceFile) -> Vec<SdkError> {
        let r = self.kb.load(src, &self.name);
        r.diagnostics.into_iter().map(SdkError::from).collect()
    }

    pub(super) fn after_ingest(&mut self) -> SdkResult<()> {
        self.kb
            .make_session_axiomatic(&self.name)
            .map_err(|e: PromoteError| SdkError::from(e.to_diagnostic()))?;
        Ok(())
    }

    /// Convert a test [`Source`] into a [`TestCase`], ingesting any background
    /// theory it carries into the KB as real, promoted axioms.
    ///
    /// Two kinds of background get ingested + promoted (in one bulk
    /// [`make_session_axiomatic`](Self::after_ingest) pass *after* everything is
    /// loaded, so the prover SInE-selects them):
    ///   * linked axiom libraries (`.ax` / any non-test source the test pulls in);
    ///   * a TPTP problem's `axiom`-role statements — `from_tptp` hands these back
    ///     separately as `background`, distinct from the `Hypothesis`-role
    ///     statements that stay in `tc.axioms` as force-included support.
    ///
    /// The returned `TestCase`'s `axioms` therefore hold only hypotheses; `kb.ask`
    /// stages those session-scoped. (`include` directives were already spliced by
    /// [`Source::read`].)
    #[cfg(any(feature = "ask", feature = "native-prover"))]
    pub(super) fn source_to_test_case(
        &mut self,
        test_src: Source,
    ) -> Result<TestCase, Vec<SdkError>> {
        let sources = test_src.read(self.sink().as_ref()).map_err(|e| vec![e])?;
        let mut errs = vec![];
        let mut tcs = vec![];
        // Did we ingest any background axioms (a linked library or TPTP
        // `axiom`-role statements) that need promoting to selectable axioms?
        let mut ingested_axioms = false;
        for sf in sources {
            if !sf.parser.is_test() {
                // not a test → a linked axiom library: ingest it
                errs.extend(self.ingest_inner(sf));
                ingested_axioms = true;
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
                    errs.extend(self.ingest_inner(SourceFile {
                        parser: Parser::Kif { options: None },
                        name: sf.name.clone(),
                        path: sf.path.clone(),
                        origin: sf.origin,
                        contents: String::new(),
                        prebuilt: Some(background),
                    }));
                    ingested_axioms = true;
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

        // Bulk-promote everything ingested above to axioms, once, before the
        // test runs — so the linked libraries + TPTP background are SInE-
        // selectable rather than transient session assertions.
        if ingested_axioms {
            if let Err(e) = self.after_ingest() {
                errs.push(e);
            }
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
        let (n, errs) = s.ingest_counted(Source::Local(vec![root.clone()]), false);
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
}
