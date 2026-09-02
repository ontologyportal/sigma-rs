//! `textDocument/semanticTokens/full` handler.
//!
//! The server advertises a fixed token-type legend at initialize; every token
//! in a requested document is classified and emitted in LSP's delta-encoded
//! 5-tuple form `[deltaLine, deltaStart, length, typeIdx, modifiersBitset]`.
//!
//! Tokens come from the document's cached token stream (`DocState::tokens`,
//! computed once per reparse). Symbol classification consults the shared
//! KB: `KnowledgeBase::is_class` highlights as `type`; `is_function` as
//! `function`; a predicate or any other non-function relation as
//! `relation`; anything else falls back to a title-case heuristic.
//! Operators are always `keyword`.

use lsp_types::{
    SemanticToken, SemanticTokenType, SemanticTokens, SemanticTokensLegend, SemanticTokensParams,
    SemanticTokensResult,
};
use ropey::Rope;

use sigmakee_rs_sdk::{KnowledgeBase, TokenKind, TopLayer};

use crate::conv::{offset_to_position, uri_to_tag};
use crate::state::GlobalState;

// -- Legend -------------------------------------------------------------------

/// The fixed token-type legend the server advertises at startup. Each token's
/// `typeIdx` is an index into this array.
///
/// Order matters: the client uses the index to look up the type name. Never
/// reorder without bumping the legend version.
pub const TOKEN_TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::KEYWORD,         // 0: logical operators
    SemanticTokenType::TYPE,            // 1: class-like symbols
    SemanticTokenType::FUNCTION,        // 2: function symbols (is_function)
    SemanticTokenType::VARIABLE,        // 3: ?X, @X
    SemanticTokenType::STRING,          // 4: "string literals"
    SemanticTokenType::NUMBER,          // 5: numeric literals
    SemanticTokenType::COMMENT,         // 6: ; line comments
    SemanticTokenType::new("relation"), // 7: predicate / non-function relation symbols
];

// Indices into TOKEN_TYPES.  `u32` matches LSP's wire type.
const T_KEYWORD: u32 = 0;
const T_TYPE: u32 = 1;
const T_FUNCTION: u32 = 2;
const T_VARIABLE: u32 = 3;
const T_STRING: u32 = 4;
const T_NUMBER: u32 = 5;
const T_COMMENT: u32 = 6;
const T_RELATION: u32 = 7;

/// Assemble the legend value used in server capabilities.
pub fn semantic_tokens_legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: TOKEN_TYPES.to_vec(),
        token_modifiers: Vec::new(),
    }
}

// -- Handler ------------------------------------------------------------------

/// Handle a `textDocument/semanticTokens/full` request, returning the
/// document's classified tokens in LSP delta-encoded form, or `None` when the
/// document or KB is unavailable.
pub fn handle_semantic_tokens_full<L: TopLayer>(
    state: &GlobalState<L>,
    params: SemanticTokensParams,
) -> Option<SemanticTokensResult> {
    let uri = params.text_document.uri;
    let tag = uri_to_tag(&uri);

    let docs = state.docs.read().ok()?;
    let doc = docs.get(&uri)?;
    let session = state.session.read().ok()?;
    let kb = session.kb();

    let rope = &doc.rope;
    let tokens = &doc.tokens;
    let mut classified: Vec<ClassifiedToken> = Vec::with_capacity(tokens.len());
    // In a `.kif.tq` test file, a harness-directive head (`note` / `time` /
    // `answer` / `file` / `query` / `ask`) at the top level is a keyword --
    // the KB can never classify it, so the symbol fallback would paint it as
    // an ordinary function.  Track "first token after a top-level `(`" with
    // a depth counter; comments are transparent to head position.
    let is_tq = crate::server::is_tq(&tag);
    let mut depth = 0usize;
    let mut head_pending = false;
    for tok in tokens {
        let directive_head = is_tq
            && depth == 1
            && head_pending
            && matches!(&tok.kind, TokenKind::Symbol(n) if sigmakee_rs_sdk::is_tq_directive(n));
        match &tok.kind {
            TokenKind::LParen => {
                depth += 1;
                head_pending = true;
            }
            TokenKind::RParen => {
                depth = depth.saturating_sub(1);
                head_pending = false;
            }
            TokenKind::Comment(_) => {}
            _ => head_pending = false,
        }
        if let Some(mut ct) = classify_token(tok, kb) {
            if directive_head {
                ct.type_idx = T_KEYWORD;
            }
            classified.push(ct);
        }
    }

    let data = encode_delta(&classified, rope);

    Some(SemanticTokensResult::Tokens(SemanticTokens {
        result_id: None,
        data,
    }))
}

// -- Classification -----------------------------------------------------------

#[derive(Debug, Clone)]
struct ClassifiedToken {
    start_offset: usize,
    end_offset: usize,
    type_idx: u32,
}

fn classify_token<L: TopLayer>(
    tok: &sigmakee_rs_sdk::Token,
    kb: &KnowledgeBase<L>,
) -> Option<ClassifiedToken> {
    let type_idx = match &tok.kind {
        TokenKind::LParen | TokenKind::RParen => return None,
        TokenKind::Operator(_) => T_KEYWORD,
        TokenKind::Str(_) => T_STRING,
        TokenKind::Number(_) => T_NUMBER,
        TokenKind::Variable(_) | TokenKind::RowVariable(_) => T_VARIABLE,
        TokenKind::Symbol(name) => classify_symbol(name, kb),
        TokenKind::Comment(_) => T_COMMENT,
    };
    Some(ClassifiedToken {
        start_offset: tok.span.offset,
        end_offset: tok.span.end_offset,
        type_idx,
    })
}

/// Decide the semantic-token type for a symbol name. Queries the KB first
/// (taxonomy-aware); falls back to a title-case heuristic (capitalized ->
/// type, otherwise function) for symbols the KB has not classified.
///
/// A function (`is_function`) and a non-function relation (a predicate, or
/// any other declared relation) get distinct token types -- `function` vs
/// `relation` -- so a client theme can color `(SuccessorFn ?X)` differently
/// from `(subclass ?X ?Y)`.
fn classify_symbol<L: TopLayer>(name: &str, kb: &KnowledgeBase<L>) -> u32 {
    if let Some(id) = kb.symbol_id(name) {
        if kb.is_class(id) {
            return T_TYPE;
        }
        if kb.is_function(id) {
            return T_FUNCTION;
        }
        if kb.is_predicate(id) || kb.is_relation(id) {
            return T_RELATION;
        }
        // Known but unclassified: fall through to the heuristic.
    }
    if name.chars().next().is_some_and(|c| c.is_uppercase()) {
        T_TYPE
    } else {
        T_FUNCTION
    }
}

// -- Delta encoding -----------------------------------------------------------

/// Delta-encode `tokens` into the LSP wire shape.
///
/// `length` for each token is measured in UTF-16 code units, mirroring LSP's
/// default position encoding. Multi-line tokens are skipped.
fn encode_delta(tokens: &[ClassifiedToken], rope: &Rope) -> Vec<SemanticToken> {
    let mut prev_line = 0u32;
    let mut prev_start = 0u32;
    let mut out: Vec<SemanticToken> = Vec::with_capacity(tokens.len());

    for tok in tokens {
        let start_pos = offset_to_position(rope, tok.start_offset);
        let end_pos = offset_to_position(rope, tok.end_offset);

        // LSP's semantic-token format assumes single-line tokens.
        if end_pos.line != start_pos.line {
            continue;
        }

        let length: u32 = end_pos.character.saturating_sub(start_pos.character);
        if length == 0 {
            continue;
        }

        let delta_line = start_pos.line - prev_line;
        let delta_start = if delta_line == 0 {
            start_pos.character - prev_start
        } else {
            start_pos.character
        };

        out.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type: tok.type_idx,
            token_modifiers_bitset: 0,
        });

        prev_line = start_pos.line;
        prev_start = start_pos.character;
    }

    out
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use sigmakee_rs_sdk::{tokenize_kif, KnowledgeBase};

    /// Build a KB with `text` loaded as `file`.
    fn kb_with(text: &str, file: &str) -> KnowledgeBase {
        let mut kb = KnowledgeBase::new();
        let _ = kb.load(
            sigmakee_rs_sdk::SourceFile::kif(std::path::PathBuf::from(file), text.to_string()),
            file,
        );
        kb
    }

    /// Tokenise `text` tagged as `file`.
    fn tokens_for(text: &str, file: &str) -> Vec<sigmakee_rs_sdk::Token> {
        let (toks, _errs) = tokenize_kif(text, file);
        toks
    }

    #[test]
    fn operator_classified_as_keyword() {
        let src = "(=> (P ?X) (Q ?X))";
        let kb = kb_with(src, "t.kif");
        let toks = tokens_for(src, "t.kif");
        let tok = toks
            .iter()
            .find(|t| matches!(t.kind, TokenKind::Operator(_)))
            .expect("operator token present");
        let c = classify_token(tok, &kb).expect("classified");
        assert_eq!(c.type_idx, T_KEYWORD);
    }

    #[test]
    fn variable_classified_as_variable() {
        let src = "(P ?X)";
        let kb = kb_with(src, "t.kif");
        let toks = tokens_for(src, "t.kif");
        let tok = toks
            .iter()
            .find(|t| matches!(t.kind, TokenKind::Variable(_)))
            .expect("variable token");
        assert_eq!(classify_token(tok, &kb).unwrap().type_idx, T_VARIABLE);
    }

    #[test]
    fn uppercase_symbol_is_type_when_unclassified() {
        let kb = KnowledgeBase::new();
        let toks = tokens_for("(P Foo)", "t.kif");
        let tok = toks
            .iter()
            .find(|t| matches!(&t.kind, TokenKind::Symbol(s) if s == "Foo"))
            .expect("Foo token");
        assert_eq!(classify_token(tok, &kb).unwrap().type_idx, T_TYPE);
    }

    #[test]
    fn lowercase_symbol_is_function_when_unclassified() {
        let kb = KnowledgeBase::new();
        let toks = tokens_for("(foo Bar)", "t.kif");
        let tok = toks
            .iter()
            .find(|t| matches!(&t.kind, TokenKind::Symbol(s) if s == "foo"))
            .expect("foo token");
        assert_eq!(classify_token(tok, &kb).unwrap().type_idx, T_FUNCTION);
    }

    #[test]
    fn delta_encoding_is_relative() {
        let src = "(subclass Human Animal)";
        let kb = kb_with(src, "t.kif");
        let toks = tokens_for(src, "t.kif");
        let rope = Rope::from_str(src);

        let classified: Vec<ClassifiedToken> =
            toks.iter().filter_map(|t| classify_token(t, &kb)).collect();
        assert_eq!(classified.len(), 3, "subclass, Human, Animal");

        let encoded = encode_delta(&classified, &rope);
        assert_eq!(encoded.len(), 3);

        assert_eq!(encoded[0].delta_line, 0);
        assert_eq!(encoded[0].delta_start, 1);
        assert_eq!(encoded[0].length, "subclass".len() as u32);

        assert_eq!(encoded[1].delta_line, 0);
        assert_eq!(encoded[1].delta_start, 9);
        assert_eq!(encoded[1].length, "Human".len() as u32);
    }

    #[test]
    fn parens_are_skipped() {
        let kb = kb_with("(P)", "t.kif");
        let toks = tokens_for("(P)", "t.kif");
        let classified: Vec<_> = toks.iter().filter_map(|t| classify_token(t, &kb)).collect();
        assert_eq!(classified.len(), 1);
    }

    #[test]
    fn declared_function_is_function_declared_predicate_is_relation() {
        let src = "(instance SuccessorFn Function)\n(instance likes Predicate)";
        let mut kb = kb_with(src, "t.kif");
        // `is_function`/`is_predicate` read `Base` scope; an ingested-but-
        // unpromoted session isn't visible there yet.
        kb.make_session_axiomatic("t.kif").expect("promote to Base");
        let toks = tokens_for(src, "t.kif");

        let func_tok = toks
            .iter()
            .find(|t| matches!(&t.kind, TokenKind::Symbol(s) if s == "SuccessorFn"))
            .expect("SuccessorFn token");
        assert_eq!(classify_token(func_tok, &kb).unwrap().type_idx, T_FUNCTION);

        let pred_tok = toks
            .iter()
            .find(|t| matches!(&t.kind, TokenKind::Symbol(s) if s == "likes"))
            .expect("likes token");
        assert_eq!(classify_token(pred_tok, &kb).unwrap().type_idx, T_RELATION);
    }

    #[test]
    fn declared_class_wins_over_heuristic() {
        let src = "(subclass Human Animal)\n(instance Human Class)";
        let kb = kb_with(src, "t.kif");
        let toks = tokens_for(src, "t.kif");
        let human_tok = toks
            .iter()
            .find(|t| matches!(&t.kind, TokenKind::Symbol(s) if s == "Human"))
            .expect("Human token");
        let c = classify_token(human_tok, &kb).unwrap();
        assert_eq!(c.type_idx, T_TYPE, "Human should be TYPE via is_class");
    }
}
