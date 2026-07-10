// Fallback provider for the compile-time SUMO_* / SINE_* symbol names.
//
// These are normally injected by the workspace's `.cargo/config.toml`
// `[env]` table, which cargo only loads when the build is invoked from
// inside (a checkout of) the workspace.  `cargo install --git …` builds
// from a fresh clone using the *invoker's* config, so the `[env]` table
// never applies and every `env!(…)` in semantics/consts.rs fails.
//
// This script re-derives the same values from the repo's own config file
// (found by walking up from the crate dir, so it works both in the
// workspace and in a cargo-install clone) and emits them as rustc-env.
// Variables already present in the environment are skipped, preserving
// the normal precedence: real environment > `[env]` table > this fallback.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR").expect("cargo always sets CARGO_MANIFEST_DIR"),
    );
    let mut dir = manifest_dir.as_path();
    let config = loop {
        let candidate = dir.join(".cargo").join("config.toml");
        if candidate.is_file() {
            break Some(candidate);
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break None,
        }
    };
    // No config found (e.g. a source tarball without `.cargo/`): emit
    // nothing and let the missing-env compile error surface with its
    // variable name, which is the clearest available diagnostic.
    let Some(config) = config else { return };
    println!("cargo:rerun-if-changed={}", config.display());

    let Ok(text) = fs::read_to_string(&config) else { return };

    // Minimal parse of the `[env]` table: the file is repo-controlled and
    // holds only `KEY = "value"` lines there (see the workspace
    // .cargo/config.toml), so a full TOML dependency is not warranted.
    let mut in_env = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_env = line == "[env]";
            continue;
        }
        if !in_env || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let key = key.trim();
        let value = value.trim().trim_matches('"');
        println!("cargo:rerun-if-env-changed={}", key);
        if env::var_os(key).is_none() {
            println!("cargo:rustc-env={}={}", key, value);
        }
    }
}
