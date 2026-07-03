// crates/sdk/src/source/mod.rs
//
// Module dictating how KB sources are read from

use std::{io::Read, path::{Path, PathBuf}};

use sigmakee_rs_core::{DynSink, FileOrigin, ProgressEvent, SourceFile};
use sigmakee_rs_core::Parser;
#[cfg(feature = "http")]
use ureq::http::Uri;

use crate::{SdkError, SdkResult};

#[cfg(feature = "git")]
mod git;
mod tptp;

/// A source to ingest.  Local file or arbitrary reader for now; remote URLs
/// (`http(s)`/`git`) arrive behind the `remote` feature.
pub enum Source {
    /// A local filesystem path — the SDK opens and reads it.
    Local(Vec<PathBuf>),
    /// An already-open byte stream (e.g. stdin); `name` drives format detection.
    Reader { name: String, reader: Box<dyn Read> },
    /// A remote source URI
    #[cfg(feature = "http")]
    Http { uri: Uri },
    /// A git repository
    #[cfg(feature = "git")]
    Git { uri: String, paths: Vec<PathBuf> }
}

impl Source {
    /// Clone the declarative variants (`Local` / `Http` / `Git`).  A live
    /// `Reader` holds a `Box<dyn Read>` and can't be cloned, so it yields
    /// `None` — fine for config-derived sources, which never carry a reader.
    pub fn try_clone(&self) -> Option<Source> {
        match self {
            Source::Local(p) => Some(Source::Local(p.clone())),
            #[cfg(feature = "http")]
            Source::Http { uri } => Some(Source::Http { uri: uri.clone() }),
            #[cfg(feature = "git")]
            Source::Git { uri, paths } => Some(Source::Git { uri: uri.clone(), paths: paths.clone() }),
            Source::Reader { .. } => None,
        }
    }
}

impl PartialEq for Source {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Local(l0), Self::Local(r0)) => l0 == r0,
            (Self::Reader { name: l_name, .. }, Self::Reader { name: r_name, .. }) => l_name == r_name,
            (Self::Http { uri: l_uri }, Self::Http { uri: r_uri }) => l_uri == r_uri,
            (Self::Git { uri: l_uri, paths: l_paths }, Self::Git { uri: r_uri, paths: r_paths }) => l_uri == r_uri && l_paths == r_paths,
            _ => false,
        }
    }
}

impl Eq for Source {}

impl Source {
    fn variant_index(&self) -> u8 {
        match self {
            Self::Local(_) => 0,
            Self::Reader { .. } => 1,
            Self::Http { .. } => 2,
            Self::Git { .. } => 3,
        }
    }
}

impl Ord for Source {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (Self::Local(r), Self::Local(l)) => r.cmp(l),
            (Self::Reader { name, .. }, Self::Reader { name: lname, .. }) => name.cmp(lname),
            (Self::Http { uri }, Self::Http { uri: luri }) => {
                uri.to_string().cmp(&luri.to_string())
            }
            (Self::Git { uri, paths }, Self::Git { uri: luri, paths: lpaths }) => {
                uri.cmp(luri).then_with(|| paths.cmp(lpaths))
            }
            _ => self.variant_index().cmp(&other.variant_index()),
        }
    }
}

impl PartialOrd for Source {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

// -- serde -------------------------------------------------------------------
//
// `Source` carries a `Box<dyn Read>` in its `Reader` variant (a live runtime
// stream), which cannot be (de)serialized — so the wire form covers only the
// declarative variants (`Local` / `Http` / `Git`).  `Http`'s URI travels as its
// string form.  Serializing a `Reader` is an error; it never appears in a
// deserialized config (e.g. a `KBManager`'s constituents).

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum SourceWire {
    Local(Vec<PathBuf>),
    #[cfg(feature = "http")]
    Http(String),
    #[cfg(feature = "git")]
    Git { uri: String, paths: Vec<PathBuf> },
}

impl serde::Serialize for Source {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let wire = match self {
            Source::Local(p) => SourceWire::Local(p.clone()),
            #[cfg(feature = "http")]
            Source::Http { uri } => SourceWire::Http(uri.to_string()),
            #[cfg(feature = "git")]
            Source::Git { uri, paths } => SourceWire::Git { uri: uri.clone(), paths: paths.clone() },
            Source::Reader { name, .. } => return Err(serde::ser::Error::custom(format!(
                "Source::Reader (`{name}`) is a runtime stream and cannot be serialized"))),
        };
        wire.serialize(ser)
    }
}

impl<'de> serde::Deserialize<'de> for Source {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        Ok(match SourceWire::deserialize(de)? {
            SourceWire::Local(p) => Source::Local(p),
            #[cfg(feature = "http")]
            SourceWire::Http(u) => Source::Http {
                uri: u.parse().map_err(serde::de::Error::custom)?,
            },
            #[cfg(feature = "git")]
            SourceWire::Git { uri, paths } => Source::Git { uri, paths },
        })
    }
}

// Manual `Debug` — the `Reader` variant's `Box<dyn Read>` isn't `Debug`, so the
// enum can't derive it; the reader is shown as an opaque field.
impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Source::Local(paths) => f.debug_tuple("Local").field(paths).finish(),
            Source::Reader { name, .. } =>
                f.debug_struct("Reader").field("name", name).finish_non_exhaustive(),
            #[cfg(feature = "http")]
            Source::Http { uri } =>
                f.debug_struct("Http").field("uri", &uri.to_string()).finish(),
            #[cfg(feature = "git")]
            Source::Git { uri, paths } =>
                f.debug_struct("Git").field("uri", uri).field("paths", paths).finish(),
        }
    }
}

impl Source {
    /// Read a `Source` to `(name, contents)`.
    pub(crate) fn read(self, sink: Option<&DynSink>) -> SdkResult<Vec<SourceFile>> {
        if let Some(s) = sink {
            s.emit(&ProgressEvent::PhaseStarted { name: "opening source for read" });
        }
        let r: Vec<SourceFile> = match self {
            Source::Local(paths) => {
                // A path may be a file (read directly) or a directory (expanded
                // into its parseable children, non-recursive + sorted).  This
                // lets callers pass a mix of files and ontology directories.
                let mut out: Vec<SourceFile> = Vec::with_capacity(paths.len());
                for p in paths {
                    if p.is_dir() {
                        for child in read_dir_sources(&p)? {
                            out.push(read_local_file(child)?);
                        }
                    } else {
                        out.push(read_local_file(p)?);
                    }
                }
                out
            },
            Source::Reader { name, mut reader } => {
                let mut contents = String::new();
                reader.read_to_string(&mut contents)
                    .map_err(|e| SdkError::Io { path: PathBuf::from(&name), source: e })?;
                let source = SourceFile::from_file(name.clone().into(), contents, FileOrigin::Local).ok_or_else(|| {
                    SdkError::Input(name.into())
                })?;
                vec![source]
            }
            #[cfg(feature = "http")]
            Source::Http { uri } => {
                // Last non-empty path segment → a file-ish name for parser
                // detection (the content sniff in `from_file` is the fallback
                // when the URL has no recognizable extension).
                let name = uri.path().rsplit('/').find(|s| !s.is_empty())
                    .unwrap_or("remote").to_string();
                if let Some(s) = sink.as_ref() {
                    s.emit(&ProgressEvent::PhaseStarted { name: "http fetch" });
                }
                let mut resp = ureq::get(uri).call()
                    .map_err(|e| SdkError::Http(e.to_string()))?;
                let contents = resp.body_mut().read_to_string()
                    .map_err(|e| SdkError::Http(e.to_string()))?;
                if let Some(s) = sink.as_ref() {
                    s.emit(&ProgressEvent::PhaseFinished { name: "http fetch" });
                }
                let source = SourceFile::from_file(name.clone().into(), contents, FileOrigin::Remote)
                    .ok_or_else(|| SdkError::Input(name.into()))?;
                vec![source]
            }
            #[cfg(feature = "git")]
            Source::Git { uri, paths } => {
                let paths: Vec<String> = paths.iter().map(|p| -> String { p.to_string_lossy().into() }).collect();
                let (_tmp, dir) = git::fetch_repo_sparse(&uri, &paths)?;
                // Resolve each requested path under the checkout exactly like a
                // local argument: a file is read directly; a directory loads its
                // direct files (NON-recursive, unrecognized files dropped).  We
                // only ever look at the requested paths, so the `.git/` metadata
                // dir is never visited.  A path the repo didn't have is skipped.
                let mut sources: Vec<SourceFile> = Vec::new();
                for path_str in &paths {
                    let p = dir.join(path_str.trim_end_matches('/'));
                    if p.is_dir() {
                        for child in read_dir_sources(&p)? {
                            sources.push(read_file_source(child, FileOrigin::Git)?);
                        }
                    } else if p.is_file() {
                        sources.push(read_file_source(p, FileOrigin::Git)?);
                    }
                }
                sources
            },
        };
        if let Some(s) = sink {
            s.emit(&&ProgressEvent::PhaseFinished { name: "opening source for read" });
        }
        Ok(r)
    }
}

/// Read one local file into a [`SourceFile`] (origin [`FileOrigin::Local`]).
fn read_local_file(p: PathBuf) -> SdkResult<SourceFile> {
    read_file_source(p, FileOrigin::Local)
}

/// Read one on-disk file into a [`SourceFile`], detecting its parser from the
/// file name + content and tagging it `origin`.  TPTP files have their
/// `include(...)` directives spliced first (see [`splice_tptp_includes`]).  An
/// undetectable file is an error here — callers expanding a *directory* filter
/// unrecognized names out first (see [`read_dir_sources`]), so this only errors
/// for an explicitly-named single source.
fn read_file_source(p: PathBuf, origin: FileOrigin) -> SdkResult<SourceFile> {
    let contents = std::fs::read_to_string(&p)
        .map_err(|e| SdkError::Io { path: p.clone(), source: e })?;
    let contents = splice_tptp_includes(&p, contents)?;
    SourceFile::from_file(p.clone(), contents, origin)
        .ok_or_else(|| SdkError::Input(p))
}

/// Splice TPTP `include('…')` directives (the cross-file handler) when `path`
/// names a TPTP file (`.p` / `.tptp` / `.ax`).  Relative includes resolve
/// against the file's directory and `$TPTP`; non-TPTP files pass through
/// unchanged.  This makes every TPTP source the SDK reads — for `Session::test`
/// or `Session::ingest` — self-contained before it reaches the parser.
fn splice_tptp_includes(path: &Path, contents: String) -> SdkResult<String> {
    if matches!(Parser::from_filename(&path.to_string_lossy()), Some(Parser::Tptp { .. })) {
        // STOPGAP so the crate compiles: drive the new `Vec<SourceFile>` resolver
        // with a local-fs reader and re-join the parts into the single spliced
        // string this call site still expects.  Replace with real per-file
        // `SourceFile` handling (and a Source-backed reader) when integrating.
        let base = path.to_string_lossy();
        let read = |loc: &str| std::fs::read_to_string(loc).map_err(|e| e.to_string());
        let parts = tptp::resolve_includes(&contents, &base, &read).map_err(|e| {
            SdkError::Config(format!("include resolution failed for {}: {e}", path.display()))
        })?;
        Ok(parts.into_iter().map(|sf| sf.contents).collect::<Vec<_>>().join("\n"))
    } else {
        Ok(contents)
    }
}

#[cfg(test)]
mod tests {
    use super::Source;
    use sigmakee_rs_core::{FileOrigin, Parser};

    // `ontologyportal/sumo` Merge.kif — the SUMO upper ontology.  Stable, public,
    // and large enough to exercise a real fetch.  These tests hit the network, so
    // they are `#[ignore]`d: run with `cargo test -p sigmakee-rs-sdk --features
    // <http|git> -- --ignored`.
    const RAW_MERGE_KIF: &str =
        "https://raw.githubusercontent.com/ontologyportal/sumo/refs/heads/master/Merge.kif";
    const SUMO_REPO: &str = "https://github.com/ontologyportal/sumo";

    #[cfg(feature = "http")]
    #[test]
    #[ignore = "network: fetches Merge.kif from raw.githubusercontent.com"]
    fn http_fetches_a_remote_kif() {
        let uri = RAW_MERGE_KIF.parse::<ureq::http::Uri>().expect("valid uri");
        let mut files = Source::Http { uri }.read(None).expect("http fetch should succeed");
        assert_eq!(files.len(), 1, "one SourceFile per http source");
        let sf = files.pop().unwrap();
        assert_eq!(sf.name, "Merge.kif", "name comes from the last URL segment");
        assert!(matches!(sf.parser, Parser::Kif), "`.kif` detected as KIF");
        assert!(matches!(sf.origin, FileOrigin::Remote), "tagged as a remote fetch");
        assert!(sf.contents.contains("subclass"),
            "fetched body should be the real Merge.kif (contains `subclass`)");
    }

    #[cfg(feature = "git")]
    #[test]
    #[ignore = "network: sparse-clones ontologyportal/sumo over git (transfers HEAD blobs)"]
    fn git_sparse_fetches_a_single_file() {
        let src = Source::Git {
            uri:   SUMO_REPO.to_string(),
            paths: vec![std::path::PathBuf::from("Merge.kif")],
        };
        let files = src.read(None).expect("git fetch should succeed");
        // Only the requested file is checked out; the `.git` dir is filtered out.
        assert_eq!(files.len(), 1, "sparse checkout yields exactly the requested file");
        let merge = &files[0];
        assert_eq!(merge.name, "Merge.kif");
        assert!(matches!(merge.parser, Parser::Kif));
        assert!(matches!(merge.origin, FileOrigin::Git), "tagged as a git fetch");
        assert!(merge.contents.contains("subclass"),
            "checked-out blob should be the real Merge.kif");
    }

    #[cfg(feature = "git")]
    #[test]
    #[ignore = "network: sparse-clones ontologyportal/sumo over git (transfers HEAD blobs)"]
    fn git_sparse_fetches_a_subdirectory() {
        // Fetching a *directory* path loads its direct files (non-recursive),
        // the same as a local directory argument.  `SimpleFacts/` holds one
        // `.kif`, which must surface.
        let src = Source::Git {
            uri:   SUMO_REPO.to_string(),
            paths: vec![std::path::PathBuf::from("SimpleFacts")],
        };
        let files = src.read(None).expect("git fetch should succeed");
        let car = files.iter().find(|sf| sf.name == "CarBrands.kif")
            .expect("CarBrands.kif surfaced from the SimpleFacts directory");
        assert!(matches!(car.parser, Parser::Kif));
        assert!(matches!(car.origin, FileOrigin::Git));
        assert!(!car.contents.trim().is_empty(), "blob should have content");
    }

    // Deterministic (no network): the non-recursive directory expansion that
    // both `Source::Local(dir)` and `Source::Git { paths: [dir] }` rely on.
    #[test]
    fn read_dir_sources_is_non_recursive_and_drops_unparseable() {
        use std::fs;
        let root = std::env::temp_dir().join("sdk-readdir-nonrecursive");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a.kif"), "(subclass A B)").unwrap();              // recognized, top-level
        fs::write(root.join("README.md"), "not an ontology").unwrap();         // unrecognized → dropped
        fs::write(root.join("sub").join("b.kif"), "(instance x A)").unwrap();   // nested → NOT loaded

        let files = super::read_dir_sources(&root).unwrap();
        let names: Vec<String> = files.iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["a.kif"],
            "only top-level recognized files: drops README, the `sub/` subtree, and `.git/`");
        let _ = fs::remove_dir_all(&root);
    }
}

/// List the parseable files in `dir` (non-recursive, sorted).  A file counts
/// iff [`Parser::from_filename`] recognizes its name — so an ontology directory
/// of `*.kif` (or a suite of `*.tq`) comes through while READMEs and other
/// stray files are skipped.
fn read_dir_sources(dir: &Path) -> SdkResult<Vec<PathBuf>> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| SdkError::DirRead { path: dir.to_path_buf(), message: e.to_string() })?;
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .and_then(Parser::from_filename)
                    .is_some()
        })
        .collect();
    files.sort();
    Ok(files)
}