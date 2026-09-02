//! Loading the WordNet lexicon from real sources.
//!
//! The core module (`sigmakee_rs_core::lexicon`, re-exported here in full)
//! is pure in-memory parsing -- [`WordNet::from_texts`] over already-read
//! strings, wasm-safe.  Knowledge of the SigmaKEE `WordNetMappings/`
//! directory layout and the I/O to obtain it live at this SDK seam, the
//! same place the SDK owns constituent-file resolution:
//!
//!   - [`load_dir`]  -- a local `WordNetMappings/` directory;
//!   - [`load_http`] -- a base URL serving the same layout (feature `http`),
//!     e.g. `https://raw.githubusercontent.com/ontologyportal/sumo/master/WordNetMappings`;
//!   - [`load_git`]  -- a repository containing the directory (feature
//!     `git`), fetched shallow the same way `--git` constituent loads are.
//!
//! All three run the same [`load_with`] assembly: the four
//! `WordNetMappings30-*.txt` files are required (any failure aborts), while
//! `index.sense` (most-frequent-sense ordering) and `noun.exc`/`verb.exc`
//! (irregular inflections) are optional and skipped on any fetch failure.

pub use sigmakee_rs_core::lexicon::*;

use std::path::Path;

use crate::{SdkError, SdkResult};

/// The four required mapping files, in `(file name, pos)` order.
const MAPPING_FILES: &[(&str, Pos)] = &[
    (env!("WORDNET_NOUN_FILE"), Pos::Noun),
    (env!("WORDNET_VERB_FILE"), Pos::Verb),
    (env!("WORDNET_ADJV_FILE"), Pos::Adj),
    (env!("WORDNET_ADVR_FILE"), Pos::Adv),
];

/// The optional companions: sense ordering, then the two irregular
/// inflection tables (concatenated into one exception list).
const INDEX_SENSE: &str = env!("WORDNET_SENSE_IDX");
const EXC_FILES: &[&str] = &["noun.exc", "verb.exc"];

/// Assemble a [`WordNet`] from any per-file-name text fetcher.  Required
/// mapping files propagate the fetcher's error; the optional companions are
/// skipped on failure.  Every loader below is this function plus a fetcher.
fn load_with(mut fetch: impl FnMut(&str) -> SdkResult<String>) -> SdkResult<WordNet> {
    let mut texts = Vec::with_capacity(MAPPING_FILES.len());
    for (name, pos) in MAPPING_FILES {
        texts.push((fetch(name)?, *pos));
    }
    let index_sense = fetch(INDEX_SENSE).ok();
    let mut exc = String::new();
    for name in EXC_FILES {
        if let Ok(t) = fetch(name) {
            exc.push_str(&t);
            exc.push('\n');
        }
    }
    Ok(WordNet::from_texts(
        texts.iter().map(|(t, p)| (t.as_str(), *p)),
        index_sense.as_deref(),
        if exc.is_empty() {
            None
        } else {
            Some(exc.as_str())
        },
    ))
}

/// Load a [`WordNet`] from a local `WordNetMappings` directory (SigmaKEE
/// convention: `<kbDir>/WordNetMappings/`).
///
/// Each file is read fully into memory (~30 MB transient across the
/// required four) and handed to [`WordNet::from_texts`]; the raw text is
/// dropped once the index is built.
pub fn load_dir(dir: &Path) -> SdkResult<WordNet> {
    load_with(|name| {
        let path = dir.join(name);
        std::fs::read_to_string(&path).map_err(|e| SdkError::Io { path, source: e })
    })
}

/// Load a [`WordNet`] over HTTP from a base URL serving the
/// `WordNetMappings/` layout.
/// A failed fetch of an optional companion (404 on `index.sense`, say)
/// degrades exactly like its absence from a local directory.
#[cfg(feature = "http")]
pub fn load_http(base_url: &str) -> SdkResult<WordNet> {
    let base = base_url.trim_end_matches('/');
    load_with(|name| fetch_text(&format!("{base}/{name}")))
}

/// One HTTP GET -> body text.  The default ureq body cap is 10 MiB and the
/// noun mappings file alone is ~16 MB, so the limit is raised explicitly.
#[cfg(feature = "http")]
fn fetch_text(url: &str) -> SdkResult<String> {
    const BODY_LIMIT: u64 = 64 * 1024 * 1024;
    let uri: ureq::http::Uri = url
        .parse()
        .map_err(|e| SdkError::Http(format!("{url}: {e}")))?;
    let mut resp = ureq::get(uri)
        .call()
        .map_err(|e| SdkError::Http(format!("{url}: {e}")))?;
    resp.body_mut()
        .with_config()
        .limit(BODY_LIMIT)
        .read_to_string()
        .map_err(|e| SdkError::Http(format!("{url}: {e}")))
}

/// Load the [`WsdIndex`] (SigmaKEE's `wordFrequencies_combined.txt`
/// context-co-occurrence data) from a `WordNetMappings` directory.  A
/// missing file is an error here; callers that treat WSD as an optional
/// refinement (the CLI does) map it to `None`.
pub fn load_wsd(dir: &Path) -> SdkResult<WsdIndex> {
    let path = dir.join(env!("WORDNET_WSD_FILE"));
    let text = std::fs::read_to_string(&path).map_err(|e| SdkError::Io { path, source: e })?;
    Ok(WsdIndex::from_text(&text))
}

/// Load a [`WordNet`] from a git repository containing a `WordNetMappings`
/// directory (`subdir` overrides that default path, e.g. for a repo that
/// keeps it elsewhere).  The repository is fetched shallow into a tempdir
/// with only `subdir` checked out then read like a local directory. `branch:
/// None` follows the remote's default branch.
#[cfg(feature = "git")]
pub fn load_git(url: &str, branch: Option<&str>, subdir: Option<&str>) -> SdkResult<WordNet> {
    let subdir = subdir.unwrap_or(option_env!("WORDNET_GIT_SUBDIR").unwrap_or("/")).trim_matches('/');
    let (_checkout_guard, dir, provenance) =
        crate::source::fetch_repo_sparse(url, &[subdir.to_string()], branch)?;
    log::debug!(
        "wordnet: fetched {url} @ {} ({})",
        provenance.branch,
        provenance.commit
    );
    load_dir(&dir.join(subdir))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assertions shared by every loader's smoke test -- the same checks
    /// regardless of where the bytes came from.
    fn assert_real_wordnet(wn: &WordNet) {
        // WordNet 3.0: ~117k synsets across the four POS files.
        assert!(wn.len() > 100_000, "only {} synsets parsed", wn.len());
        let dog = wn.senses("dog");
        assert!(!dog.is_empty(), "'dog' has no senses");
        assert_eq!(
            dog[0].label(),
            "dog#n#1",
            "MFS ordering broken: {:?}",
            dog.iter().map(Sense::label).collect::<Vec<_>>()
        );
        let car = wn.senses("car");
        assert!(
            car.iter()
                .any(|s| s.synset.sumo.iter().any(|a| a.term == "Automobile")),
            "'car' does not reach Automobile: {:?}",
            car.iter().flat_map(|s| &s.synset.sumo).collect::<Vec<_>>()
        );
        assert!(
            !wn.senses("children").is_empty(),
            "exception table not applied"
        );
    }

    /// Smoke test against a real local WordNetMappings directory. Defaults
    /// to `~/projects/sumo/WordNetMappings` (this repo's dev-machine
    /// convention); override with `SIGMA_WN_DIR=<dir>`. Ignored by default
    /// since it depends on a local checkout of the SUMO distribution; run
    /// with `cargo test -p sigmakee-rs-sdk --lib lexicon -- --ignored`.
    #[test]
    #[ignore = "requires a local WordNetMappings directory"]
    fn real_mappings_smoke() {
        let dir = std::env::var_os("SIGMA_WN_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|| dirs_home().map(|h| h.join("projects/sumo/WordNetMappings")))
            .expect("no SIGMA_WN_DIR and no home directory to default from");
        let wn = load_dir(&dir).expect("load_dir failed");
        assert_real_wordnet(&wn);
        assert_eq!(wn.senses("dog")[0].synset.sumo[0].term, "DomesticDog");
        assert!(!wn.synsets_of_term("Canine").is_empty());
    }

    fn dirs_home() -> Option<std::path::PathBuf> {
        std::env::var_os("HOME").map(std::path::PathBuf::from)
    }

    #[cfg(feature = "http")]
    const RAW_WN_BASE: &str =
        "https://raw.githubusercontent.com/ontologyportal/sumo/master/WordNetMappings";
    #[cfg(feature = "git")]
    const SUMO_REPO: &str = "https://github.com/ontologyportal/sumo";

    #[cfg(feature = "http")]
    #[test]
    #[ignore = "network: fetches ~30 MB of WordNet mappings from raw.githubusercontent.com"]
    fn http_loads_wordnet_mappings() {
        let wn = load_http(RAW_WN_BASE).expect("http load should succeed");
        assert_real_wordnet(&wn);
    }

    #[cfg(feature = "git")]
    #[test]
    #[ignore = "network: sparse-clones ontologyportal/sumo over git (transfers HEAD blobs)"]
    fn git_loads_wordnet_mappings() {
        let wn = load_git(SUMO_REPO, Some("master"), None).expect("git load should succeed");
        assert_real_wordnet(&wn);
    }
}
