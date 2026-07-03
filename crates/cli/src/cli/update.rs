//! `sumo update` — refresh the running CLI to the latest GitHub release.
//!
//! Dispatches on the compile-time `SUMO_BUILD_KIND` provenance flag:
//!
//!   * release: query the GitHub Releases API, compare to the running version,
//!     download the asset for this platform+arch, and replace the running binary
//!     in place via the `self_update` crate.
//!   * source: report the upstream version and recommend a rebuild rather than
//!     overwriting the developer's local build.
//!
//! `--check` queries upstream and reports without modifying anything in either flow.

use crate::style::*;

/// GitHub owner of the official `sumo` release stream. Hard-coded so a fork
/// cannot be substituted as an update source.
const REPO_OWNER: &str = "ontologyportal";
/// GitHub repo name of the official `sumo` release stream.
const REPO_NAME:  &str = "sigma-rs";

/// Tag prefix used by the release workflow. Tags look like `sigmakee-vX.Y.Z`;
/// `self_update` strips the prefix automatically.
const TAG_PREFIX: &str = "sigmakee-v";

/// Compile-time provenance set by `build.rs`: `"release"` or `"source"`.
const BUILD_KIND: &str = env!("SUMO_BUILD_KIND");

/// Compile-time short git commit SHA; `"unknown"` when git was unavailable.
const BUILD_COMMIT: &str = env!("SUMO_BUILD_COMMIT");

/// Compile-time Rust target triple (e.g. `aarch64-apple-darwin`), used to pick
/// the right release archive when self-updating.
const BUILD_TARGET: &str = env!("SUMO_BUILD_TARGET");

/// CLI version from `Cargo.toml`, compared against the release tag's version.
const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Map the embedded Rust target triple to the npm-style label used in the
/// release-asset names. Returns `None` for targets with no prebuilt binary.
fn target_to_release_label(target: &str) -> Option<&'static str> {
    match target {
        "aarch64-apple-darwin"      => Some("darwin-arm64"),
        "x86_64-apple-darwin"       => Some("darwin-x64"),
        "x86_64-unknown-linux-gnu"  => Some("linux-x64-gnu"),
        "aarch64-unknown-linux-gnu" => Some("linux-arm64-gnu"),
        "x86_64-pc-windows-msvc"    => Some("win32-x64"),
        _                           => None,
    }
}

/// Run the `update` subcommand.
///
/// `check_only`: don't modify anything; just print whether a newer version
/// exists upstream. Honoured regardless of build kind. Returns `true` on
/// success (including a successful check or an up-to-date binary).
pub fn run_update(check_only: bool) -> bool {
    let label = target_to_release_label(BUILD_TARGET);
    let label_str = label.unwrap_or("unsupported");
    println!(
        "{style_bold}sumo update{style_reset}\n  \
         current version : {}\n  \
         build kind      : {}\n  \
         build commit    : {}\n  \
         build target    : {}\n  \
         release label   : {}\n",
        CLI_VERSION, BUILD_KIND, BUILD_COMMIT, BUILD_TARGET, label_str,
    );

    let latest = match query_latest_release() {
        Ok(l)  => l,
        Err(e) => {
            log::error!("update: could not query GitHub releases: {}", e);
            return false;
        }
    };

    println!("  latest release  : {}\n", latest.version);

    if !is_newer(&latest.version, CLI_VERSION) {
        println!("{color_bright_green}You're on the latest version.{color_reset}");
        return true;
    }

    if check_only {
        println!(
            "{color_bright_yellow}A newer version is available: {} → {}{color_reset}",
            CLI_VERSION, latest.version,
        );
        return true;
    }

    match BUILD_KIND {
        "release" => apply_release_update(&latest.version),
        "source"  => recommend_rebuild(&latest.version),
        other     => {
            log::error!(
                "update: unknown SUMO_BUILD_KIND `{}` embedded at compile time \
                 — this is a build-script bug",
                other
            );
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Release flow: download + replace
// ---------------------------------------------------------------------------

fn apply_release_update(latest: &str) -> bool {
    println!(
        "{color_bright_cyan}Updating from {} to {}…{color_reset}",
        CLI_VERSION, latest,
    );

    let label = match target_to_release_label(BUILD_TARGET) {
        Some(l) => l,
        None => {
            log::error!(
                "update: no prebuilt binary published for target `{}`.\n\
                 Either build from source (`cargo install --path crates/cli`) \
                 or open an issue to request a release for this platform.",
                BUILD_TARGET,
            );
            return false;
        }
    };

    let result = self_update::backends::github::Update::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name("sumo")
        .target(label)
        .identifier(TAG_PREFIX)
        .show_download_progress(true)
        .show_output(true)
        .current_version(CLI_VERSION)
        .target_version_tag(&format!("{}{}", TAG_PREFIX, latest))
        .build();

    let updater = match result {
        Ok(u)  => u,
        Err(e) => {
            log::error!("update: could not configure self-updater: {}", e);
            return false;
        }
    };

    match updater.update() {
        Ok(self_update::Status::UpToDate(v)) => {
            println!("{color_bright_green}Already at {}{color_reset}", v);
            true
        }
        Ok(self_update::Status::Updated(v)) => {
            println!(
                "{color_bright_green}Updated successfully → {}.  \
                 Restart any running `sumo` processes to use the new binary.{color_reset}",
                v
            );
            true
        }
        Err(e) => {
            log::error!("update: download/replace failed: {}", e);
            log::error!("update: your existing binary is unchanged");
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Source flow: refuse to overwrite, recommend a rebuild
// ---------------------------------------------------------------------------

fn recommend_rebuild(latest: &str) -> bool {
    println!(
        "{color_bright_yellow}This binary was built from source.{color_reset}\n\
         Self-update is disabled to avoid overwriting your local build with \n\
         an unrelated upstream binary.\n"
    );
    println!(
        "  {style_bold}A newer version is available: {} → {}{style_reset}\n",
        CLI_VERSION, latest,
    );
    println!("  To update from source, pull the latest commits and rebuild:");
    println!();
    println!("    {color_bright_cyan}git pull --recurse-submodules{color_reset}");
    println!("    {color_bright_cyan}cargo install --path crates/cli --features ask,integrated-prover{color_reset}");
    println!();
    println!(
        "  Or, if you'd rather use an official prebuilt binary instead, \
         download one from:"
    );
    println!(
        "    {color_bright_cyan}https://github.com/{}/{}/releases/latest{color_reset}",
        REPO_OWNER, REPO_NAME,
    );

    true
}

// ---------------------------------------------------------------------------
// Upstream query helpers
// ---------------------------------------------------------------------------

/// Snapshot of the upstream "latest release" info.
struct LatestRelease {
    /// Version string with the `sigmakee-v` tag prefix stripped.
    version: String,
}

fn query_latest_release() -> Result<LatestRelease, String> {
    let list = self_update::backends::github::ReleaseList::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .build()
        .map_err(|e| format!("failed to configure release list: {}", e))?
        .fetch()
        .map_err(|e| format!("failed to fetch release list: {}", e))?;

    let latest = list
        .iter()
        .filter(|r| r.version.starts_with(TAG_PREFIX) || !r.version.is_empty())
        .max_by(|a, b| cmp_semver(&a.version, &b.version))
        .ok_or_else(|| "no releases found".to_string())?;

    // Strip the `sigmakee-v` (or bare `v`) prefix for display and comparison.
    let stripped = latest
        .version
        .strip_prefix(TAG_PREFIX)
        .unwrap_or(&latest.version)
        .trim_start_matches('v')
        .to_string();

    Ok(LatestRelease { version: stripped })
}

/// Best-effort semver comparison of `a` vs. `b`. Falls back to lexical
/// comparison if either side doesn't parse as `MAJOR.MINOR.PATCH`.
fn cmp_semver(a: &str, b: &str) -> std::cmp::Ordering {
    fn parts(v: &str) -> Option<(u32, u32, u32)> {
        let v = v.trim_start_matches(TAG_PREFIX).trim_start_matches('v');
        let mut it = v.split('.').take(3);
        Some((
            it.next()?.parse().ok()?,
            it.next()?.parse().ok()?,
            // PATCH may have a `-rc.N` suffix; cut at the first non-digit.
            it.next()?
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .ok()?,
        ))
    }
    match (parts(a), parts(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        _                  => a.cmp(b),
    }
}

/// `true` iff `latest > current` under [`cmp_semver`].
fn is_newer(latest: &str, current: &str) -> bool {
    cmp_semver(latest, current) == std::cmp::Ordering::Greater
}

// ---------------------------------------------------------------------------
// Tests — purely the version-comparison logic.  Network-dependent
// paths (release fetch, self-replace) are exercised manually.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmp_semver_basic() {
        assert_eq!(cmp_semver("1.0.0", "1.0.0"), std::cmp::Ordering::Equal);
        assert_eq!(cmp_semver("1.0.1", "1.0.0"), std::cmp::Ordering::Greater);
        assert_eq!(cmp_semver("0.9.9", "1.0.0"), std::cmp::Ordering::Less);
        assert_eq!(cmp_semver("2.0.0", "1.99.99"), std::cmp::Ordering::Greater);
    }

    #[test]
    fn cmp_semver_strips_tag_and_v_prefix() {
        assert_eq!(cmp_semver("sigmakee-v1.2.3", "1.2.3"), std::cmp::Ordering::Equal);
        assert_eq!(cmp_semver("v1.2.4", "v1.2.3"), std::cmp::Ordering::Greater);
        assert_eq!(cmp_semver("sigmakee-v2.0.0", "v1.99.99"), std::cmp::Ordering::Greater);
    }

    #[test]
    fn cmp_semver_handles_rc_suffix() {
        // Prerelease info is ignored: `-rc.N` compares equal to the final.
        assert_eq!(cmp_semver("1.2.3-rc.1", "1.2.3"), std::cmp::Ordering::Equal);
        assert_eq!(cmp_semver("1.2.4-rc.1", "1.2.3"), std::cmp::Ordering::Greater);
    }

    #[test]
    fn cmp_semver_falls_back_to_lex_on_parse_failure() {
        // Garbage in → lexical comparison, never panics.
        assert_eq!(cmp_semver("not-a-version", "1.0.0"), "not-a-version".cmp("1.0.0"));
    }

    #[test]
    fn is_newer_works_both_directions() {
        assert!(is_newer("1.2.4", "1.2.3"));
        assert!(!is_newer("1.2.3", "1.2.4"));
        assert!(!is_newer("1.2.3", "1.2.3"));
    }

    #[test]
    fn target_to_release_label_covers_every_release_matrix_entry() {
        // The labels here MUST match the `matrix.label` values in
        // `.github/workflows/release-sumo.yml`.  Update both lists
        // when adding a new release platform.
        assert_eq!(target_to_release_label("aarch64-apple-darwin"),     Some("darwin-arm64"));
        assert_eq!(target_to_release_label("x86_64-apple-darwin"),      Some("darwin-x64"));
        assert_eq!(target_to_release_label("x86_64-unknown-linux-gnu"), Some("linux-x64-gnu"));
        assert_eq!(target_to_release_label("aarch64-unknown-linux-gnu"),Some("linux-arm64-gnu"));
        assert_eq!(target_to_release_label("x86_64-pc-windows-msvc"),   Some("win32-x64"));
    }

    #[test]
    fn target_to_release_label_returns_none_for_unsupported() {
        assert_eq!(target_to_release_label("x86_64-unknown-linux-musl"), None);
        assert_eq!(target_to_release_label("riscv64gc-unknown-linux-gnu"), None);
        assert_eq!(target_to_release_label(""), None);
    }
}
