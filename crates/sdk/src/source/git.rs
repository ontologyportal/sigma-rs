// crates/cli/src/git.rs
//
// Sparse git fetch for --git flag, using git2 (no system git required).

use std::fs;
use std::path::{Path, PathBuf};

use git2::{FetchOptions, ObjectType, RemoteCallbacks, Repository, Tree};
// use indicatif::{ProgressBar, ProgressStyle};
use tempfile::TempDir;

use crate::SdkError;

/// Fetch `url` at depth=1 into a fresh temporary directory, then write
/// only the paths listed in `sparse_paths` to the working tree.
///
/// git2 / libgit2 does not support `--filter=blob:none`, so all blobs
/// at HEAD are transferred.  The sparse selection happens after the
/// fetch: only requested files and directories are written to disk.
///
/// `sparse_paths` are repo-relative paths (e.g. `"Merge.kif"`,
/// `"KBs/"`) exactly as the user supplied them.
pub fn fetch_repo_sparse(url: &str, sparse_paths: &[String]) -> Result<(TempDir, PathBuf), SdkError> {
    let tmp = TempDir::new().map_err(|e| {
        SdkError::TempDir(PathBuf::new(), e)
    })?;
    let dest = tmp.path();

    let repo = Repository::init(dest).map_err(|e| {
        SdkError::Git(e)
    })?;

    let mut remote = repo.remote("origin", url).map_err(|e| {
        SdkError::Git(e)
    })?;

    let callbacks = RemoteCallbacks::new();
    // callbacks.transfer_progress(move |stats| {
    //     bar2.set_length(stats.total_objects() as u64);
    //     bar2.set_position(stats.received_objects() as u64);
    //     true
    // });

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
            SdkError::Git(e)
        })?;

    // bar.finish_and_clear();

    let reference = repo
        .find_reference("refs/remotes/origin/HEAD")
        .map_err(|e| {
            SdkError::Git(e)
        })?;
    let commit = reference.peel_to_commit().map_err(|e| {
        SdkError::Git(e)
    })?;
    let tree = commit.tree().map_err(|e| {
        SdkError::Git(e)
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
) -> Result<(), SdkError> {
    for path_str in paths {
        // Strip trailing slash so tree.get_path() finds directory entries.
        let path = Path::new(path_str.trim_end_matches('/'));
        match tree.get_path(path) {
            Ok(entry) => match entry.kind() {
                Some(ObjectType::Blob) => {
                    let blob = repo.find_blob(entry.id()).map_err(|e| {
                        SdkError::Git(e)
                    })?;
                    let out = dest.join(path);
                    if let Some(parent) = out.parent() {
                        fs::create_dir_all(parent).map_err(|e| {
                            SdkError::TempDir(parent.into(), e)
                        })?;
                    }
                    fs::write(&out, blob.content()).map_err(|e| {
                        SdkError::TempDir(out.into(), e)
                    })?;
                }
                Some(ObjectType::Tree) => {
                    let subtree = repo.find_tree(entry.id()).map_err(|e| {
                        SdkError::Git(e)
                    })?;
                    extract_tree(repo, &subtree, &dest.join(path))?;
                }
                _ => {
                    // log::warn!("'{}' is neither a file nor a directory — skipped", path_str);
                }
            },
            Err(_) => {
                // log::warn!("'{}' not found in repository — skipped", path_str);
            }
        }
    }
    Ok(())
}

/// Recursively write all blobs in `tree` under `dest`.
fn extract_tree(repo: &Repository, tree: &Tree, dest: &Path) -> Result<(), SdkError> {
    fs::create_dir_all(dest).map_err(|e| {
        SdkError::TempDir(dest.into(), e)
    })?;
    for entry in tree.iter() {
        let name = match entry.name() {
            Ok(n) => n,
            Err(_) => continue,
        };
        match entry.kind() {
            Some(ObjectType::Blob) => {
                let blob = repo.find_blob(entry.id()).map_err(|e| {
                    SdkError::Git(e)
                })?;
                fs::write(dest.join(name), blob.content()).map_err(|e| {
                    SdkError::TempDir(dest.join(name).into(), e)
                })?;
            }
            Some(ObjectType::Tree) => {
                let subtree = repo.find_tree(entry.id()).map_err(|e| {
                    SdkError::Git(e)
                })?;
                extract_tree(repo, &subtree, &dest.join(name))?;
            }
            _ => {}
        }
    }
    Ok(())
}
