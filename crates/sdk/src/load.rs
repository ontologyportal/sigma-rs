//! Persist KIF sources to an LMDB-backed [`KnowledgeBase`].
//!
//! [`LoadOp`] is the SDK counterpart of `sumo load` — the only
//! operation that *writes* to the database.  Unlike [`crate::IngestOp`]
//! (which only stages sentences in memory), `LoadOp::run` walks the
//! per-source reconcile pipeline AND commits each diff to LMDB via
//! `kb.persist_reconcile_diff`.
//!
//! # Caller's responsibility
//!
//! - **Open the KB.**  Caller calls `KnowledgeBase::open(db_path)`
//!   before driving `LoadOp` — the SDK does not own the LMDB path.
//!   `KnowledgeBase::open` creates the directory if it doesn't exist.
//! - **Wipe-on-flush is on the caller.**  If you want the CLI's
//!   `--flush` semantics (drop the entire DB and rebuild from
//!   scratch), `fs::remove_dir_all(db_path)` then re-open the KB
//!   *before* calling [`LoadOp::new`].  Reconcile against an empty
//!   KB then writes everything fresh.  The SDK does not do this for
//!   you because it never touches the LMDB filesystem layout.
//!
//! # Strict vs. permissive
//!
//! By default `LoadOp` is **strict**: any semantic error found during
//! reconcile aborts the LMDB commit.  The in-memory KB state is
//! already updated by reconcile; the caller should discard the KB
//! (drop the value) if they want to revert.  Re-opening the KB from
//! disk gets back to the pre-load state.
//!
//! With [`LoadOp::strict`] set to `false` the commit happens
//! regardless; semantic errors ride out in the report for the
//! consumer to surface.  Useful for LSP-on-save flows where you want
//! to publish diagnostics without blocking the save.
//!
//! # Example
//!
//! ```no_run
//! use sigmakee_rs_sdk::LoadOp;
//!
//! // Caller owns the KB and the path:
//! let mut kb = sigmakee_rs_core::KnowledgeBase::open("./sumo.kb".as_ref()).unwrap();
//!
//! let report = LoadOp::new(&mut kb)
//!     .add_dir("ontology/")
//!     .run()
//!     .unwrap();
//!
//! if report.is_clean() {
//!     println!("committed +{} -{}", report.total_added, report.total_removed);
//! } else if !report.committed {
//!     eprintln!("aborted: {} semantic error(s)", report.semantic_errors.len());
//! }
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use sigmakee_rs_core::KnowledgeBase;

use crate::error::{SdkError, SdkResult};
use sigmakee_rs_core::{ProgressEvent, ProgressSink};
use crate::report::load::{LoadFileStatus, LoadReport};

/// Internal source representation — same shape as `IngestOp`'s.
enum Source {
    File(PathBuf),
    Dir(PathBuf),
    Inline { tag: String, text: String },
}

/// Builder for a load + commit pass.
pub struct LoadOp<'a> {
    kb:       &'a mut KnowledgeBase,
    sources:  Vec<Source>,
    strict:   bool,
    progress: Option<Box<dyn ProgressSink>>,
}

impl<'a> LoadOp<'a> {
    /// Start a new load against `kb`.  The KB must already be opened
    /// against an LMDB path; loading into an in-memory KB will
    /// reconcile in-memory but commit nothing (LMDB calls are no-ops
    /// when no DB is attached).
    pub fn new(kb: &'a mut KnowledgeBase) -> Self {
        Self {
            kb,
            sources:  Vec::new(),
            strict:   true,
            progress: None,
        }
    }

    /// Add a single `.kif` file.  SDK reads it during `run()`.  The
    /// path's display string is used as the source tag — same tag is
    /// what reconcile keys off when this load is repeated later.
    pub fn add_file<P: Into<PathBuf>>(mut self, p: P) -> Self {
        self.sources.push(Source::File(p.into()));
        self
    }

    /// Add a directory of `.kif` files.  SDK enumerates
    /// (non-recursive), filters to `*.kif`, sorts.
    pub fn add_dir<P: Into<PathBuf>>(mut self, p: P) -> Self {
        self.sources.push(Source::Dir(p.into()));
        self
    }

    /// Add already-resident KIF text under a synthetic tag.
    pub fn add_source(
        mut self,
        tag:  impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        self.sources.push(Source::Inline { tag: tag.into(), text: text.into() });
        self
    }

    /// Add many resident sources.  Same shape as
    /// [`crate::IngestOp::add_sources`].
    pub fn add_sources<I>(mut self, iter: I) -> Self
    where
        I: IntoIterator<Item = (String, String)>,
    {
        for (tag, text) in iter {
            self.sources.push(Source::Inline { tag, text });
        }
        self
    }

    /// Strict mode toggle.  Default is `true`: any semantic error
    /// blocks the LMDB commit.  With `false`, the commit happens
    /// regardless and semantic errors ride out in the report.
    pub fn strict(mut self, yes: bool) -> Self {
        self.strict = yes;
        self
    }

    /// Install a progress sink.
    pub fn progress(mut self, sink: Box<dyn ProgressSink>) -> Self {
        self.progress = Some(sink);
        self
    }

    /// Run the load + commit pipeline.
    ///
    /// Phases:
    /// 1. Expand `add_dir` entries into individual file paths.
    /// 2. Read every disk source up-front.
    /// 3. Pass `(tag, text)` slice to `kb.reconcile_files(...)` for
    ///    a single batched reconcile pass.
    /// 4. Walk the per-source reports.  Parse errors abort with
    ///    `Err(SdkError::Kb)`; semantic errors accumulate.
    /// 5. If `strict` and any semantic errors were collected,
    ///    return `Ok(report)` with `committed: false`.
    /// 6. Otherwise commit each per-file diff via
    ///    `kb.persist_reconcile_diff`.  A mid-batch persist failure
    ///    surfaces as `Err(SdkError::Persist)` — earlier files are
    ///    already on disk; reconcile is idempotent so the next run
    ///    completes cleanly.
    pub fn run(self) -> SdkResult<LoadReport> {
        let LoadOp { kb, sources, strict, mut progress } = self;

        // Phase 1: expand File / Dir / Inline → flat list of
        // (tag, text) pairs.  Disk reads happen here.
        let materialised = materialise_sources(sources, &mut progress)?;
        let total = materialised.len();

        let mut report = LoadReport::default();

        if materialised.is_empty() {
            // No sources — nothing to reconcile, nothing to commit.
            // Return a clean report with `committed: false` to make
            // the no-op explicit (a caller checking `is_clean()` on
            // an empty load gets `false`, matching expectations).
            return Ok(report);
        }

        if let Some(p) = progress.as_deref_mut() {
            p.emit(&ProgressEvent::LoadStarted { total_sources: total });
        }

        // Phase 2: batched reconcile.  Single bulk-rebuild of SInE +
        // taxonomy at the end; ~10x faster than per-file reconcile
        // on bootstrap-scale loads.
        let reports = kb.reconcile_files(
            materialised.iter().map(|(tag, text)| (tag.as_str(), text.as_str())),
        );

        // Phase 3: walk per-file results.  Parse errors abort
        // immediately (the in-memory KB is already mutated for the
        // files reconciled before this point — the caller decides
        // whether to keep it).
        let mut pending: Vec<(String, Vec<sigmakee_rs_core::SentenceId>, Vec<sigmakee_rs_core::SentenceId>)> =
            Vec::with_capacity(reports.len());
        for r in reports {
            let tag = r.file.clone();

            if !r.parse_errors.is_empty() {
                // First parse error wins — report.parse_errors is
                // already a Vec<KbError>, so wrap directly.
                let first = r.parse_errors.into_iter().next().unwrap();
                return Err(SdkError::Kb(first));
            }

            for e in &r.semantic_errors {
                report.semantic_errors.push((tag.clone(), e.clone()));
            }

            if let Some(p) = progress.as_deref_mut() {
                p.emit(&ProgressEvent::SourceIngested {
                    tag:      tag.clone(),
                    added:    r.added(),
                    removed:  r.removed(),
                    retained: r.retained,
                });
            }

            report.total_added    += r.added();
            report.total_removed  += r.removed();
            report.total_retained += r.retained;
            report.files.push(LoadFileStatus {
                tag:               tag.clone(),
                added:             r.added(),
                removed:           r.removed(),
                retained:          r.retained,
                semantic_warnings: r.semantic_errors,
            });
            pending.push((tag, r.removed_sids, r.added_sids));
        }

        // Phase 4: strict gate.
        if strict && !report.semantic_errors.is_empty() {
            log::warn!(
                target: "sigmakee_rs_sdk::load",
                "load aborted (strict): {} semantic error(s); DB not modified",
                report.semantic_errors.len()
            );
            return Ok(report);
        }

        // Phase 5: commit.  Per-file LMDB transactions so a mid-batch
        // failure leaves earlier files committed and recoverable on
        // next run via reconcile's idempotence.
        if let Some(p) = progress.as_deref_mut() {
            p.emit(&ProgressEvent::PromoteStarted {
                session: sigmakee_rs_core::session_tags::SESSION_LOAD.to_string(),
            });
        }
        let t_promote = Instant::now();
        for (tag, removed_sids, added_sids) in &pending {
            kb.persist_reconcile_diff(removed_sids, added_sids).map_err(|e| {
                log::error!(target: "sigmakee_rs_sdk::load",
                    "commit failed for {}: {}", tag, e);
                SdkError::Kb(e)
            })?;
        }
        if let Some(p) = progress.as_deref_mut() {
            p.emit(&ProgressEvent::PromoteFinished {
                promoted:   report.total_added,
                duplicates: 0, // reconcile path doesn't dedupe at commit
                elapsed:    t_promote.elapsed(),
            });
        }

        report.committed = true;
        log::info!(
            target: "sigmakee_rs_sdk::load",
            "committed {} file(s): +{} -{} ={}",
            report.files.len(),
            report.total_added,
            report.total_removed,
            report.total_retained,
        );
        Ok(report)
    }
}

// ---------------------------------------------------------------------------
// Helpers (mirrors IngestOp's expansion, but materialises text since
// reconcile_files needs the slice all at once).
// ---------------------------------------------------------------------------

fn materialise_sources(
    sources:  Vec<Source>,
    progress: &mut Option<Box<dyn ProgressSink>>,
) -> SdkResult<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = Vec::with_capacity(sources.len());

    // Pre-compute total disk-source count for FileRead progress.
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut inline: Vec<(String, String)> = Vec::new();
    for s in sources {
        match s {
            Source::Inline { tag, text } => inline.push((tag, text)),
            Source::File(p)              => paths.push(p),
            Source::Dir(d)               => paths.extend(scan_dir_for_kif(&d)?),
        }
    }
    let total_disk = paths.len();

    for (idx, path) in paths.into_iter().enumerate() {
        let text = std::fs::read_to_string(&path).map_err(|e| SdkError::Io {
            path:   path.clone(),
            source: e,
        })?;
        if let Some(p) = progress.as_deref_mut() {
            p.emit(&ProgressEvent::FileRead {
                path:  path.clone(),
                idx,
                total: total_disk,
                bytes: text.len(),
            });
        }
        out.push((path.display().to_string(), text));
    }
    out.extend(inline);
    Ok(out)
}

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
