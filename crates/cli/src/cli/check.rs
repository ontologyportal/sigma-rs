// crates/cli/src/cli/check.rs
//
// `sumo check` — report whether the KB's currently-loaded sources have
// changed since they were last loaded (local mtime/hash, git branch tip).
// Read-only: never reloads anything; `sumo -c load` / `sumo --git ... load`
// do that.

use crate::style::*;
use sigmakee_rs_sdk::{
    check_freshness, check_local_freshness, check_git_tracked, snapshot_git_tracked,
    Freshness, KnowledgeBase, Session, TopLayer, TranslationLayer,
};

/// Run `sumo check` against an opened (read-only) KB. `kb` is `None` when no
/// persisted store exists yet — nothing has ever been loaded, so there's
/// nothing to check.
pub fn run_check(kb: Option<KnowledgeBase<TranslationLayer>>) -> bool {
    let Some(kb) = kb else {
        println!("{color_bright_black}(no database found — nothing has been loaded yet; \
                  run `sumo -c load` or `sumo --git ... load` first){color_reset}");
        return true;
    };

    let session = Session::from_kb(kb, None);
    let mut reports = check_freshness(&session);
    reports.sort_by(|a, b| a.label.cmp(&b.label));

    if reports.is_empty() {
        println!("{color_bright_black}(no tracked sources in this database — local/git \
                  provenance is only recorded going forward from this feature){color_reset}");
        return true;
    }

    let mut notable = 0usize;
    for r in &reports {
        if r.freshness.is_notable() { notable += 1; }
        let (marker, detail) = describe(&r.freshness);
        println!("  {marker} {color_bright_cyan}{}{color_reset}  {detail}", r.label);
    }

    println!();
    if notable == 0 {
        println!("{color_bright_green}all {} tracked source(s) up to date{color_reset}", reports.len());
    } else {
        println!(
            "{color_bright_yellow}{notable} of {} tracked source(s) need attention{color_reset} \
             — reload with `sumo -c load` / `sumo --git ... load` to pick up changes.",
            reports.len(),
        );
    }
    true
}

/// A colored marker plus a human-readable detail line for one verdict.
fn describe(f: &Freshness) -> (String, String) {
    match f {
        Freshness::Unchanged =>
            (format!("{color_bright_green}✓{color_reset}"), "unchanged".to_string()),
        Freshness::Modified =>
            (format!("{color_bright_yellow}●{color_reset}"), "modified on disk since last load".to_string()),
        Freshness::Missing =>
            (format!("{color_bright_red}✗{color_reset}"), "no longer exists on disk".to_string()),
        Freshness::Behind { local_commit, remote_commit } => (
            format!("{color_bright_yellow}●{color_reset}"),
            format!("branch has moved: {} → {}", short(local_commit), short(remote_commit)),
        ),
        Freshness::Unreachable =>
            (format!("{color_bright_black}?{color_reset}"), "could not reach the remote to check".to_string()),
        Freshness::Unknown =>
            (format!("{color_bright_black}?{color_reset}"), "no baseline recorded".to_string()),
    }
}

/// First 8 chars of a commit SHA, for compact display.
fn short(sha: &str) -> &str {
    sha.get(..8).unwrap_or(sha)
}

// Passive notices: run after every command that opens a KB (skipped for
// `load`/`check`, which already report freshness explicitly). Local is a
// synchronous stat/hash pass — no network, cheap enough to run inline. Git
// needs a network round trip, so it follows the same cached +
// background-refresh pattern as `maybe_notify_update` (see `update.rs`): a
// per-command notice only ever reads a cache file (instant); a stale/missing
// cache kicks off a detached background thread that refreshes it for the
// next invocation.

/// Cheap "some local sources changed on disk" notice — a `stat()`/hash per
/// tracked local file, no network. Call after ingest, once per command.
pub fn maybe_notify_stale_local<L: TopLayer>(session: &Session<L>) {
    let reports = check_local_freshness(session);
    let notable = reports.iter().filter(|r| r.freshness.is_notable()).count();
    if notable > 0 {
        eprintln!(
            "{color_bright_yellow}info:{color_reset} {notable} of {} loaded source(s) changed \
             on disk since last load — run `sumo check` for details, `sumo -c load` to refresh.",
            reports.len(),
        );
    }
}

const GIT_CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

#[derive(serde::Serialize, serde::Deserialize)]
struct GitCheckCache {
    checked_at: u64,
    stale:      usize,
    total:      usize,
}

fn git_cache_path() -> Option<std::path::PathBuf> {
    Some(crate::config::home_dir()?.join(".sigmakee").join("git-freshness-check.json"))
}

fn read_git_cache(path: &std::path::Path) -> Option<GitCheckCache> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn write_git_cache(path: &std::path::Path, cache: &GitCheckCache) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string(cache) {
        let _ = std::fs::write(path, text);
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Cached "some git-tracked sources may be behind their upstream branch"
/// notice. Reading the cache is instant (no network); a stale/missing cache
/// triggers a background refresh for next time. A no-op when there's
/// nothing git-tracked to check (`check_git_tracked` degrades to `Unknown`
/// for every entry if the SDK's own `git` feature happens to be off, so this
/// never needs its own feature gate).
///
/// Convergence is opportunistic, not per-command: this is spawned *after*
/// opening the KB, and opening a large persisted store can itself take
/// longer than the network round-trip this needs — a command that does
/// little after that (e.g. `search`) may `process::exit` before the thread
/// finishes, same as `maybe_notify_update`'s fast-command case. A command
/// with real tail work after ingest (`ask`, `test`, …) reliably gives it
/// enough time; the cache just catches up on whichever command happens to.
pub fn maybe_notify_stale_git<L: TopLayer>(session: &Session<L>) {
    let Some(path) = git_cache_path() else { return };
    let cached = read_git_cache(&path);

    if let Some(c) = &cached {
        if c.stale > 0 {
            eprintln!(
                "{color_bright_yellow}info:{color_reset} {} of {} git-tracked source(s) may be \
                 behind their upstream branch — run `sumo check` for details, \
                 `sumo --git <url> --branch <name> load` to refresh.",
                c.stale, c.total,
            );
        }
    }

    let stale = cached
        .map(|c| now_secs().saturating_sub(c.checked_at) > GIT_CHECK_INTERVAL_SECS)
        .unwrap_or(true);
    if !stale {
        return;
    }

    // Snapshot synchronously (cheap KB introspection, no network) so the
    // background thread never needs to touch the KB/session at all.
    let snapshot = snapshot_git_tracked(session);
    if snapshot.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        let reports = check_git_tracked(&snapshot);
        let stale = reports.iter().filter(|r| r.freshness.is_notable()).count();
        write_git_cache(&path, &GitCheckCache { checked_at: now_secs(), stale, total: reports.len() });
    });
}
