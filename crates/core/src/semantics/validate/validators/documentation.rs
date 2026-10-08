//! H001-H004 documentation completeness: every symbol carries a
//! `documentation` entry (one per language), a `termFormat` entry, and -- for
//! relations -- a `format` string.
//!
//! Each finding is raised at the symbol's defining sentence (see
//! `semantic::defining_sentences`), so whole-KB, per-file, and per-session
//! validation all report a symbol's gaps where it is declared. A symbol with
//! no defining sentence is never reported.

use thiserror::Error;

use crate::semantics::errors::{semantic_error, BoxedError};
use crate::semantics::validate::cx::Cx;
use crate::semantics::validate::traits::FormulaValidator;
use crate::{Element, SentenceId, SymbolId};

/// A term has no `documentation` axiom.
#[derive(Debug, Clone, Error)]
#[error("term '{sym}' has no documentation axiom")]
pub struct MissingDocumentation {
    pub sym: String,
}
semantic_error!(MissingDocumentation, "H001", "missing-documentation", Hint);

/// A term has several `documentation` axioms in one language.
#[derive(Debug, Clone, Error)]
#[error("term '{sym}' has {count} documentation axioms in {language} (expected 1)")]
pub struct MultipleDocumentation {
    pub sym: String,
    pub language: String,
    pub count: usize,
}
semantic_error!(
    MultipleDocumentation,
    "H002",
    "multiple-documentation",
    Hint
);

/// A term has no `termFormat` axiom.
#[derive(Debug, Clone, Error)]
#[error("term '{sym}' has no termFormat axiom")]
pub struct MissingTermFormat {
    pub sym: String,
}
semantic_error!(MissingTermFormat, "H003", "missing-term-format", Hint);

/// A relation has no `format` axiom.
#[derive(Debug, Clone, Error)]
#[error("relation '{sym}' has no format axiom")]
pub struct MissingFormatString {
    pub sym: String,
}
semantic_error!(MissingFormatString, "H004", "missing-format-string", Hint);

pub(crate) struct DocumentationCompleteness;

impl FormulaValidator for DocumentationCompleteness {
    type Error = BoxedError;

    fn check(&self, cx: &Cx<'_>, root: SentenceId) -> Vec<BoxedError> {
        let Some(s) = cx.sentence(root) else {
            return Vec::new();
        };
        // A root can only define its head or its first argument.
        let mut defined: Vec<SymbolId> = [s.elements.first(), s.elements.get(1)]
            .into_iter()
            .filter_map(|el| match el {
                Some(Element::Symbol(sym)) => Some(sym.id()),
                _ => None,
            })
            .filter(|&sym| cx.defining_sentence(sym) == Some(root) && !cx.is_skolem(sym))
            .collect();
        defined.dedup();

        let coverage = cx.doc_coverage();
        let mut out: Vec<BoxedError> = Vec::new();
        for sym in defined {
            let name = cx.sym_name(sym);
            match coverage.documentation.get(&sym) {
                None => out.push(Box::new(MissingDocumentation { sym: name.clone() })),
                Some(per_language) => {
                    for (language, &count) in per_language.iter().filter(|(_, &n)| n > 1) {
                        out.push(Box::new(MultipleDocumentation {
                            sym: name.clone(),
                            language: language.clone(),
                            count,
                        }));
                    }
                }
            }
            if !coverage.term_format.contains(&sym) {
                out.push(Box::new(MissingTermFormat { sym: name.clone() }));
            }
            let is_relation = cx.is_relation(sym) || cx.is_predicate(sym) || cx.is_function(sym);
            if is_relation && !coverage.format.contains(&sym) {
                out.push(Box::new(MissingFormatString { sym: name }));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use crate::semantics::types::Scope;
    use crate::semantics::validate::test_support::{kif_layer, roots};
    use crate::semantics::SemanticLayer;

    const RELATIONS: &str = "
        (subclass Relation Entity)
        (subclass BinaryPredicate Relation)
    ";

    /// Every documentation hint raised for `sym`, with the root it was raised
    /// at.
    fn hints(layer: &SemanticLayer, sym: &str) -> Vec<(String, &'static str)> {
        let quoted = format!("'{sym}'");
        let mut out: Vec<(String, &'static str)> = roots(layer)
            .into_iter()
            .flat_map(|sid| {
                let kif = crate::syntactic::display::sentence_to_plain_kif(sid, &layer.syntactic);
                layer
                    .validator_scoped(Scope::Base)
                    .validate_sentence_collect(sid)
                    .into_iter()
                    .filter(|e| e.code().starts_with('H') && e.to_string().contains(&quoted))
                    .map(|e| (kif.clone(), e.code()))
                    .collect::<Vec<_>>()
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn undocumented_symbols_are_reported_at_their_defining_sentence() {
        let layer = kif_layer(&format!(
            "{RELATIONS}\n(subclass Dog Entity)\n(likes Dog Cat)"
        ));
        assert_eq!(
            hints(&layer, "Dog"),
            [
                ("(subclass Dog Entity)".to_string(), "H001"),
                ("(subclass Dog Entity)".to_string(), "H003"),
            ]
        );
        // `Cat` is only an argument: it has no defining sentence.
        assert!(hints(&layer, "Cat").is_empty());
    }

    #[test]
    fn a_documented_symbol_raises_nothing() {
        let layer = kif_layer(
            "(subclass Dog Entity)
             (documentation Dog EnglishLanguage \"A dog.\")
             (termFormat EnglishLanguage Dog \"dog\")",
        );
        assert_eq!(hints(&layer, "Dog"), []);
    }

    #[test]
    fn duplicate_documentation_in_one_language_is_h002() {
        let layer = kif_layer(
            "(subclass Dog Entity)
             (documentation Dog EnglishLanguage \"A dog.\")
             (documentation Dog EnglishLanguage \"A canine.\")
             (documentation Dog FrenchLanguage \"Un chien.\")",
        );
        let h002: Vec<_> = hints(&layer, "Dog")
            .into_iter()
            .filter(|(_, c)| *c == "H002")
            .collect();
        assert_eq!(h002, [("(subclass Dog Entity)".to_string(), "H002")]);
    }

    #[test]
    fn relations_without_format_are_h004() {
        let layer = kif_layer(&format!(
            "{RELATIONS}
            (instance likes BinaryPredicate)
            (instance loves BinaryPredicate)
            (format EnglishLanguage loves \"%1 loves %2\")"
        ));
        let h004 = |sym| -> Vec<_> {
            hints(&layer, sym)
                .into_iter()
                .filter(|(_, c)| *c == "H004")
                .collect()
        };
        assert_eq!(
            h004("likes"),
            [("(instance likes BinaryPredicate)".to_string(), "H004")]
        );
        assert_eq!(h004("loves"), []);
    }
}
