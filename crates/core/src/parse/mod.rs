// crates/core/src/parse/mod.rs
//
// Parse submodule -- extensible for multiple input formats.
// Currently only KIF is supported.

pub mod ast;
pub mod dialect;
pub mod doc;
pub mod document;
pub mod error;
pub mod fingerprint;
pub mod kif;
pub mod macros;
pub mod span;
#[cfg(any(feature = "ask", feature = "native-prover"))]
pub mod szs;
pub mod tptp;
pub mod tq;

pub use ast::*;
pub use document::{parse_document, ParsedDocument};
pub use error::*;
pub use fingerprint::sentence_fingerprint;
pub use span::*;

pub use crate::parse::doc::CommentBlock;
pub use crate::parse::kif::parser::KifParseOptions;
pub use crate::parse::tptp::parser::TptpParseOptions;
use crate::{
    parse::{doc::DocItem, tq::parse_tq},
    Diagnostic, ToDiagnostic,
};

pub(crate) type ParseResult<T> = (Vec<T>, Vec<(Span, Box<dyn ParseError>)>);

#[derive(Debug, Clone)]
pub enum Parser {
    Kif { options: Option<KifParseOptions> },
    Tptp { options: Option<TptpParseOptions> },
    Tq,
}

// Manual impl: `#[derive(Default)]`'s `#[default]` attribute only accepts a
// unit variant, and `Kif` now carries options.
impl Default for Parser {
    fn default() -> Self {
        Parser::Kif { options: None }
    }
}

impl Parser {
    /// Perform full parsing on a file input.
    ///
    /// Comments are lexical trivia and never appear in the returned AST; the
    /// consolidated [`CommentBlock`] side list surfaces on
    /// [`ParsedDocument::comments`](crate::ParsedDocument) via
    /// [`parse_document`](crate::parse_document).
    pub fn parse(&self, inp: &str, file: &str) -> ParseResult<DocItem> {
        self.parse_full(inp, file).0
    }

    /// The one parse implementation: AST + errors plus the consolidated
    /// [`CommentBlock`]s (KIF only, and empty under
    /// [`KifParseOptions::skip_comments`]).  Crate-internal: the public
    /// surface is [`Self::parse`] and `parse_document` (which carries the
    /// comments out on the document).
    pub(crate) fn parse_full(
        &self,
        inp: &str,
        file: &str,
    ) -> (ParseResult<DocItem>, Vec<CommentBlock>) {
        let (ast, errors, comments) = match self {
            Parser::Kif { options } => {
                let skip = options.as_ref().is_some_and(|o| o.skip_comments);
                let (tokens, tok_err) = if skip {
                    kif::tokenize_without_comments(inp, file)
                } else {
                    kif::tokenize(inp, file)
                };
                let comments = if skip {
                    Vec::new()
                } else {
                    kif::comment_blocks(&tokens)
                };
                let (ast, parse_err) = kif::parse(tokens, file);
                let mut errors = tok_err;
                errors.extend(parse_err);
                let doc: Vec<DocItem> = ast.into_iter().map(DocItem::Stmt).collect();
                (doc, wrap_error(errors), comments)
            }
            Parser::Tptp { options } => {
                let (tokens, tok_err, metas) = tptp::tokenize_with_meta(inp, file);
                let skip = options.as_ref().is_some_and(|o| o.skip_comments);
                let comments = if skip {
                    Vec::new()
                } else {
                    tptp::comment_blocks(&tokens)
                };
                let (mut ast, parse_err) = tptp::parse(tokens, file, options.clone());
                let mut errors = tok_err;
                errors.extend(parse_err);
                // Only TPTP-specific literal decoding stays in the parse stage.  The
                // generic macros (quantifier collapse, top-level-`forall` strip, row-var
                // expansion) moved to the ingest/normalization stage and run there,
                // parser-free, so `SourceStore` keeps the raw parsed AST.
                for node in &mut ast {
                    macros::decode_tptp_literals(node, self);
                }
                // Header pragmas (`% Status : Theorem`) recognized by the
                // tokenizer ride in as `DocItem::Meta` alongside the parsed
                // statements — the SDK's SZS grading path reads the `status`
                // key back off the document.
                let mut doc: Vec<DocItem> = metas.into_iter().map(DocItem::Meta).collect();
                doc.extend(ast.into_iter().map(DocItem::Stmt));
                (doc, wrap_error(errors), comments)
            }
            Parser::Tq => {
                let (doc, errors, comments) = parse_tq(inp, file);
                (doc, wrap_error(errors), comments)
            }
        };
        ((ast, errors), comments)
    }

    /// Perform tokenization ONLY on file contents
    pub fn tokenize(&self, inp: &str, file: &str) -> ParseResult<String> {
        match self {
            Parser::Kif { options } => {
                let skip = options.as_ref().is_some_and(|o| o.skip_comments);
                let (tokens, err) = if skip {
                    kif::tokenize_without_comments(inp, file)
                } else {
                    kif::tokenize(inp, file)
                };
                let errors = wrap_error(err);
                (
                    tokens
                        .iter()
                        .map(|t| format!("{}", t).to_uppercase())
                        .collect(),
                    errors,
                )
            }
            Parser::Tq => {
                let (tokens, err) = kif::tokenize(inp, file);
                let errors = wrap_error(err);
                (
                    tokens
                        .iter()
                        .map(|t| format!("{}", t).to_uppercase())
                        .collect(),
                    errors,
                )
            }
            Parser::Tptp { options } => {
                let skip = options.as_ref().is_some_and(|o| o.skip_comments);
                let (tokens, err) = if skip {
                    tptp::tokenize_without_comments(inp, file)
                } else {
                    tptp::tokenize(inp, file)
                };
                let errors = wrap_error(err);
                (
                    tokens
                        .iter()
                        .map(|t| format!("{}", t).to_uppercase())
                        .collect(),
                    errors,
                )
            }
        }
    }

    /// Determine if the parser is for a test file
    pub fn is_test(&self) -> bool {
        match self {
            Parser::Kif { .. } => false,
            Parser::Tptp { options } => options.as_ref().is_some_and(|o| o.keep_conjectures),
            Parser::Tq => true,
        }
    }

    /// Create a parser from the file's extension. Returns `None` when nothing
    /// matches
    pub fn from_filename(filename: &str) -> Option<Self> {
        let ext = filename.split(".").last()?;
        let p = match ext {
            "kif" => Parser::Kif { options: None },
            // A `.p` / `.tptp` file is a theorem-proving *problem*: keep its
            // conjecture so it is recognized as a test (`is_test`) and its goal
            // surfaces as the `TestCase` query.
            "p" | "tptp" => Parser::Tptp {
                options: Some(TptpParseOptions {
                    formulas_only: false,
                    keep_conjectures: true,
                    ..TptpParseOptions::default()
                }),
            },
            "ax" => Parser::Tptp {
                options: Some(TptpParseOptions {
                    formulas_only: true,
                    ..TptpParseOptions::default()
                }),
            },
            "tq" => Parser::Tq,
            _ => return None,
        };
        Some(p)
    }

    /// Best-effort parser selection for a source. Returns `None` when nothing matches
    pub fn from_contents(contents: &str) -> Option<Parser> {
        // Content sniff over a bounded prefix (skip line/block comments cheaply
        // by just scanning for the annotated-formula keyword anywhere early).
        let head: String = contents.chars().take(4096).collect();
        if ["fof(", "cnf(", "tff(", "thf(", "tcf(", "include("]
            .iter()
            .any(|kw| head.contains(kw))
        {
            return Some(Parser::Tptp { options: None });
        }
        if head.trim_start().starts_with('(') {
            return Some(Parser::Kif { options: None });
        }
        None
    }
}

/// Parse-only syntax check of KIF text — no KB, no ingestion, no state.
/// Returns the tokenizer/parser diagnostics; empty means the text is
/// syntactically well-formed.  `file` names the source in each diagnostic's
/// span.  Use this to vet a transient editor buffer BEFORE staging it into a
/// [`KnowledgeBase`]: staging syntactically broken content reads as "the file
/// is now empty" and retracts every sentence the file previously contributed.
pub fn try_parse_file(text: &str, file: &str) -> Vec<Diagnostic> {
    let Some(parser) = Parser::from_filename(file) else {
        return vec![GenericParseError::UnknownFileType {
            filename: file.to_string(),
        }
        .to_diagnostic()];
    };
    let (_, errors) = parser.parse(text, file);
    errors.iter().map(|(_, e)| e.to_diagnostic()).collect()
}

fn wrap_error<E: ParseError + 'static>(err: Vec<E>) -> Vec<(Span, Box<dyn ParseError>)> {
    err.into_iter()
        .map(|e| (e.get_span(), Box::new(e) as Box<dyn ParseError>))
        .collect::<Vec<(Span, Box<dyn ParseError>)>>()
}

#[cfg(test)]
mod tests {
    use super::*;

    // A TPTP problem's `% Status : <word>` header pragma must surface as a
    // `DocItem::Meta` (key "status") in the parsed document — the SDK's SZS
    // grading path (`Session::test`) reads it back off here rather than
    // re-parsing the raw file text itself.
    #[test]
    fn tptp_status_header_becomes_a_meta_docitem() {
        let src = "\
            % File     : MINI001+1\n\
            % Status   : Theorem\n\
            fof(a1, axiom, p).\n\
            fof(g, conjecture, p).\n";
        let opts = TptpParseOptions {
            keep_conjectures: true,
            ..TptpParseOptions::none()
        };
        let (doc, errors) = Parser::Tptp {
            options: Some(opts),
        }
        .parse(src, "mini");
        assert!(errors.is_empty(), "unexpected parse errors: {errors:?}");
        let metas: Vec<&crate::parse::doc::MetaNode> =
            doc.iter().filter_map(DocItem::as_meta).collect();
        assert_eq!(metas.len(), 1, "exactly one status meta expected: {doc:?}");
        assert_eq!(metas[0].key, "status");
        assert!(
            matches!(&metas[0].args[0], AstNode::Symbol { name, .. } if name == "Theorem"),
            "expected Symbol(\"Theorem\"), got {:?}",
            metas[0].args[0]
        );
        // The two `fof` statements still parse as ordinary Stmt items.
        assert_eq!(doc.iter().filter(|d| d.as_stmt().is_some()).count(), 2);
    }
}
