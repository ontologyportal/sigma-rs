//! Natural-language rendering of SUO-KIF formulas via SUMO's
//! `format` and `termFormat` relations.
//!
//! This module backs the `--proof <LANGUAGE>` option on the `sumo ask`
//! CLI: once Vampire produces a proof, each step's AST is handed to
//! [`KnowledgeBase::render_formula`] which walks the tree, looks up
//! template strings in the chosen language, and substitutes argument
//! renderings in positional order.
//!
//! ## Template DSL
//!
//! Templates live in the KB as `(format ?LANG ?REL "...")` and
//! `(termFormat ?LANG ?SYM "...")` facts.  The notation we decode:
//!
//! | Marker       | Meaning                                                      |
//! |--------------|--------------------------------------------------------------|
//! | `%1` … `%9`  | Positional argument (1-indexed) rendered recursively.        |
//! | `&%Symbol`   | Cross-reference: render via `termFormat(Symbol, lang)` with a fallback to the raw name. |
//! | `%n`         | Negation placeholder: empty in positive context, `" not "` in negative. |
//! | `%n{TEXT}`   | Custom negation phrase: empty in positive context, `TEXT` in negative. |
//!
//! ## Structural cases (no `format` entry required)
//!
//! SUMO doesn't provide `format` strings for the logical connectives,
//! so we render them structurally:
//!
//! | KIF                        | Rendering                                       |
//! |----------------------------|-------------------------------------------------|
//! | `(and A B C)`              | `A and B and C`                                 |
//! | `(or A B C)`               | `A or B or C`                                   |
//! | `(not P)`                  | `P` with its negation context propagated, or `it is not the case that P` |
//! | `(=> A B)`                 | `if A then B`                                   |
//! | `(<=> A B)`                | `A if and only if B`                            |
//! | `(exists (?V …) B)`        | `there exists some ?V such that B`              |
//! | `(forall (?V …) B)`        | `for every ?V, B`                               |
//! | `(equal X Y)`              | `X equals Y` (unless `format` overrides it)     |
//!
//! ## Missing specifier handling
//!
//! When a relation lacks a `format` entry or an argument term lacks a
//! `termFormat` entry in the chosen language, the renderer **does not**
//! silently guess.  Missing symbols are collected in a
//! [`RenderReport::missing`] vector, and the caller can decide to fall
//! back to the bare KIF rendering with a warning listing the missing
//! specifiers.  The rendered string still contains a best-effort
//! rendering (bare symbol names where templates are missing) for
//! diagnostic purposes, but callers that want fidelity should check
//! `missing.is_empty()` and fall back otherwise.

#![cfg(feature = "ask")]

use super::KnowledgeBase;
use crate::parse::ast::AstNode;

pub use crate::semantics::render::RenderReport;

#[cfg(test)]
use crate::semantics::render::{ANSI_LINKED, ANSI_NEG, ANSI_OP, ANSI_RESET, ANSI_VAR};

impl<L: crate::layer::TopLayer> KnowledgeBase<L> {
    /// Render `formula` to natural language in `language` (e.g.
    /// `"EnglishLanguage"`).  Plain (no ANSI escapes) — safe for logs / files /
    /// JSON.  See [`crate::semantics::render`] for the template DSL.
    pub fn render_formula(&self, formula: &AstNode, language: &str) -> RenderReport {
        self.layer.semantic().render_formula(formula, language)
    }

    /// Same as [`render_formula`](Self::render_formula) but wraps variables,
    /// `&%Symbol` cross-references, negations, and structural operators in ANSI
    /// colour escapes for terminal output.
    pub fn render_formula_colored(&self, formula: &AstNode, language: &str) -> RenderReport {
        self.layer.semantic().render_formula_colored(formula, language)
    }
}

// -- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KnowledgeBase;

    /// Load a tiny in-memory KB with just enough `format` /
    /// `termFormat` to exercise the template DSL.
    fn kb_with_english_templates(extras: &str) -> KnowledgeBase {
        let mut kb = KnowledgeBase::new();
        let kif = format!(r#"
            (termFormat EnglishLanguage Dog "dog")
            (termFormat EnglishLanguage Animal "animal")
            (termFormat EnglishLanguage Fido "fido")
            (termFormat EnglishLanguage Juno "juno")
            (termFormat EnglishLanguage attribute "attribute")
            (termFormat EnglishLanguage instance "instance")
            (termFormat EnglishLanguage subclass "subclass")
            (termFormat EnglishLanguage Friendly "friendly")
            (format EnglishLanguage instance "%1 is %n an instance of %2")
            (format EnglishLanguage subclass "%1 is %n a subclass of %2")
            (format EnglishLanguage attribute "%1 has %n the attribute %2")
            (format EnglishLanguage likes "%1 %n{{does not}} &%likes %2")
            (termFormat EnglishLanguage likes "likes")
            {extras}
        "#);
        // Ingest as a *file* source (not `tell`): `tell` registers an inline
        // (`__inline(N)__`) source, and inline assertions are transient
        // super-hypotheses that `make_session_axiomatic` refuses to promote
        // (`PromoteError::ContainsInline`).  A file source under session
        // `"tests.kif"` promotes cleanly so the `format`/`termFormat` facts
        // become base axioms the renderer reads.
        let r = kb.reload_kif(&kif, &std::path::PathBuf::from("tests.kif"), "tests.kif");
        assert!(r.ok, "load failed: {:?}", r.diagnostics);
        let r = kb.make_session_axiomatic(
            "tests.kif"
        );
        assert!(matches!(r, Ok(_)), "promote failed: {r:?}");
        kb
    }

    fn parse_kif_formula(kif: &str) -> AstNode {
        use crate::parse::parse_document;
        let doc = parse_document("test", kif, crate::Parser::Kif);
        assert!(!doc.has_errors(), "parse errors: {:?}", doc.parse_errors);
        doc.ast.into_iter().next().expect("at least one root").as_stmt().cloned().expect("doc stmt")
    }

    #[test]
    fn positive_binary_predicate() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula("(instance Fido Dog)");
        let r = kb.render_formula(&f, "EnglishLanguage");
        assert_eq!(r.rendered, "fido is an instance of dog");
        assert!(r.missing.is_empty(), "unexpected misses: {:?}", r.missing);
    }

    #[test]
    fn negated_n_marker_becomes_not() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula("(not (instance Fido Dog))");
        let r = kb.render_formula(&f, "EnglishLanguage");
        assert_eq!(r.rendered, "fido is not an instance of dog");
        assert!(r.missing.is_empty(), "unexpected misses: {:?}", r.missing);
    }

    #[test]
    fn custom_negation_phrase() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula("(not (likes Fido Juno))");
        let r = kb.render_formula(&f, "EnglishLanguage");
        assert!(r.rendered.contains("does not"), "got: {}", r.rendered);
        assert!(r.rendered.contains("likes"), "got: {}", r.rendered);
    }

    #[test]
    fn cross_reference_uses_termformat() {
        // The `instance` template references `attribute` nowhere, but
        // we can smoke-test cross-reference with a bespoke format
        // string injected via `extras`.
        let kb = kb_with_english_templates(
            r#"(format EnglishLanguage hasAttr "%1 has the &%attribute %2")
               (termFormat EnglishLanguage hasAttr "has attribute")"#
        );
        let f = parse_kif_formula("(hasAttr Fido Friendly)");
        let r = kb.render_formula(&f, "EnglishLanguage");
        assert_eq!(r.rendered, "fido has the attribute friendly");
    }

    #[test]
    fn conjunction_rendering() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula("(and (instance Fido Dog) (instance Fido Animal))");
        let r = kb.render_formula(&f, "EnglishLanguage");
        assert_eq!(
            r.rendered,
            "fido is an instance of dog and fido is an instance of animal"
        );
    }

    #[test]
    fn implication_rendering() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula("(=> (instance Fido Dog) (instance Fido Animal))");
        let r = kb.render_formula(&f, "EnglishLanguage");
        assert_eq!(
            r.rendered,
            "if fido is an instance of dog then fido is an instance of animal"
        );
    }

    #[test]
    fn quantifier_rendering() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula("(forall (?X) (instance ?X Animal))");
        let r = kb.render_formula(&f, "EnglishLanguage");
        assert_eq!(
            r.rendered,
            "for every ?X , ?X is an instance of animal"
        );
    }

    #[test]
    fn missing_format_records_miss_and_falls_back() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula("(undefinedRel Fido Dog)");
        let r = kb.render_formula(&f, "EnglishLanguage");
        assert!(r.missing.iter().any(|m| m.starts_with("format:")
            && m.contains("undefinedRel")),
            "expected format:undefinedRel miss, got: {:?}", r.missing);
        // Best-effort rendering still includes the args.
        assert!(r.rendered.contains("fido"), "got: {}", r.rendered);
        assert!(r.rendered.contains("dog"),  "got: {}", r.rendered);
    }

    #[test]
    fn missing_termformat_records_miss() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula("(instance UnknownThing Dog)");
        let r = kb.render_formula(&f, "EnglishLanguage");
        assert!(
            r.missing.iter().any(|m| m == "termFormat:UnknownThing"),
            "expected termFormat:UnknownThing miss, got: {:?}", r.missing
        );
    }

    #[test]
    fn logical_constants_render_without_being_flagged_missing() {
        // `$true` / `$false` can't be lexed by the KIF parser (`$` is
        // not a legal KIF character), so they only reach the renderer
        // if an upstream component synthesises them — we cover them
        // defensively in `render_term_symbol` and exercise that code
        // path by constructing `AstNode::Symbol` directly below.  The
        // bare `true` / `false` / `True` / `False` cases *are*
        // parseable and represent what actually shows up in Vampire
        // refutation proofs.
        let kb = kb_with_english_templates("");
        for name in ["true", "false", "True", "False"] {
            let f = parse_kif_formula(name);
            let r = kb.render_formula(&f, "EnglishLanguage");
            let expected = name.to_lowercase();
            assert_eq!(
                r.rendered, expected,
                "expected {} → {:?}, got {:?}", name, expected, r.rendered
            );
            assert!(
                !r.missing.iter().any(|m| m.contains(name)),
                "logical constant `{}` should not register as missing: {:?}",
                name, r.missing,
            );
        }
        // Direct-construction path for the `$`-prefixed variants.
        use crate::parse::ast::Span;
        for name in ["$true", "$false"] {
            let node = AstNode::Symbol {
                name: name.to_string(),
                span: Span::point("test".to_string(), 0, 0, 0),
            };
            let r = kb.render_formula(&node, "EnglishLanguage");
            let expected = name.trim_start_matches('$').to_lowercase();
            assert_eq!(
                r.rendered, expected,
                "expected {} → {:?}, got {:?}", name, expected, r.rendered
            );
            assert!(r.missing.is_empty(),
                "logical constant `{}` should not register as missing: {:?}",
                name, r.missing,
            );
        }
    }

    // -- Colour rendering --------------------------------------------------
    //
    // `render_formula_colored` wraps lexical classes in ANSI escapes.
    // We assert presence-and-placement of each class (variable, linked
    // symbol via &%, negation, operator) rather than exact bytes, so
    // the palette can shift without breaking these tests.  The colour
    // codes are the module constants — we check for both the opening
    // and the matching `ANSI_RESET` to catch unbalanced emission.

    /// Strip every `\x1b[…m` sequence from a rendered string.  Gives
    /// back the "plain" view so we can assert structural text stays
    /// intact regardless of coloration.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'[') {
                // Skip CSI sequence: ESC [ ... m
                let mut j = i + 2;
                while j < bytes.len() && bytes[j] != b'm' { j += 1; }
                i = j + 1;
                continue;
            }
            out.push(bytes[i] as char);
            i += 1;
        }
        out
    }

    #[test]
    fn coloured_stripped_equals_plain_render() {
        // The coloured output must reduce to the uncoloured output
        // when ANSI escapes are removed — guards against accidentally
        // dropping text while colouring.  Checked across a mix of
        // no-colour (pure atom), colour-expected, and edge cases.
        let kb = kb_with_english_templates("");
        let cases = [
            "(instance Fido Dog)",                              // no colour expected
            "(not (instance Fido Dog))",                        // negation
            "(and (instance Fido Dog) (instance Fido Animal))", // connective
            "(=> (instance Fido Dog) (instance Fido Animal))",  // if/then
            "(forall (?X) (instance ?X Animal))",               // variables + connective
            "(not (likes Fido Juno))",                          // custom negation
        ];
        for kif in cases {
            let f = parse_kif_formula(kif);
            let plain = kb.render_formula(&f, "EnglishLanguage").rendered;
            let coloured = kb.render_formula_colored(&f, "EnglishLanguage").rendered;
            assert_eq!(
                strip_ansi(&coloured), plain,
                "colour stripping must recover plain output for {}", kif
            );
        }
    }

    #[test]
    fn formulas_with_colourable_elements_actually_gain_colour() {
        // At least one ANSI open must appear whenever the formula
        // contains any of: variable, `&%`, negation, or connective.
        // A bare `(predicate arg arg)` with no colourable elements
        // intentionally returns the same string as the plain path —
        // covered by a separate assertion below.
        let kb = kb_with_english_templates("");
        for kif in [
            "(not (instance Fido Dog))",
            "(and (instance Fido Dog) (instance Fido Animal))",
            "(=> (instance Fido Dog) (instance Fido Animal))",
            "(forall (?X) (instance ?X Animal))",
            "(not (likes Fido Juno))",
        ] {
            let f = parse_kif_formula(kif);
            let coloured = kb.render_formula_colored(&f, "EnglishLanguage").rendered;
            assert!(
                coloured.contains("\x1b["),
                "expected ANSI colour in {}: {:?}", kif, coloured
            );
        }

        // Conversely, a bare predicate with no variables, `&%`, or
        // connectives has nothing to colour — coloured == plain.
        let f = parse_kif_formula("(instance Fido Dog)");
        let plain    = kb.render_formula(&f, "EnglishLanguage").rendered;
        let coloured = kb.render_formula_colored(&f, "EnglishLanguage").rendered;
        assert_eq!(plain, coloured,
            "bare predicate with no colourable class should render identically");
    }

    #[test]
    fn variables_are_wrapped_in_magenta() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula("(forall (?X) (instance ?X Animal))");
        let r = kb.render_formula_colored(&f, "EnglishLanguage").rendered;
        // `?X` appears twice (once in the quantifier list, once in the
        // body).  Each occurrence must be wrapped in a matched pair.
        let magenta_opens = r.matches(super::ANSI_VAR).count();
        let resets        = r.matches(super::ANSI_RESET).count();
        assert!(magenta_opens >= 2, "expected ≥2 magenta opens, got {}: {:?}", magenta_opens, r);
        assert!(resets >= magenta_opens, "resets must cover opens: {:?}", r);
    }

    #[test]
    fn connectives_are_wrapped_in_cyan() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula(
            "(and (instance Fido Dog) (=> (instance Fido Dog) (instance Fido Animal)))",
        );
        let r = kb.render_formula_colored(&f, "EnglishLanguage").rendered;
        // `and`, `if`, `then` should all be coloured.
        assert!(r.contains(&format!("{}and{}", super::ANSI_OP, super::ANSI_RESET)),
            "missing cyan `and`: {:?}", r);
        assert!(r.contains(&format!("{}if{}", super::ANSI_OP, super::ANSI_RESET)),
            "missing cyan `if`: {:?}", r);
        assert!(r.contains(&format!("{}then{}", super::ANSI_OP, super::ANSI_RESET)),
            "missing cyan `then`: {:?}", r);
    }

    #[test]
    fn negation_n_marker_is_wrapped_in_red() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula("(not (instance Fido Dog))");
        let r = kb.render_formula_colored(&f, "EnglishLanguage").rendered;
        assert!(r.contains(&format!("{}not{}", super::ANSI_NEG, super::ANSI_RESET)),
            "missing red `not`: {:?}", r);
    }

    #[test]
    fn custom_negation_phrase_is_wrapped_in_red() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula("(not (likes Fido Juno))");
        let r = kb.render_formula_colored(&f, "EnglishLanguage").rendered;
        assert!(r.contains(&format!("{}does not{}", super::ANSI_NEG, super::ANSI_RESET)),
            "missing red `does not`: {:?}", r);
    }

    #[test]
    fn cross_reference_is_wrapped_in_bright_blue() {
        let kb = kb_with_english_templates(
            r#"(format EnglishLanguage hasAttr "%1 has the &%attribute %2")
               (termFormat EnglishLanguage hasAttr "has attribute")"#
        );
        let f = parse_kif_formula("(hasAttr Fido Friendly)");
        let r = kb.render_formula_colored(&f, "EnglishLanguage").rendered;
        assert!(r.contains(&format!("{}attribute{}", super::ANSI_LINKED, super::ANSI_RESET)),
            "missing bright-blue `attribute`: {:?}", r);
    }

    #[test]
    fn it_is_not_the_case_fallback_is_red() {
        // A template-less predicate wrapped in `not` triggers the
        // "it is not the case that …" path, which must be red.
        let kb = kb_with_english_templates(
            r#"(format EnglishLanguage bareRel "%1 bareRel %2")
               (termFormat EnglishLanguage bareRel "bare rel")"#,
        );
        let f = parse_kif_formula("(not (bareRel Fido Dog))");
        let r = kb.render_formula_colored(&f, "EnglishLanguage").rendered;
        assert!(
            r.contains(&format!("{}it is not the case that{}", super::ANSI_NEG, super::ANSI_RESET)),
            "missing red `it is not the case that`: {:?}", r
        );
    }

    #[test]
    fn missing_language_reports_everything() {
        let kb = kb_with_english_templates("");
        let f = parse_kif_formula("(instance Fido Dog)");
        let r = kb.render_formula(&f, "KlingonLanguage");
        // Every symbol should be missing in Klingon.
        assert!(r.missing.iter().any(|m| m == "format:instance"));
        assert!(r.missing.iter().any(|m| m == "termFormat:Fido"));
        assert!(r.missing.iter().any(|m| m == "termFormat:Dog"));
    }
}
