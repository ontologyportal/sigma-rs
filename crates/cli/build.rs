// Build script for the `sigmakee` CLI.
//
// Embeds compile-time provenance metadata that the `sumo update`
// subcommand reads at runtime to decide between two upgrade flows:
//
//   - `release` :  binary was built by the official release CI
//                  workflow (which sets `SUMO_BUILD_KIND=release`
//                  before invoking `cargo build`).  `sumo update`
//                  self-replaces from the latest GitHub release.
//
//   - `source`  :  binary was built by a contributor / from source
//                  (no env var set).  `sumo update` prints a
//                  "you built this; run `git pull` and rebuild"
//                  message rather than touching the binary.
//
// The detection is deliberately simple — a compile-time env var
// check.  More elaborate schemes (signature verification, hash
// matching against a manifest) would add cryptographic complexity
// without changing the threat model: a tampered binary can always
// lie about its provenance.  This is for ergonomics, not security.

use std::env;

fn main() {
    // The release CI exports SUMO_BUILD_KIND=release before
    // invoking `cargo build`.  Everything else (developer machines,
    // CI test runs, `cargo install --path .`) leaves it unset and
    // we default to "source".
    let kind = env::var("SUMO_BUILD_KIND").unwrap_or_else(|_| "source".to_string());

    // Validate to a known-good set so a typo (e.g. SUMO_BUILD_KIND=relesae)
    // surfaces as a build error instead of a bad runtime branch.
    let kind = match kind.as_str() {
        "release" | "source" => kind,
        other => panic!(
            "SUMO_BUILD_KIND has unexpected value `{}`; expected `release` or `source`",
            other
        ),
    };

    // Embed for `env!("SUMO_BUILD_KIND")` at lib-build time.
    println!("cargo:rustc-env=SUMO_BUILD_KIND={}", kind);

    // Trigger a rebuild whenever the env var changes (e.g. switching
    // from a local `cargo build` to a CI run on the same machine).
    println!("cargo:rerun-if-env-changed=SUMO_BUILD_KIND");

    // Also embed the git commit SHA when available.  Useful for
    // bug-report templates and for the `update` command's "you have
    // X, latest is Y" output.  Best-effort: a release archive built
    // outside the repo (e.g. from a tarball) just gets "unknown".
    let commit = env::var("SUMO_BUILD_COMMIT")
        .ok()
        .or_else(|| {
            std::process::Command::new("git")
                .args(["rev-parse", "--short=12", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=SUMO_BUILD_COMMIT={}", commit);
    println!("cargo:rerun-if-env-changed=SUMO_BUILD_COMMIT");
    println!("cargo:rerun-if-changed=../../.git/HEAD");

    // Embed the Rust target triple this binary was built for.  Used
    // by `--version` for diagnostic output and by `sumo update` to
    // map onto the release-asset label naming convention.  Cargo
    // exposes the active target as `TARGET` to every build script;
    // this is always set, never an Option.
    let target = env::var("TARGET")
        .expect("Cargo always sets TARGET for build scripts");
    println!("cargo:rustc-env=SUMO_BUILD_TARGET={}", target);
}
