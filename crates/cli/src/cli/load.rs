use std::fs;

use log;
use sigmakee_rs_sdk::{HasTranslation, TopLayer};
use sigmakee_rs_sdk::manager::KBManager;
use sigmakee_rs_sdk::{Session};

/// Entry point for `sumo load`.
///
/// The only command that writes to the LMDB database. This command 
/// writes the current state of the KB to the LMDB persistence backend
/// by the point that `run_load` is called, the backend should have 
/// already been flushed.
/// 
/// This variant is for layers which perform TPTP translation. It will
/// warm the translation cache and persist that
pub fn run_load_warm<L>(mut session: Session<L>, manager: KBManager) -> bool
where L: HasTranslation {
    if let Err(e) = session.translate(manager.into()) {
        log::error!("Error warming session translation cache: {}", e);
        log::error!("Aborting KB loading");
        false
    } else if let Err(err) = session.persist() {
        log::error!("Failed to save KB to disk: {}", err);
        false
    } else {
        true
    }
}

/// Entry point for `sumo load`.
///
/// The only command that writes to the LMDB database. This command 
/// writes the current state of the KB to the LMDB persistence backend
/// by the point that `run_load` is called, the backend should have 
/// already been flushed.
/// 
/// This variant is for layers which do not have translation capacity. 
/// No cache warming is necessary
pub fn run_load<L>(session: Session<L>, _manager: KBManager) -> bool where L : TopLayer {
    if let Err(err) = session.persist() {
        log::error!("Failed to save KB to disk: {}", err);
        false
    } else {
        true
    }
}

/// `--flush` path: drop the DB directory entirely, then rebuild from
/// the supplied files.  With no files, the result is an empty
/// initialised database at `kb_args.db`.
pub fn run_flush(manager: &KBManager) -> bool {
    // Wipe the DB directory if it exists.  `remove_dir_all` is
    // atomic per-inode on all supported filesystems; if the path
    // doesn't exist we just fall through to the create path.
    if let Some(kb_path) = manager.db_path() {
        if kb_path.exists() {
            if let Err(e) = fs::remove_dir_all(&kb_path) {
                log::error!(
                    "load --flush: failed to wipe '{}': {}",
                    kb_path.display(),
                    e
                );
                return false;
            }
            log::info!(target: "sigmakee_rs_core::load",
                "load --flush: wiped '{}'", kb_path.display());    
        }
    }
    true
}

