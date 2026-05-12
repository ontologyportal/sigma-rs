//! Canonical definitions of shared data types.

use std::path::PathBuf;
use serde::{Deserialize, Serialize};

use crate::Parser;
pub use crate::parse::{OpKind, Span};

// -- Id types -----------------------------------------------------------------

pub use crate::syntactic::sentence::SymbolId;
pub use crate::syntactic::sentence::SentenceId;
pub(crate) use crate::syntactic::caches::session::SessionId;

// -- Per-layer type facade ----------------------------------------------------

#[allow(unused_imports)]
pub use crate::semantics::types::{DocEntry, TaxDirection, TaxRelation};
#[allow(unused_imports)]
pub(crate) use crate::semantics::types::{
    ClassInference, RelationDomain, RelationRange, RelationRelation,
};

#[allow(unused_imports)]
pub(crate) use crate::trans::types::CachedFormula;

// -- Literal ------------------------------------------------------------------

pub use crate::syntactic::sentence::Literal;

// -- Element -------------------------------------------------------------------

pub use crate::syntactic::sentence::Element;

// -- Sentence ------------------------------------------------------------------

pub use crate::syntactic::sentence::Sentence;

// -- Symbols -------------------------------------------------------------------

pub use crate::syntactic::sentence::{InternedSym, Symbol};

// -- ElementVec ----------------------------------------------------------------

pub use crate::syntactic::sentence::ElementVec;

// Ast
pub use crate::parse::AstNode;

// -- Occurrence ----------------------------------------------------------------

/// Position of a single symbol reference inside the knowledge base.
///
/// One `Occurrence` is recorded per `AstNode::Symbol` in a formula's AST.
/// Variables are not indexed and synthetic spans are filtered out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Occurrence {
    /// Fingerprint of the root formula (`AstNode`) this occurrence belongs to.
    pub node: u64,
    /// Source range of the symbol token itself.
    pub span: Span,
    /// Role the symbol plays in its immediate enclosing list.
    pub kind: OccurrenceKind,
}

// Identity is `span`: a source range identifies exactly one token in exactly
// one file, so `node`/`kind` are excluded from equality/hashing.
impl PartialEq for Occurrence {
    fn eq(&self, other: &Self) -> bool {
        self.span == other.span
    }
}
impl Eq for Occurrence {}
impl std::hash::Hash for Occurrence {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.span.hash(state);
    }
}

/// Classification of a symbol occurrence by its position inside its
/// immediate enclosing list.  `Head` means the symbol is `elements[0]`
/// of a (possibly nested) form; `Arg` means it appears as any
/// subsequent argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OccurrenceKind {
    /// Symbol is `elements[0]` of its enclosing form.
    Head,
    /// Symbol appears as an argument in its enclosing form.
    Arg,
}

// -- Symbol --------------------------------------------------------------------

/// Where a source file was obtained from.
#[derive(Debug, Clone)]
pub enum FileOrigin {
    /// Fetched from a git repository.
    #[allow(dead_code)]
    Git,
    /// Read from the local filesystem.
    Local,
    /// Fetched from a remote URL.
    #[allow(dead_code)]
    Remote,
    /// Generated in memory rather than read from a source.
    #[allow(dead_code)]
    Synthetic,
    /// Supplied inline as a string.
    Inline
}

/// A source file with its parser, path, contents, and any prebuilt AST.
#[derive(Debug, Clone)]
pub struct SourceFile {
    /// Parser dialect for this file's contents.
    pub parser: crate::Parser,
    /// File name.
    #[allow(dead_code)]
    pub name: String,
    /// File path.
    pub path: std::path::PathBuf,
    /// Where the file was obtained from.
    #[allow(dead_code)]
    pub origin: FileOrigin,
    /// Raw source text.
    pub contents: String,
    /// Prebuilt AST nodes, if available.
    pub prebuilt: Option<Vec<AstNode>>
}

impl SourceFile {
    /// Builds a KIF source file from a path and its contents.
    pub fn kif(file: PathBuf, contents: String) -> Self {
        Self {
            parser: crate::Parser::Kif,
            name: file.file_name().unwrap_or_default().to_str().unwrap_or_default().to_string(),
            path: file,
            origin: FileOrigin::Local,
            contents,
            prebuilt: None,
        }
    }

    /// Builds an inline KIF source file from a name and its contents.
    pub fn inline_kif(name: &str, contents: String) -> Self {
        Self {
            parser: crate::Parser::Kif,
            name: name.to_string(),
            path: PathBuf::new(),
            origin: FileOrigin::Inline,
            contents,
            prebuilt: None,
        }
    }

    /// Builds a KIF source file for `file` with empty contents.
    pub fn truncate(file: PathBuf) -> Self {
        Self {
            parser: crate::Parser::Kif,
            name: file.file_name().unwrap_or_default().to_str().unwrap_or_default().to_string(),
            path: file,
            origin: FileOrigin::Local,
            contents: String::new(),
            prebuilt: None,
        }
    }

    /// Builds a source file, inferring the parser from the file name or, failing
    /// that, from the contents. Returns `None` when no parser can be determined.
    pub fn from_file(path: PathBuf, contents: String, origin: FileOrigin) -> Option<Self> {
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let parser = match Parser::from_filename(&name) {
            Some(p) => p,
            None => match Parser::from_contents(&contents) {
                Some(p) => p,
                None => return None
            }
        };
        Some(Self {
            parser, 
            name,
            path,
            origin,
            contents,
            prebuilt: None
        })
    }
}