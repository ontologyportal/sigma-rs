//! Ingest KIF text into a [`KnowledgeBase`].
//!
//! `IngestOp` is the SDK entry point for "merge this KIF into the KB".
//! Sources can come from three places, and one builder mixes them
//! freely:
//!
//! - **Already-in-memory text** via [`IngestOp::add_source`] — for
//!   network / stdin / inline / test inputs.  No I/O on the SDK side.
//! - **A file path** via [`IngestOp::add_file`] — SDK opens and
//!   reads the file.  `tag` is the path's display string.
//! - **A directory** via [`IngestOp::add_dir`] — SDK enumerates
//!   `*.kif` children (non-recursive, sorted) and reads each.
//!
//! All three converge on the same internal pipeline: each `(tag, text)`
//! pair is dispatched to either reconcile (tag already in KB) or
//! fresh-load (tag new), and per-source results aggregate into one
//! [`IngestReport`].
//!
//! # Example: pure in-memory (network / stdin)
//!
//! ```no_run
//! use sumo_sdk::IngestOp;
//!
//! let mut kb = sumo_kb::KnowledgeBase::new();
//! let body   = "(subclass Animal Organism)".to_string();
//!
//! IngestOp::new(&mut kb)
//!     .add_source("ws://client/42", body)
//!     .run()
//!     .unwrap();
//! ```
//!
//! # Example: filesystem
//!
//! ```no_run
//! use sumo_sdk::IngestOp;
//!
//! let mut kb = sumo_kb::KnowledgeBase::new();
//!
//! IngestOp::new(&mut kb)
//!     .add_file("Merge.kif")
//!     .add_dir("ontology/")
//!     .run()
//!     .unwrap();
//! ```

use std::path::{Path, PathBuf};

use sumo_kb::KnowledgeBase;

use crate::error::{SdkError, SdkResult};
use sumo_kb::{ProgressEvent, ProgressSink};
use crate::report::ingest::SourceIngestStatus;
use crate::report::IngestReport;

/// Internal source representation.  Held privately so the public
/// API stays small (`add_source` / `add_file` / `add_dir`).  When
/// `run()` fires we expand File/Dir into resident-text entries.
enum Source {
    Inline { tag: String, text: String },
    File(PathBuf),
    Dir(PathBuf),
}

/// Builder for an ingest pass.  Borrows the KB mutably for the
/// duration of `run`; the resulting [`IngestReport`] is the only
/// thing the SDK gives back.
pub struct IngestOp<'a> {
    kb:       &'a mut KnowledgeBase,
    sources:  Vec<Source>,
    progress: Option<Box<dyn ProgressSink>>,
}

impl<'a> IngestOp<'a> {
    /// Start a new ingest pass against `kb`.
    pub fn new(kb: &'a mut KnowledgeBase) -> Self {
        Self {
            kb,
            sources:  Vec::new(),
            progress: None,
        }
    }

    /// Add already-resident KIF text.  Use this for network input,
    /// stdin pipes, in-memory tests — anything where the bytes are
    /// already in your hands.  `tag` is the synthetic file
    /// identifier the ingest pipeline records against the sentences;
    /// re-call `add_source` with the same tag later to apply a
    /// sentence-level diff via reconcile instead of a fresh load.
    pub fn add_source(
        mut self,
        tag:  impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        self.sources.push(Source::Inline { tag: tag.into(), text: text.into() });
        self
    }

    /// Add many resident sources at once.  Equivalent to repeated
    /// [`Self::add_source`] calls.  Useful when reading from a JSON
    /// blob of `{ "tag": "...", "text": "..." }` objects.
    pub fn add_sources<I>(mut self, iter: I) -> Self
    where
        I: IntoIterator<Item = (String, String)>,
    {
        for (tag, text) in iter {
            self.sources.push(Source::Inline { tag, text });
        }
        self
    }

    /// Add a file on disk.  The SDK opens and reads it during
    /// `run()`.  The path's display string is used as the source tag.
    /// Errors at run-time are surfaced via [`SdkError::Io`].
    pub fn add_file<P: Into<PathBuf>>(mut self, path: P) -> Self {
        self.sources.push(Source::File(path.into()));
        self
    }

    /// Add a directory.  The SDK lists it (non-recursively),
    /// filters to `*.kif`, sorts for deterministic order, and reads
    /// each.  Errors at run-time are [`SdkError::DirRead`] for the
    /// listing and [`SdkError::Io`] for individual reads.
    pub fn add_dir<P: Into<PathBuf>>(mut self, path: P) -> Self {
        self.sources.push(Source::Dir(path.into()));
        self
    }

    /// Install a progress sink.  Phase events are delivered on the
    /// calling thread between sources.
    pub fn progress(mut self, sink: Box<dyn ProgressSink>) -> Self {
        self.progress = Some(sink);
        self
    }

    /// Run the ingest.  Returns `Ok(IngestReport)` on success; only
    /// infrastructural failures (file I/O, KB-level bail-outs)
    /// bubble out as `Err`.  Per-source diagnostics (semantic
    /// warnings) ride out in the report — they don't abort.
    ///
    /// **Aborts on parse error or I/O failure.**  Sources processed
    /// before the failure remain ingested into the KB; the caller
    /// decides whether to discard the KB.
    pub fn run(self) -> SdkResult<IngestReport> {
        let IngestOp { kb, sources, mut progress } = self;

        // Expand File / Dir variants into a flat (kind, path) list so
        // we know the true total *before* emitting LoadStarted.
        // Inline entries materialise as resident text up-front; disk
        // entries hold their PathBuf and get read in the next loop
        // (so per-file FileRead events can be ordered correctly).
        let resolved = expand_sources(sources)?;
        let total = resolved.len();

        if let Some(p) = progress.as_deref_mut() {
            p.emit(&ProgressEvent::LoadStarted { total_sources: total });
        }

        let base_session = sumo_kb::session_tags::SESSION_FILES;
        let mut report   = IngestReport::default();

        for (idx, src) in resolved.into_iter().enumerate() {
            let (tag, text) = match src {
                ResolvedSource::Inline { tag, text } => (tag, text),
                ResolvedSource::Disk(path) => {
                    let text = std::fs::read_to_string(&path).map_err(|e| {
                        SdkError::Io { path: path.clone(), source: e }
                    })?;
                    if let Some(p) = progress.as_deref_mut() {
                        p.emit(&ProgressEvent::FileRead {
                            path: path.clone(),
                            idx,
                            total,
                            bytes: text.len(),
                        });
                    }
                    (path.display().to_string(), text)
                }
            };

            let status = ingest_one(kb, &tag, &text, base_session)?;
            if let Some(p) = progress.as_deref_mut() {
                p.emit(&ProgressEvent::SourceIngested {
                    tag:      status.tag.clone(),
                    added:    status.added,
                    removed:  status.removed,
                    retained: status.retained,
                });
            }
            report.total_added    += status.added;
            report.total_removed  += status.removed;
            report.total_retained += status.retained;
            report.sources.push(status);
        }

        // Promote the freshly-loaded session to axiomatic status so
        // subsequent ask / translate calls see the new sentences.
        // This is a no-op if no fresh load was performed.
        kb.make_session_axiomatic(base_session);

        log::info!(
            target: "sumo_sdk::ingest",
            "ingested {} source(s): +{} -{} ={}",
            total,
            report.total_added,
            report.total_removed,
            report.total_retained,
        );
        Ok(report)
    }
}

// ---------------------------------------------------------------------------
// Source expansion (Dir → list of File; File / Inline → identity)
// ---------------------------------------------------------------------------

/// One ingest unit after dir-walking.  Inline has its bytes already;
/// Disk holds a `PathBuf` that the run loop will read.
enum ResolvedSource {
    Inline { tag: String, text: String },
    Disk(PathBuf),
}

fn expand_sources(sources: Vec<Source>) -> SdkResult<Vec<ResolvedSource>> {
    let mut out: Vec<ResolvedSource> = Vec::with_capacity(sources.len());
    for s in sources {
        match s {
            Source::Inline { tag, text } => out.push(ResolvedSource::Inline { tag, text }),
            Source::File(p)              => out.push(ResolvedSource::Disk(p)),
            Source::Dir(d) => {
                for child in scan_dir_for_kif(&d)? {
                    out.push(ResolvedSource::Disk(child));
                }
            }
        }
    }
    Ok(out)
}

/// List `*.kif` files in `dir` (non-recursive), sorted.  Mirrors
/// `cli::util::kif_files_in_dir` but with structured errors.
fn scan_dir_for_kif(dir: &Path) -> SdkResult<Vec<PathBuf>> {
    let entries = std::fs::read_dir(dir).map_err(|e| SdkError::DirRead {
        path:    dir.to_path_buf(),
        message: e.to_string(),
    })?;
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("kif"))
        .collect();
    files.sort();
    Ok(files)
}

// ---------------------------------------------------------------------------
// Per-source ingest dispatch
// ---------------------------------------------------------------------------

/// Ingest a single source.  Picks reconcile vs. fresh-load based on
/// whether the tag is already present in the KB.
fn ingest_one(
    kb:           &mut KnowledgeBase,
    tag:          &str,
    text:         &str,
    base_session: &str,
) -> SdkResult<SourceIngestStatus> {
    if !kb.file_roots(tag).is_empty() {
        // Reconcile path — diff the new text against the existing tag.
        let report = kb.reconcile_file(tag, text);
        if !report.parse_errors.is_empty() {
            let first = report.parse_errors.into_iter().next().unwrap();
            return Err(SdkError::Kb(first));
        }
        for e in &report.semantic_errors {
            log::debug!(
                target: "sumo_sdk::ingest",
                "{}: {}", tag, e
            );
        }
        return Ok(SourceIngestStatus {
            tag:               tag.to_string(),
            added:             report.added(),
            removed:           report.removed(),
            retained:          report.retained,
            semantic_warnings: report.semantic_errors,
            was_reconciled:    true,
        });
    }

    // Fresh-load path — first time we've seen this tag.
    let result = kb.load_kif(text, tag, Some(base_session));
    if !result.ok {
        if let Some(first) = result.errors.into_iter().next() {
            return Err(SdkError::Kb(first));
        }
        return Err(SdkError::Config(format!(
            "load_kif reported failure for tag '{}' but produced no errors",
            tag
        )));
    }
    let added = kb.file_roots(tag).len();
    Ok(SourceIngestStatus {
        tag:               tag.to_string(),
        added,
        removed:           0,
        retained:          0,
        semantic_warnings: Vec::new(),
        was_reconciled:    false,
    })
}
