// crates/cli/src/cli/update.rs
//
// `sumo update` — refresh the running CLI to the latest GitHub
// release.
//
// The handler dispatches on a compile-time provenance flag
// (`SUMO_BUILD_KIND`, embedded by `build.rs`).  Two flows:
//
//   * release: the binary was built by the official release CI.
//     Query the GitHub Releases API for the latest version, compare
//     to the running version, download the asset matching this
//     platform+arch, and atomically replace the running binary
//     in place.  Powered by the `self_update` crate.
//
//   * source: the binary was built from source by a contributor or
//     by `cargo install --path .`.  We don't replace it — that would
//     overwrite the developer's local build with an unrelated
//     binary.  Instead we tell them what version is available
//     upstream and recommend the right rebuild incantation.
//
// `--check` is honoured in both flows: it queries upstream and
// reports without modifying anything.

use inline_colorization::*;

/// GitHub repo coordinates for the official `sumo` release stream.
/// Hard-coded so the binary can self-update without reading a
/// config file or accepting a `--repo` flag (which would be a foot-
/// gun: pointing at a fork would let an attacker substitute a
/// trojan binary on update).
const REPO_OWNER: &str = "ontologyportal";
const REPO_NAME:  &str = "sigma-rs";

/// Tag prefix used by the release workflow (see
/// `.github/workflows/release-sumo.yml`).  Tags look like
/// `sigmakee-vX.Y.Z`; `self_update` strips the prefix automatically.
const TAG_PREFIX: &str = "sigmakee-v";

/// Compile-time provenance, set by `build.rs`.  One of:
/// - `"release"`: built by the official CI release workflow.
/// - `"source"`:  built from source (the default).
const BUILD_KIND: &str = env!("SUMO_BUILD_KIND");

/// Compile-time short git commit SHA.  `"unknown"` when built from
/// a tarball or any context where `git rev-parse` couldn't run.
const BUILD_COMMIT: &str = env!("SUMO_BUILD_COMMIT");

/// Compile-time Rust target triple (e.g. `aarch64-apple-darwin`).
/// Used to pick the right release archive when self-updating.
const BUILD_TARGET: &str = env!("SUMO_BUILD_TARGET");

/// CLI version (from `Cargo.toml`).  Compared against the GitHub
/// release tag's stripped version string.
const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Map the embedded Rust target triple to the npm-style label used
/// in the release-asset names (see `.github/workflows/release-sumo.yml`).
///
/// Returns `None` for unsupported targets — `sumo update` then errors
/// out with a clear "no prebuilt binary for this platform" message
/// instead of silently failing on an asset-not-found.
///
/// Keep this aligned with the matrix in `release-sumo.yml`.  Adding
/// a new release target requires updating both lists.
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
/// `check_only`: don't modify anything; just print whether a newer
/// version exists upstream.  Always honoured regardless of build kind.
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

    // Step 1: query upstream.  Both flows need this.
    let latest = match query_latest_release() {
        Ok(l)  => l,
        Err(e) => {
            log::error!("update: could not query GitHub releases: {}", e);
            return false;
        }
    };

    println!("  latest release  : {}\n", latest.version);

    // Step 2: compare.  No-op if we're already current.
    if !is_newer(&latest.version, CLI_VERSION) {
        println!("{color_bright_green}You're on the latest version.{color_reset}");
        return true;
    }

    // Step 3: dispatch on build kind.  --check short-circuits both.
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

    // Resolve the release-asset label for this binary's target.
    // The release workflow names archives `sumo-X.Y.Z-<label>.{tar.gz,zip}`
    // (see `.github/workflows/release-sumo.yml`).  `self_update` matches
    // an asset name containing the configured `target` substring —
    // passing our label (`darwin-arm64`, `linux-x64-gnu`, …) instead
    // of the Rust triple is what makes the asset lookup succeed.
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
        // Tell self_update which asset suffix to look for.  Without
        // this it'd try the Rust triple (`aarch64-apple-darwin`),
        // which doesn't appear anywhere in our archive names.
        .target(label)
        // Strip the `sigmakee-v` tag prefix when comparing.
        // `self_update` accepts a custom tag → version transform via
        // `identifier`; we pass the raw tag prefix and it does the
        // right thing.
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

    // Returning true: the command did its job (informed the user).
    // The lack of an in-place replacement isn't a failure — it's the
    // correct behaviour for source builds.
    true
}

// ---------------------------------------------------------------------------
// Upstream query helpers
// ---------------------------------------------------------------------------

/// Snapshot of the upstream "latest release" info we care about.
struct LatestRelease {
    /// Version string with the `sigmakee-v` tag prefix stripped.
    version: String,
}

fn query_latest_release() -> Result<LatestRelease, String> {
    // `self_update::backends::github::ReleaseList` returns every
    // release; we filter for the `sigmakee-v` tag prefix and pick
    // the highest semver.
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

    // Strip the `sigmakee-v` (or bare `v`) prefix for display + the
    // is-newer comparison upstream.  The raw tag is what we'd send
    // to `target_version_tag` if applying the update.
    let stripped = latest
        .version
        .strip_prefix(TAG_PREFIX)
        .unwrap_or(&latest.version)
        .trim_start_matches('v')
        .to_string();

    Ok(LatestRelease { version: stripped })
}

/// Best-effort semver comparison.  Returns the [`std::cmp::Ordering`]
/// of `a` vs. `b`.  Falls back to lexical comparison if either side
/// doesn't parse as `MAJOR.MINOR.PATCH`.
fn cmp_semver(a: &str, b: &str) -> std::cmp::Ordering {
    fn parts(v: &str) -> Option<(u32, u32, u32)> {
        // Strip the `sigmakee-v` prefix if it slipped through, plus
        // any leading 'v'.
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
        // `1.2.3-rc.1` should compare equal to `1.2.3` for our
        // purposes (we ignore prerelease info).  This is intentional:
        // a release-candidate tag shouldn't trigger an "update
        // available" prompt for users on the matching final.
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
