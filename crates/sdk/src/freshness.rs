// crates/sdk/src/freshness.rs
//
// Compare a KB's recorded source provenance (see `KnowledgeBase::file_origin`)
// against each loaded source's *current* state — "has this changed since I
// loaded it." Purely a function of what's already persisted in the KB: no
// `-f`/`-d`/`-c`/`--git`/`--branch` need to be re-supplied to run this, since
// `GitProvenance` carries its own repo URI. Local checks are a `stat()`/hash
// (cheap, always run); the git check queries the remote's ref advertisement
// (network — one round-trip per distinct repo+branch; see
// `source::remote_branch_head`).

use std::collections::HashMap;
use std::path::Path;

use sigmakee_rs_core::{FileOrigin, TopLayer};

use crate::Session;

/// Freshness verdict for one loaded source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Freshness {
    /// Matches the recorded provenance exactly.
    Unchanged,
    /// The file's on-disk mtime/hash no longer matches what was recorded.
    Modified,
    /// The file that produced this source no longer exists on disk.
    Missing,
    /// A git-fetched source's branch tip has moved past the recorded commit.
    Behind { local_commit: String, remote_commit: String },
    /// The remote couldn't be reached to check (network error) — distinct
    /// from [`Freshness::Unknown`] so a caller doesn't conflate "offline"
    /// with "never loaded."
    Unreachable,
    /// No baseline was ever recorded for this source (never loaded, or
    /// loaded before this feature existed) — nothing to compare against.
    Unknown,
}

impl Freshness {
    /// Whether this verdict is worth surfacing to a user (i.e. not simply
    /// "matches what was recorded").
    pub fn is_notable(&self) -> bool {
        !matches!(self, Freshness::Unchanged)
    }
}

/// One source's freshness result, labeled for display.
#[derive(Debug, Clone)]
pub struct FreshnessReport {
    /// The file key as recorded in the KB (a path — absolute for a local
    /// source, repo-relative for a git one).
    pub label: String,
    pub freshness: Freshness,
}

/// Check every file the KB has recorded provenance for against its current
/// state. Walks [`KnowledgeBase::iter_files`](sigmakee_rs_core::KnowledgeBase::iter_files);
/// a file with no recorded [`FileOrigin`] (never loaded through a path that
/// tracks provenance — e.g. `--tell`) is skipped rather than reported
/// [`Freshness::Unknown`], since it was never a candidate for tracking.
///
/// Combines [`check_local_freshness`] (cheap — a `stat()`/hash, no network)
/// and [`check_git_freshness`] (a remote round-trip per distinct repo+branch).
/// A caller that wants to avoid ever blocking on the network (e.g. a passive
/// per-command notice) should call `check_local_freshness` alone and handle
/// the git check separately (cached / backgrounded).
pub fn check_freshness<L: TopLayer>(session: &Session<L>) -> Vec<FreshnessReport> {
    let mut out = check_local_freshness(session);
    out.extend(check_git_freshness(session));
    out
}

/// The local-file half of [`check_freshness`]: a `stat()`/hash per tracked
/// local file, no network. Cheap enough to run on every command that opens a
/// persisted KB, not just an explicit `sumo check`.
pub fn check_local_freshness<L: TopLayer>(session: &Session<L>) -> Vec<FreshnessReport> {
    let kb = session.kb();
    kb.iter_files().into_iter()
        .filter(|file| matches!(kb.file_origin(file), Some(FileOrigin::Local(_))))
        .map(|file| check_local(kb, &file))
        .collect()
}

/// The git-source half of [`check_freshness`]: one remote ref-advertisement
/// round-trip per distinct (repo, branch) — network, so a caller that must
/// stay non-blocking should run this on a background thread (see
/// `sigmakee`'s `maybe_notify_update` for the same pattern applied to the
/// CLI's own version check).
pub fn check_git_freshness<L: TopLayer>(session: &Session<L>) -> Vec<FreshnessReport> {
    check_git_tracked(&snapshot_git_tracked(session))
}

/// Every git-tracked file in the KB, grouped by (uri, branch) → `(file_key,
/// recorded_commit)` pairs — no network, just KB introspection. Fully owned
/// (no borrow of `session`/`kb`), so it's safe to move into a background
/// thread that runs [`check_git_tracked`] without holding the KB open across
/// the network round-trip — the pattern a passive per-command notice wants
/// (snapshot synchronously and cheaply, check on a detached thread, never
/// block the current command on the network).
pub fn snapshot_git_tracked<L: TopLayer>(session: &Session<L>) -> HashMap<(String, String), Vec<(String, String)>> {
    let kb = session.kb();
    let mut out: HashMap<(String, String), Vec<(String, String)>> = HashMap::new();
    for file in kb.iter_files() {
        if let Some(FileOrigin::Git(prov)) = kb.file_origin(&file) {
            out.entry((prov.uri, prov.branch)).or_default().push((file, prov.commit));
        }
    }
    out
}

/// Check a [`snapshot_git_tracked`] snapshot against the live remote — the
/// network half, deliberately taking no KB/session reference so it can run
/// on a background thread.
#[cfg(feature = "git")]
pub fn check_git_tracked(snapshot: &HashMap<(String, String), Vec<(String, String)>>) -> Vec<FreshnessReport> {
    let mut out = Vec::new();
    for ((uri, branch), files) in snapshot {
        let remote = crate::source::remote_branch_head(uri, Some(branch));
        for (file, local_commit) in files {
            let freshness = match &remote {
                Err(_) => Freshness::Unreachable,
                Ok(head) if &head.commit == local_commit => Freshness::Unchanged,
                Ok(head) => Freshness::Behind {
                    local_commit:  local_commit.clone(),
                    remote_commit: head.commit.clone(),
                },
            };
            out.push(FreshnessReport { label: file.clone(), freshness });
        }
    }
    out
}

#[cfg(not(feature = "git"))]
pub fn check_git_tracked(snapshot: &HashMap<(String, String), Vec<(String, String)>>) -> Vec<FreshnessReport> {
    snapshot.values().flatten()
        .map(|(file, _)| FreshnessReport { label: file.clone(), freshness: Freshness::Unknown })
        .collect()
}

fn check_local<L: TopLayer>(kb: &sigmakee_rs_core::KnowledgeBase<L>, file: &str) -> FreshnessReport {
    let label = file.to_string();
    let path = Path::new(file);

    if !path.exists() {
        return FreshnessReport { label, freshness: Freshness::Missing };
    }
    let Some(FileOrigin::Local(recorded)) = kb.file_origin(file) else {
        return FreshnessReport { label, freshness: Freshness::Unknown };
    };

    // mtime is the cheap first check; only fall back to hashing (a full
    // read) when it disagrees, so an unrelated `touch` doesn't get reported
    // as a real change.
    let mtime_matches = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .is_some_and(|d| d.as_secs() == recorded.mtime_secs);
    if mtime_matches {
        return FreshnessReport { label, freshness: Freshness::Unchanged };
    }
    match std::fs::read(path) {
        Ok(bytes) if sigmakee_rs_core::hash_file_contents(&bytes) == recorded.content_hash =>
            FreshnessReport { label, freshness: Freshness::Unchanged },
        Ok(_)  => FreshnessReport { label, freshness: Freshness::Modified },
        Err(_) => FreshnessReport { label, freshness: Freshness::Missing },
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Session, Source};
    use sigmakee_rs_core::TranslationLayer;

    fn tmp_kif(name: &str, contents: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("sdk-freshness-{name}-{}.kif", std::process::id()));
        std::fs::write(&p, contents).unwrap();
        p
    }

    #[test]
    fn unchanged_file_reports_unchanged() {
        let p = tmp_kif("unchanged", "(subclass Dog Animal)");
        let mut session = Session::<TranslationLayer>::new("s".into());
        assert!(session.ingest(Source::Local(vec![p.clone()]), false).is_empty());

        let reports = check_freshness(&session);
        let r = reports.iter().find(|r| r.label == p.to_string_lossy()).expect("file tracked");
        assert_eq!(r.freshness, Freshness::Unchanged);

        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn edited_file_reports_modified() {
        let p = tmp_kif("modified", "(subclass Dog Animal)");
        let mut session = Session::<TranslationLayer>::new("s".into());
        assert!(session.ingest(Source::Local(vec![p.clone()]), false).is_empty());

        // Backdate the recorded mtime so a same-second edit still trips the
        // mtime check (this filesystem's mtime resolution may be coarser
        // than the test's wall-clock delta).
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&p, "(subclass Cat Animal)").unwrap();

        let reports = check_freshness(&session);
        let r = reports.iter().find(|r| r.label == p.to_string_lossy()).expect("file tracked");
        assert_eq!(r.freshness, Freshness::Modified);

        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn deleted_file_reports_missing() {
        let p = tmp_kif("missing", "(subclass Dog Animal)");
        let mut session = Session::<TranslationLayer>::new("s".into());
        assert!(session.ingest(Source::Local(vec![p.clone()]), false).is_empty());
        std::fs::remove_file(&p).unwrap();

        let reports = check_freshness(&session);
        let r = reports.iter().find(|r| r.label == p.to_string_lossy()).expect("file tracked");
        assert_eq!(r.freshness, Freshness::Missing);
    }
}
