// crates/cli/src/git.rs
//
// Sparse git fetch for --git flag, using git2 (no system git required).

use std::fs;
use std::path::{Path, PathBuf};

use git2::{FetchOptions, ObjectType, RemoteCallbacks, Repository, Tree};
use indicatif::{ProgressBar, ProgressStyle};
use tempfile::TempDir;

/// Fetch `url` at depth=1 into a fresh temporary directory, then write
/// only the paths listed in `sparse_paths` to the working tree.
///
/// git2 / libgit2 does not support `--filter=blob:none`, so all blobs
/// at HEAD are transferred.  The sparse selection happens after the
/// fetch: only requested files and directories are written to disk.
///
/// `sparse_paths` are repo-relative paths (e.g. `"Merge.kif"`,
/// `"KBs/"`) exactly as the user supplied them.
pub fn fetch_repo_sparse(url: &str, sparse_paths: &[String]) -> Result<(TempDir, PathBuf), ()> {
    let tmp = TempDir::new().map_err(|e| {
        log::error!("failed to create temporary directory: {}", e);
    })?;
    let dest = tmp.path();

    let bar = ProgressBar::new(0);
    bar.set_style(
        ProgressStyle::with_template(
            "{spinner:.cyan} Fetching {pos}/{len} objects  ({bytes_per_sec})",
        )
        .unwrap(),
    );

    let repo = Repository::init(dest).map_err(|e| {
        log::error!("failed to initialise git repository: {}", e);
    })?;

    let mut remote = repo.remote("origin", url).map_err(|e| {
        log::error!("failed to add remote '{}': {}", url, e);
    })?;

    let bar2 = bar.clone();
    let mut callbacks = RemoteCallbacks::new();
    callbacks.transfer_progress(move |stats| {
        bar2.set_length(stats.total_objects() as u64);
        bar2.set_position(stats.received_objects() as u64);
        true
    });

    let mut fetch_opts = FetchOptions::new();
    fetch_opts.remote_callbacks(callbacks);
    fetch_opts.depth(1);

    // Fetch HEAD into a stable local ref so we can peel it to a commit.
    remote
        .fetch(
            &["HEAD:refs/remotes/origin/HEAD"],
            Some(&mut fetch_opts),
            None,
        )
        .map_err(|e| {
            log::error!("failed to fetch '{}': {}", url, e);
        })?;

    bar.finish_and_clear();

    let reference = repo
        .find_reference("refs/remotes/origin/HEAD")
        .map_err(|e| {
            log::error!("could not resolve fetched HEAD: {}", e);
        })?;
    let commit = reference.peel_to_commit().map_err(|e| {
        log::error!("could not peel to commit: {}", e);
    })?;
    let tree = commit.tree().map_err(|e| {
        log::error!("could not read commit tree: {}", e);
    })?;

    checkout_paths(&repo, &tree, dest, sparse_paths)?;

    let root = dest.to_path_buf();
    Ok((tmp, root))
}

/// Write each requested path from the git tree to `dest`.
///
/// Files are written as-is; directories are recursively extracted.
/// Paths not found in the tree produce a warning and are skipped.
fn checkout_paths(
    repo: &Repository,
    tree: &Tree,
    dest: &Path,
    paths: &[String],
) -> Result<(), ()> {
    for path_str in paths {
        // Strip trailing slash so tree.get_path() finds directory entries.
        let path = Path::new(path_str.trim_end_matches('/'));
        match tree.get_path(path) {
            Ok(entry) => match entry.kind() {
                Some(ObjectType::Blob) => {
                    let blob = repo.find_blob(entry.id()).map_err(|e| {
                        log::error!("failed to read blob for '{}': {}", path_str, e);
                    })?;
                    let out = dest.join(path);
                    if let Some(parent) = out.parent() {
                        fs::create_dir_all(parent).map_err(|e| {
                            log::error!("failed to create directory '{}': {}", parent.display(), e);
                        })?;
                    }
                    fs::write(&out, blob.content()).map_err(|e| {
                        log::error!("failed to write '{}': {}", out.display(), e);
                    })?;
                }
                Some(ObjectType::Tree) => {
                    let subtree = repo.find_tree(entry.id()).map_err(|e| {
                        log::error!("failed to read tree for '{}': {}", path_str, e);
                    })?;
                    extract_tree(repo, &subtree, &dest.join(path))?;
                }
                _ => {
                    log::warn!("'{}' is neither a file nor a directory — skipped", path_str);
                }
            },
            Err(_) => {
                log::warn!("'{}' not found in repository — skipped", path_str);
            }
        }
    }
    Ok(())
}

/// Recursively write all blobs in `tree` under `dest`.
fn extract_tree(repo: &Repository, tree: &Tree, dest: &Path) -> Result<(), ()> {
    fs::create_dir_all(dest).map_err(|e| {
        log::error!("failed to create directory '{}': {}", dest.display(), e);
    })?;
    for entry in tree.iter() {
        let name = match entry.name() {
            Some(n) => n,
            None => continue,
        };
        match entry.kind() {
            Some(ObjectType::Blob) => {
                let blob = repo.find_blob(entry.id()).map_err(|e| {
                    log::error!("failed to read blob '{}': {}", name, e);
                })?;
                fs::write(dest.join(name), blob.content()).map_err(|e| {
                    log::error!("failed to write '{}': {}", name, e);
                })?;
            }
            Some(ObjectType::Tree) => {
                let subtree = repo.find_tree(entry.id()).map_err(|e| {
                    log::error!("failed to read subtree '{}': {}", name, e);
                })?;
                extract_tree(repo, &subtree, &dest.join(name))?;
            }
            _ => {}
        }
    }
    Ok(())
}
