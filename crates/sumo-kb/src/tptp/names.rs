// -- names.rs ------------------------------------------------------------------
//
// KIF -> TPTP identifier encoding.
//
// TPTP has strict lexical rules:
//   - Lower-case start  -> uninterpreted constant (a.k.a. "functor")
//   - Upper-case start  -> variable
//   - Digits/special    -> not allowed in plain tokens; must be quoted
//
// SUMO/KIF names can start with upper or lower case and may contain dots and
// hyphens.  The conventions below make every SUMO name safe for TPTP output.
//
// -- Encoding conventions -----------------------------------------------------
//
//  SUMO symbol "Foo"       -> s__Foo         (s__ prefix forces lower-case start)
//  SUMO variable "?Bar"    -> V__Bar         (V__ prefix, upper-case = TPTP var)
//  Dot / hyphen in name    -> replaced by _
//  Predicate in term pos.  -> s__pred__m     (__m = "mention" constant, type $i)
//  Number 42               -> 42 (TFF) or n__42 (FOF with hide_numbers)

use crate::semantic::SemanticLayer;
use crate::types::{Literal, SymbolId};
use super::options::{TptpLang, TptpOptions};

// -- TPTP identifier conventions -----------------------------------------------

/// All SUMO symbols are prefixed with `s__` so they start with a lower-case
/// letter and are therefore interpreted as TPTP constants/functors, not variables.
pub const TPTP_SYMBOL_PREFIX:   &str = "s__";

/// TPTP variables must start with an upper-case letter.  KIF variables start
/// with `?` (regular) or `@` (row) -- we drop the prefix and add `V__`.
pub const TPTP_VARIABLE_PREFIX: &str = "V__";

/// The "mention" suffix `__m` is appended to relation/predicate symbols when
/// they appear in *term* position (i.e., as an argument to another predicate
/// or inside `holds`).  In FOF this is how higher-order predicates are
/// reified.  In TFF the `__m` constant carries type `$i` so Vampire knows it
/// is an individual, not a formula.
pub const TPTP_MENTION_SUFFIX:  &str = "__m";

/// SUMO function names conventionally end in "Fn" (e.g. `AdditionFn`).
/// This is checked by `needs_mention_suffix` so that bare `FooFn` in term
/// position also gets the `__m` treatment even if the taxonomy lookup fails.
const FN_SUFF: &str = "Fn";

// -- Symbol name translation ---------------------------------------------------

/// Determine whether `name` needs the `__m` mention suffix when it appears
/// in term position (i.e., without arguments / `has_args = false`).
///
/// A symbol needs the suffix when it is a relation, predicate, or function
/// (either by taxonomy lookup or by the lower-case/`Fn` heuristic).
/// Pure class names like `Animal` or `Human` do NOT get the suffix.
fn needs_mention_suffix(
    name:   &str,
    sym_id: Option<SymbolId>,
    layer:  &SemanticLayer,
) -> bool {
    if let Some(id) = sym_id {
        if layer.is_relation(id) || layer.is_predicate(id) || layer.is_function(id) {
            return true;
        }
    }
    // Heuristic fallback when taxonomy information is absent:
    // lower-case first letter typically indicates a relation/predicate,
    // and names ending in "Fn" are SUMO functions.
    name.chars().next().map(|c| c.is_lowercase()).unwrap_or(false)
        || name.ends_with(FN_SUFF)
}

/// Translate a SUMO symbol name to a TPTP functor string.
///
/// `has_args` -- true when the symbol is followed by arguments (predicate call
///              or function application).  False when the symbol appears as a
///              bare constant (mention form) -- triggers the `__m` suffix for
///              relations/predicates/functions.
///
/// `sym_id`   -- optional SymbolId for taxonomy lookups; `None` in contexts
///              where we only have the name (e.g. operator-as-function).
pub(crate) fn translate_symbol(
    name:     &str,
    has_args: bool,
    sym_id:   Option<SymbolId>,
    layer:    &SemanticLayer,
) -> String {
    // Sanitise: dots and hyphens are illegal in unquoted TPTP atoms.
    let mut result = name.replace('.', "_").replace('-', "_");
    // SUMO logical operators share names with TPTP keywords in some contexts;
    // map them to their plain-word equivalents for safety.
    match name {
        "=>"     => result = "implies".to_string(),
        "<=>"    => result = "iff".to_string(),
        "and"    => result = "and".to_string(),
        "or"     => result = "or".to_string(),
        "not"    => result = "not".to_string(),
        "forall" => result = "forall".to_string(),
        "exists" => result = "exists".to_string(),
        "equal"  => result = "equal".to_string(),
        _ => {}
    }
    let suffix = if !has_args && needs_mention_suffix(name, sym_id, layer) {
        TPTP_MENTION_SUFFIX
    } else {
        ""
    };
    format!("{}{}{}", TPTP_SYMBOL_PREFIX, result, suffix)
}

/// Translate a KIF variable name (with its interned scope suffix) to a TPTP
/// variable token.  Hyphens are replaced with underscores so the name is a
/// valid TPTP upper-case word.
pub(crate) fn translate_variable(kif_name: &str) -> String {
    // Just prefix it with the TPTP prefix
    format!("{}{}", TPTP_VARIABLE_PREFIX, kif_name.replace('-', "_"))
}

/// Translate a KIF literal (string or number) to a TPTP token.
///
/// Strings become single-quoted atoms with whitespace normalised.
/// Numbers are emitted as-is in TFF mode (where `$int`/`$real` are native)
/// or encoded as `n__N` symbol constants in FOF mode when `hide_numbers` is
/// set (numbers otherwise have no built-in meaning in FOF).
pub(crate) fn translate_literal(lit: &Literal, opts: &TptpOptions) -> String {
    match lit {
        Literal::Str(s) => {
            let inner = &s[1..s.len()-1];
            // wrap in single quotes, collapse whitespace
            format!("'{}'", inner
                .chars()
                .filter(|&c| c != '\'')
                .map(|c| if matches!(c, '\n' | '\t' | '\r' | '\x0C') { ' ' } else { c })
                .collect::<String>())
        }
        // If the user wants numbers, just return the number otherwise return
        // symbol representing the number
        Literal::Number(n) => {
            if opts.hide_numbers && opts.lang != TptpLang::Tff {
                // Encode the number as a TPTP symbol constant.
                // Dots become underscores (TPTP identifiers allow `_` not `.`).
                // Negatives get a `neg_` prefix so that `-42` and `42` encode
                // to distinct symbols (`n__neg_42` vs `n__42`) and the sign
                // survives the round-trip through unmangle in tptp/kif.rs.
                let (sign, digits) = if let Some(pos) = n.strip_prefix('-') {
                    ("neg_", pos)
                } else {
                    ("", n.as_str())
                };
                format!("n__{}{}", sign, digits.replace('.', "_"))
            } else {
                n.clone()
            }
        }
    }
}
