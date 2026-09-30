// crates/sdk/src/manager/lexicon.rs
//
// Resolve a KBManager's `lexicon` config into a loaded WordNet, best-effort.

use std::path::{Path, PathBuf};

use super::{resolve_constituent, KBManager, LexiconSource};
use crate::lexicon::WordNet;

/// SigmaKEE convention: the mapping files' subdirectory when a `<lexicon>`
/// source names a base location (a repo checkout, a URL base, or -- only
/// when [`KBManager::lexicon`] has no explicit `source` at all -- `kbDir`
/// itself) rather than the mapping directory directly.
const DEFAULT_SUBDIR: &str = "WordNetMappings";

impl KBManager {
    /// Load the WordNet lexicon per [`KBManager::lexicon`] /
    /// [`KBManager::load_lexicons`], best-effort: any failure (missing
    /// directory, unreachable URL, git fetch error, or a source kind whose
    /// feature isn't compiled in) degrades to `None` rather than
    /// propagating
    pub fn load_lexicon(&self) -> Option<WordNet> {
        if !self.load_lexicons {
            return None;
        }
        let cfg = &self.lexicon;
        let result = match &cfg.source {
            // No explicit source: the SigmaKEE default local layout,
            // `<kbDir>/<directory-or-"WordNetMappings">`, resolved like a
            // constituent path (absolute as-is, else relative to kbDir,
            // itself relative to baseDir).
            None | Some(LexiconSource::Local { .. }) => {
                crate::lexicon::load_dir_with(&self.local_lexicon_dir()?, cfg)
            }
            #[cfg(feature = "http")]
            Some(LexiconSource::Http { url }) => {
                let base = match cfg.directory.as_deref() {
                    Some(sub) => format!("{}/{}", url.trim_end_matches('/'), sub.trim_matches('/')),
                    None => url.clone(),
                };
                crate::lexicon::load_http_with(&base, cfg)
            }
            #[cfg(not(feature = "http"))]
            Some(LexiconSource::Http { .. }) => {
                log::debug!(
                    "wordnet: <lexicon> names an http source but this build has no `http` feature"
                );
                return None;
            }
            #[cfg(feature = "git")]
            Some(LexiconSource::Git { url, branch }) => {
                crate::lexicon::load_git_with(url, branch.as_deref(), cfg.directory.as_deref(), cfg)
            }
            #[cfg(not(feature = "git"))]
            Some(LexiconSource::Git { .. }) => {
                log::debug!(
                    "wordnet: <lexicon> names a git source but this build has no `git` feature"
                );
                return None;
            }
        };
        match result {
            Ok(wn) => Some(wn),
            Err(e) => {
                log::debug!("wordnet: not loaded ({e})");
                None
            }
        }
    }

    /// The local mapping directory a `None` or `Local` `<lexicon>` source
    /// names; `None` for remote sources.
    ///
    /// No explicit source is the SigmaKEE default layout,
    /// `<kbDir>/<directory-or-"WordNetMappings">`, resolved like a
    /// constituent path (absolute as-is, else relative to kbDir, itself
    /// relative to baseDir).  An explicit local `path` already names the
    /// mapping directory itself (matching `load_dir`'s convention) unless
    /// `directory` narrows further into a subpath of it.
    fn local_lexicon_dir(&self) -> Option<PathBuf> {
        let cfg = &self.lexicon;
        match &cfg.source {
            None => {
                let dir_name = cfg.directory.as_deref().unwrap_or(DEFAULT_SUBDIR);
                Some(resolve_constituent(
                    Path::new(dir_name),
                    &self.base_dir,
                    &self.kb_dir,
                ))
            }
            Some(LexiconSource::Local { path }) => Some(match cfg.directory.as_deref() {
                Some(sub) => path.join(sub),
                None => path.clone(),
            }),
            Some(_) => None,
        }
    }

    /// Load the `FirstNames.csv` proper-name list from the same local
    /// directory [`KBManager::load_lexicon`] reads the mappings from,
    /// best-effort: a remote source, a disabled lexicon, or a missing file
    /// degrades to `None`.
    pub fn load_first_names(&self) -> Option<Vec<String>> {
        if !self.load_lexicons {
            return None;
        }
        let dir = self.local_lexicon_dir()?;
        match crate::lexicon::load_first_names(&dir) {
            Ok(names) => Some(names),
            Err(e) => {
                log::debug!("wordnet: first names not loaded ({e})");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::LexiconConfig;

    #[test]
    fn first_names_resolve_to_the_lexicon_dir() {
        let dir = std::env::temp_dir().join(format!("sigma-first-names-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("FirstNames.csv"),
            "\"name\",\"gender\"\n\"Zeke\",\"M\"\n",
        )
        .unwrap();
        let m = KBManager {
            lexicon: LexiconConfig {
                source: Some(LexiconSource::Local { path: dir.clone() }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(m.load_first_names(), Some(vec!["Zeke".to_string()]));
        std::fs::remove_dir_all(&dir).unwrap();

        let http = KBManager {
            lexicon: LexiconConfig {
                source: Some(LexiconSource::Http {
                    url: "http://example.invalid".into(),
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(http.load_first_names(), None);
    }

    #[test]
    fn load_lexicons_false_never_attempts_anything() {
        // Even a source that would resolve to nowhere real must not be
        // attempted -- confirmed by simply getting `None` back, not by an
        // absence of an error (there would be none to observe either way).
        let m = KBManager {
            load_lexicons: false,
            lexicon: LexiconConfig {
                source: Some(LexiconSource::Local {
                    path: "/does/not/exist".into(),
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(m.load_lexicon().is_none());
    }

    #[test]
    fn missing_local_directory_degrades_to_none() {
        let m = KBManager {
            kb_dir: "/definitely/not/a/real/path/xyz".into(),
            ..Default::default()
        };
        assert!(
            m.load_lexicon().is_none(),
            "a missing WordNetMappings directory must degrade silently, not panic or error"
        );
    }

    /// Real local WordNetMappings directory, addressed two ways: as the
    /// zero-config default (`kbDir` alone) and as an explicit `Local`
    /// source whose `path` already names the mapping directory (matching
    /// `load_dir`'s convention -- `directory` left unset must NOT append
    /// another `WordNetMappings` and look one level too deep).
    fn real_wordnet_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(std::env::var_os("HOME").expect("HOME set"))
            .join("projects/sumo/WordNetMappings")
    }

    #[test]
    #[ignore = "requires a local WordNetMappings directory"]
    fn zero_config_default_loads_from_kb_dir() {
        let kb_dir = real_wordnet_dir()
            .parent()
            .expect("WordNetMappings has a parent")
            .to_path_buf();
        let m = KBManager {
            kb_dir,
            ..Default::default()
        };
        let wn = m.load_lexicon().expect("zero-config default load failed");
        assert!(wn.len() > 100_000, "only {} synsets parsed", wn.len());
    }

    #[test]
    #[ignore = "requires a local WordNetMappings directory"]
    fn explicit_local_source_path_is_the_mapping_dir_itself() {
        let m = KBManager {
            lexicon: LexiconConfig {
                source: Some(LexiconSource::Local {
                    path: real_wordnet_dir(),
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        let wn = m.load_lexicon().expect("explicit local source load failed");
        assert!(wn.len() > 100_000, "only {} synsets parsed", wn.len());
        assert_eq!(wn.senses("dog")[0].synset.sumo[0].term, "DomesticDog");
    }

    #[cfg(feature = "git")]
    #[test]
    #[ignore = "network: sparse-clones ontologyportal/sumo over git"]
    fn git_source_uses_the_default_wordnetmappings_subdir() {
        let m = KBManager {
            lexicon: LexiconConfig {
                source: Some(LexiconSource::Git {
                    url: "https://github.com/ontologyportal/sumo".into(),
                    branch: Some("master".into()),
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        let wn = m.load_lexicon().expect("git source load failed");
        assert!(wn.len() > 100_000, "only {} synsets parsed", wn.len());
    }
}
