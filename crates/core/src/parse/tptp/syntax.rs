// crates/core/src/parse/tptp/syntax.rs
//
// The shared TPTP lexical layer: the one place that knows how TPTP spells its
// tokens.  Both emitters consume it —
//
//   * `parse/tptp/dis.rs`  (AstNode → untyped TPTP, the dialect seam), and
//   * `trans/ir/tptp_emit.rs` (typed IR → FOF/TFF, the external-prover path),
//
// so the two can never drift on operator glyphs, the `$true`/`$false` atoms, or
// word/variable armoring.  It also hosts the single [`TptpLang`] enum shared by
// the dialect, the translation layer, and the public API.

/// Target TPTP language, shared by emission (`parse::dialect`), the translation
/// layer (`trans`), and the public API.
///
/// `cnf ⊂ fof ⊆ tff`, so `Fof` is the universal untyped fallback.  `Auto` on
/// **emit** derives the smallest language that fits the whole document (`Cnf` if
/// every statement is clausal, else `Fof`) — homogeneous by construction.
/// `Auto` on **parse** means "detect from the first statement and require the
/// file to be homogeneous."  The translation layer only ever runs in `Fof`/`Tff`
/// (it builds a typed IR, not clauses); `Cnf`/`Auto` never reach it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TptpLang {
    Cnf,
    #[default]
    Fof,
    Tff,
    Auto,
}

impl TptpLang {
    /// The leading language keyword (`cnf`/`fof`/`tff`).  `Auto` has no keyword
    /// of its own and reports the untyped fallback `fof`.
    pub fn as_str(self) -> &'static str {
        match self {
            TptpLang::Cnf  => "cnf",
            TptpLang::Fof  => "fof",
            TptpLang::Tff  => "tff",
            TptpLang::Auto => "fof",
        }
    }

    /// Whether this language carries sort annotations (`tff`).  The translation
    /// layer dispatches typed vs. untyped emission on this; everything that is
    /// not `Tff` is untyped.
    pub fn is_typed(self) -> bool {
        matches!(self, TptpLang::Tff)
    }
}

/// Detect a TPTP problem's language from its first annotated-formula keyword
/// (`cnf`/`fof`/`tff`), skipping `%` line and `/* … */` block comments.  This is
/// how proof output mirrors its input dialect: a `tff` problem yields a `tff`
/// proof, a `cnf` problem a `cnf` proof, etc.
///
/// `thf`/`tcf` (unsupported elsewhere) and any non-TPTP input map to `None`, so
/// callers fall back to the universal untyped default (`Fof`).
pub fn detect_tptp_lang(text: &str) -> Option<TptpLang> {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => while i < b.len() && b[i] != b'\n' { i += 1; },
            b'/' if i + 1 < b.len() && b[i + 1] == b'*' => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') { i += 1; }
                i += 2;
            }
            c if c.is_ascii_alphabetic() => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') { i += 1; }
                let word = &text[start..i];
                let mut j = i;
                while j < b.len() && b[j].is_ascii_whitespace() { j += 1; }
                // The first `keyword(` token at top level is the language.
                if j < b.len() && b[j] == b'(' {
                    match word {
                        "cnf" => return Some(TptpLang::Cnf),
                        "fof" => return Some(TptpLang::Fof),
                        "tff" => return Some(TptpLang::Tff),
                        _ => {} // include / unrecognized — keep scanning
                    }
                }
            }
            _ => i += 1,
        }
    }
    None
}

// ── Token glyphs ─────────────────────────────────────────────────────────────
//
// The exact surface spellings.  Binary connectives include their surrounding
// spaces so callers can `join`/concatenate without re-deciding spacing.

/// `$true` / `$false` propositional constants.
pub const TRUE:  &str = "$true";
pub const FALSE: &str = "$false";

/// Negation prefix.
pub const NOT: &str = "~";
/// Conjunction / disjunction separators (spaced).
pub const AND: &str = " & ";
pub const OR:  &str = " | ";
/// Implication / biconditional connectives (spaced).
pub const IMPLIES: &str = " => ";
pub const IFF:     &str = " <=> ";
/// Equality connective (spaced).
pub const EQ: &str = " = ";
/// Universal / existential quantifier prefixes.
pub const FORALL: &str = "!";
pub const EXISTS: &str = "?";

// ── Word / variable armoring ─────────────────────────────────────────────────

/// TPTP lower-word: a bare identifier that begins lowercase and is otherwise
/// `[A-Za-z0-9_]`.  Anything else is single-quoted (with `\` and `'` escaped).
pub fn lower_word(name: &str) -> String {
    let mut chars = name.chars();
    let ok = matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

/// TPTP variable (upper-word).  Names that aren't already a legal upper-word get
/// an `X_` armor prefix with non-alphanumerics flattened to `_`.
pub fn variable(name: &str) -> String {
    if name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        name.to_string()
    } else {
        format!("X_{}", name.replace(|c: char| !c.is_ascii_alphanumeric(), "_"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lower_word_quotes_foreign() {
        assert_eq!(lower_word("human"), "human");
        assert_eq!(lower_word("Human"), "'Human'");
        assert_eq!(lower_word("s__Foo"), "s__Foo"); // SUMO-mangled names are legal
        assert_eq!(lower_word("a b"), "'a b'");
        assert_eq!(lower_word("it's"), "'it\\'s'");
    }

    #[test]
    fn variable_armors_foreign() {
        assert_eq!(variable("X"), "X");
        assert_eq!(variable("VAR1"), "VAR1");
        assert_eq!(variable("?x"), "X__x");
        assert_eq!(variable("lower"), "X_lower");
    }

    #[test]
    fn detect_lang_picks_first_keyword() {
        assert_eq!(detect_tptp_lang("fof(a, axiom, p)."), Some(TptpLang::Fof));
        assert_eq!(detect_tptp_lang("cnf(c, axiom, p)."), Some(TptpLang::Cnf));
        assert_eq!(detect_tptp_lang("tff(t, type, p: $o)."), Some(TptpLang::Tff));
        // Comments are skipped; the first real statement wins.
        assert_eq!(detect_tptp_lang("% fof(x) in a comment\n/* tff */\ncnf(c, axiom, p)."),
            Some(TptpLang::Cnf));
        // Non-TPTP input → None (caller defaults to fof).
        assert_eq!(detect_tptp_lang("(instance Foo Bar)"), None);
        assert_eq!(detect_tptp_lang(""), None);
    }

    #[test]
    fn lang_keyword_and_typing() {
        assert_eq!(TptpLang::Cnf.as_str(), "cnf");
        assert_eq!(TptpLang::Fof.as_str(), "fof");
        assert_eq!(TptpLang::Tff.as_str(), "tff");
        assert_eq!(TptpLang::Auto.as_str(), "fof");
        assert!(TptpLang::Tff.is_typed());
        assert!(!TptpLang::Fof.is_typed());
        assert_eq!(TptpLang::default(), TptpLang::Fof);
    }
}
