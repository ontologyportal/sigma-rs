use crate::semantic::SemanticLayer;
use crate::types::{Literal, SymbolId};
use super::options::{TptpLang, TptpOptions};

// ── TPTP identifier conventions ───────────────────────────────────────────────

pub const TPTP_SYMBOL_PREFIX:   &str = "s__";
pub const TPTP_VARIABLE_PREFIX: &str = "V__";
pub const TPTP_MENTION_SUFFIX:  &str = "__m";
const FN_SUFF: &str = "Fn";

// ── Symbol name translation ───────────────────────────────────────────────────

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
    name.chars().next().map(|c| c.is_lowercase()).unwrap_or(false)
        || name.ends_with(FN_SUFF)
}

pub(crate) fn translate_symbol(
    name:     &str,
    has_args: bool,
    sym_id:   Option<SymbolId>,
    layer:    &SemanticLayer,
) -> String {
    let mut result = name.replace('.', "_").replace('-', "_");
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

// Simple TPTP translation for a variable (its name will be the interned variable name)
pub(crate) fn translate_variable(kif_name: &str) -> String {
    // Just prefix it with the TPTP prefix
    format!("{}{}", TPTP_VARIABLE_PREFIX, kif_name.replace('-', "_"))
}

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
                format!("n__{}", n.replace('.', "_").replace('-', "_"))
            } else {
                n.clone()
            }
        }
    }
}
