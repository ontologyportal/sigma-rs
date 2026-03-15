// crates/sumo-kb/src/tptp.rs
//
// TPTP (FOF/TFF) output generation.
//
// Ported from sumo-parser-core/src/tptp.rs.
// Changes: `&KnowledgeBase` → `&SemanticLayer`; raw `sentences[sid as usize]`
// → `sentences[store.sent_idx(sid)]`; session filtering decoupled.

use std::collections::HashSet;

use crate::kif_store::KifStore;
use crate::semantic::SemanticLayer;
use crate::types::{Element, Literal, SentenceId, SymbolId};
use crate::tokenizer::OpKind;

// ── TPTP identifier conventions ───────────────────────────────────────────────

pub const TPTP_SYMBOL_PREFIX:   &str = "s__";
pub const TPTP_VARIABLE_PREFIX: &str = "V__";
pub const TPTP_MENTION_SUFFIX:  &str = "__m";
const FN_SUFF: &str = "Fn";

/// TPTP language variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TptpLang {
    #[default]
    Fof,
    Tff,
}

impl TptpLang {
    pub fn as_str(self) -> &'static str {
        match self {
            TptpLang::Fof => "fof",
            TptpLang::Tff => "tff",
        }
    }
}

/// Options controlling TPTP output.
#[derive(Debug, Clone)]
pub struct TptpOptions {
    pub lang:         TptpLang,
    /// Wrap free variables in `?` (existential) instead of `!` (universal).
    pub query:        bool,
    /// Replace numeric literals with `n__N` tokens (default false).
    pub hide_numbers: bool,
    /// Head predicates whose sentences are omitted from KB output.
    pub excluded:     HashSet<String>,
}

impl Default for TptpOptions {
    fn default() -> Self {
        let mut excluded = HashSet::new();
        excluded.insert("documentation".to_string());
        excluded.insert("domain".to_string());
        excluded.insert("format".to_string());
        excluded.insert("termFormat".to_string());
        excluded.insert("externalImage".to_string());
        excluded.insert("relatedExternalConcept".to_string());
        excluded.insert("relatedInternalConcept".to_string());
        excluded.insert("formerName".to_string());
        excluded.insert("abbreviation".to_string());
        excluded.insert("conventionalShortName".to_string());
        excluded.insert("conventionalLongName".to_string());
        TptpOptions { lang: TptpLang::default(), query: false, hide_numbers: false, excluded }
    }
}

impl TptpOptions {
    pub fn default_with_hide_numbers() -> Self {
        Self { hide_numbers: true, ..Self::default() }
    }
}

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

fn translate_symbol(
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

fn translate_variable(kif_name: &str) -> String {
    format!("{}{}", TPTP_VARIABLE_PREFIX, kif_name.replace('-', "_"))
}

fn translate_literal(lit: &Literal, opts: &TptpOptions) -> String {
    match lit {
        Literal::Str(s) => {
            let inner = &s[1..s.len()-1];
            format!("'{}'", inner
                .chars()
                .filter(|&c| c != '\'')
                .map(|c| if matches!(c, '\n' | '\t' | '\r' | '\x0C') { ' ' } else { c })
                .collect::<String>())
        }
        Literal::Number(n) => {
            if opts.hide_numbers && opts.lang != TptpLang::Tff {
                format!("n__{}", n.replace('.', "_").replace('-', "_"))
            } else {
                n.clone()
            }
        }
    }
}

// ── Free-variable collection ──────────────────────────────────────────────────

fn collect_all_vars(sid: SentenceId, store: &KifStore, out: &mut HashSet<String>) {
    for elem in &store.sentences[store.sent_idx(sid)].elements {
        match elem {
            Element::Variable { name, .. } => { out.insert(name.clone()); }
            Element::Sub(sub) => collect_all_vars(*sub, store, out),
            _ => {}
        }
    }
}

// ── Recursive translation ─────────────────────────────────────────────────────

fn translate_element(
    elem:       &Element,
    store:      &KifStore,
    opts:       &TptpOptions,
    layer:      &SemanticLayer,
    as_formula: bool,
) -> String {
    match elem {
        Element::Symbol(id) => {
            let sym_str = translate_symbol(store.sym_name(*id), false, Some(*id), layer);
            if as_formula {
                format!("{}holds({})", TPTP_SYMBOL_PREFIX, sym_str)
            } else {
                sym_str
            }
        }
        Element::Variable { name, .. } => {
            let var_str = translate_variable(name);
            if as_formula {
                format!("{}holds({})", TPTP_SYMBOL_PREFIX, var_str)
            } else {
                var_str
            }
        }
        Element::Literal(lit) => translate_literal(lit, opts),
        Element::Sub(sid)     => translate_sentence(*sid, store, opts, layer, as_formula),
        Element::Op(op) => {
            if as_formula {
                op.name().to_owned()
            } else {
                translate_symbol(op.name(), false, None, layer)
            }
        }
    }
}

fn translate_sentence(
    sid:        SentenceId,
    store:      &KifStore,
    opts:       &TptpOptions,
    layer:      &SemanticLayer,
    as_formula: bool,
) -> String {
    let sentence = &store.sentences[store.sent_idx(sid)];
    if sentence.is_operator() {
        return translate_operator_sentence(sid, store, opts, layer, as_formula);
    }

    match sentence.elements.first() {
        Some(Element::Symbol(head_id)) => {
            let head_id   = *head_id;
            let head_name = store.sym_name(head_id);
            let args: Vec<String> = sentence.elements[1..]
                .iter()
                .map(|e| translate_element(e, store, opts, layer, false))
                .collect();
            if as_formula {
                let head_mention = translate_symbol(head_name, false, Some(head_id), layer);
                let mut holds_args = vec![head_mention];
                holds_args.extend(args);
                format!("{}holds({})", TPTP_SYMBOL_PREFIX, holds_args.join(","))
            } else {
                let head_str = translate_symbol(head_name, true, Some(head_id), layer);
                format!("{}({})", head_str, args.join(","))
            }
        }
        Some(Element::Variable { name, .. }) => {
            let var_str = translate_variable(name);
            let args: Vec<String> = std::iter::once(var_str)
                .chain(sentence.elements[1..].iter()
                    .map(|e| translate_element(e, store, opts, layer, false)))
                .collect();
            if as_formula {
                format!("{}holds({})", TPTP_SYMBOL_PREFIX, args.join(","))
            } else {
                format!("{}holds_app({})", TPTP_SYMBOL_PREFIX, args.join(","))
            }
        }
        _ => String::new(),
    }
}

fn translate_operator_sentence(
    sid:        SentenceId,
    store:      &KifStore,
    opts:       &TptpOptions,
    layer:      &SemanticLayer,
    as_formula: bool,
) -> String {
    let sentence = &store.sentences[store.sent_idx(sid)];
    let op = match sentence.op() {
        Some(op) => op.clone(),
        None     => return String::new(),
    };
    let args: Vec<&Element> = sentence.elements[1..].iter().collect();

    if !as_formula {
        let head_str = translate_symbol(op.name(), true, None, layer);
        let arg_strs: Vec<String> = args.iter()
            .map(|e| translate_element(e, store, opts, layer, false))
            .collect();
        return format!("{}({})", head_str, arg_strs.join(","));
    }

    match op {
        OpKind::And | OpKind::Or => {
            let tptp_op = if op == OpKind::And { "&" } else { "|" };
            let parts: Vec<String> = args.iter()
                .map(|e| translate_element(e, store, opts, layer, true))
                .collect();
            format!("({})", parts.join(&format!(" {} ", tptp_op)))
        }
        OpKind::Not => {
            let inner = translate_element(args[0], store, opts, layer, true);
            format!("~({})", inner)
        }
        OpKind::Implies => {
            let a = translate_element(args[0], store, opts, layer, true);
            let b = translate_element(args[1], store, opts, layer, true);
            format!("({} => {})", a, b)
        }
        OpKind::Iff => {
            let a = translate_element(args[0], store, opts, layer, true);
            let b = translate_element(args[1], store, opts, layer, true);
            format!("(({} => {}) & ({} => {}))", a, b, b, a)
        }
        OpKind::Equal => {
            let a = translate_element(args[0], store, opts, layer, false);
            let b = translate_element(args[1], store, opts, layer, false);
            format!("({} = {})", a, b)
        }
        OpKind::ForAll | OpKind::Exists => {
            if !as_formula {
                let head_str = translate_symbol(op.name(), true, None, layer);
                let vars_str = translate_element(args[0], store, opts, layer, false);
                let body_str = translate_element(args[1], store, opts, layer, false);
                return format!("{}({},{})", head_str, vars_str, body_str);
            }
            let q = if op == OpKind::ForAll { "!" } else { "?" };
            let vars: Vec<String> = match args[0] {
                Element::Sub(var_sid) => {
                    store.sentences[store.sent_idx(*var_sid)].elements.iter()
                        .filter_map(|e| {
                            if let Element::Variable { name, .. } = e {
                                Some(translate_variable(name))
                            } else {
                                None
                            }
                        })
                        .collect()
                }
                _ => Vec::new(),
            };
            let body = translate_element(args[1], store, opts, layer, true);
            if vars.is_empty() { body }
            else { format!("({} [{}] : ({}))", q, vars.join(", "), body) }
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Translate a single root sentence to a TPTP formula string.
///
/// All free variables are wrapped in a top-level universal quantifier
/// (`opts.query = true` → existential).
pub(crate) fn sentence_to_tptp(
    sid:   SentenceId,
    layer: &SemanticLayer,
    opts:  &TptpOptions,
) -> String {
    let result = translate_sentence(sid, &layer.store, opts, layer, true);

    let mut all_vars = HashSet::new();
    collect_all_vars(sid, &layer.store, &mut all_vars);
    if all_vars.is_empty() { return result; }

    let mut var_strs: Vec<String> = all_vars.into_iter()
        .map(|v| translate_variable(&v))
        .collect();
    var_strs.sort(); // deterministic
    let q = if opts.query { "?" } else { "!" };
    format!("( {} [{}] : ({}) )", q, var_strs.join(", "), result)
}

/// Render a set of root sentences as a TPTP string.
///
/// `all_roots`       — ordered list of all root SentenceIds to render.
/// `axiom_ids`       — sentences rendered with role `axiom`.
/// `assertion_ids`   — sentences rendered with role `hypothesis`.
/// Sentences in neither set (not in `axiom_ids` and not in `assertion_ids`) are skipped.
/// Pass `axiom_ids = all_roots.iter().copied().collect()` and `assertion_ids = empty`
/// for a pure-axiom KB.
pub(crate) fn kb_to_tptp(
    layer:          &SemanticLayer,
    kb_name:        &str,
    opts:           &TptpOptions,
    axiom_ids:      &HashSet<SentenceId>,
    assertion_ids:  &HashSet<SentenceId>,
) -> String {
    let safe_name = kb_name.replace(|c: char| !c.is_alphanumeric() && c != '_', "_");
    let header = format!(
        "% Articulate Software\n\
         % www.ontologyportal.org www.articulatesoftware.com\n\
         % This software released under the GNU Public License <http://www.gnu.org/copyleft/gpl.html>.\n\
         % Translation of KB {}\n",
        safe_name
    );

    let roots: Vec<SentenceId> = layer.store.roots.clone();
    let mut lines: Vec<String> = vec![header, String::new()];
    let mut written: HashSet<String> = HashSet::new();
    let mut idx = 1usize;
    let mut assertion_header_written = false;

    for sid in roots {
        let is_assertion = assertion_ids.contains(&sid);
        let is_axiom     = axiom_ids.contains(&sid);
        if !is_assertion && !is_axiom { continue; }

        // Skip excluded predicates
        let head_name = layer.store.sentences[layer.store.sent_idx(sid)]
            .head_symbol()
            .map(|id| layer.store.sym_name(id).to_owned());
        if let Some(ref name) = head_name {
            if opts.excluded.contains(name) { continue; }
        }

        let tptp = sentence_to_tptp(sid, layer, opts);
        if tptp.is_empty() || written.contains(&tptp) { continue; }
        written.insert(tptp.clone());

        if is_assertion && !assertion_header_written {
            lines.push(String::new());
            lines.push("% Assertions (tell)".to_owned());
            assertion_header_written = true;
        }

        let role = if is_assertion { "hypothesis" } else { "axiom" };
        lines.push(format!(
            "{}(kb_{}_{},{},({})). ",
            opts.lang.as_str(), safe_name, idx, role, tptp
        ));
        idx += 1;
    }

    lines.join("\n")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kif_store::{load_kif, KifStore};
    use crate::semantic::SemanticLayer;

    fn layer_from(kif: &str) -> SemanticLayer {
        let mut store = KifStore::default();
        load_kif(&mut store, kif, "test");
        SemanticLayer::new(store)
    }

    fn opts() -> TptpOptions {
        TptpOptions { hide_numbers: true, ..TptpOptions::default() }
    }

    #[test]
    fn simple_predicate() {
        let layer = layer_from("(subclass Human Animal)");
        let sid = layer.store.roots[0];
        let tptp = sentence_to_tptp(sid, &layer, &opts());
        assert!(tptp.contains("s__holds("),   "got: {}", tptp);
        assert!(tptp.contains("s__subclass"), "got: {}", tptp);
        assert!(tptp.contains("s__Human"),    "got: {}", tptp);
        assert!(tptp.contains("s__Animal"),   "got: {}", tptp);
    }

    #[test]
    fn free_variable_wrapper() {
        let layer = layer_from("(instance ?X Human)");
        let sid = layer.store.roots[0];
        let tptp = sentence_to_tptp(sid, &layer, &opts());
        assert!(tptp.contains("! [V__X]"), "got: {}", tptp);
    }

    #[test]
    fn query_mode_existential() {
        let layer = layer_from("(instance ?X Human)");
        let sid = layer.store.roots[0];
        let q_opts = TptpOptions { query: true, hide_numbers: true, ..TptpOptions::default() };
        let tptp = sentence_to_tptp(sid, &layer, &q_opts);
        assert!(tptp.contains("? [V__X]"), "got: {}", tptp);
    }

    #[test]
    fn empty_quantifier() {
        let layer = layer_from("(exists () (subclass Human Animal))");
        assert!(!layer.store.roots.is_empty());
        let sid = layer.store.roots[0];
        let tptp = sentence_to_tptp(sid, &layer, &opts());
        assert!(!tptp.contains("? []"),    "should not contain empty quantifier: {}", tptp);
        assert!( tptp.contains("s__holds("), "should contain body: {}", tptp);
    }

    #[test]
    fn implication() {
        let layer = layer_from("(=> (instance ?X Human) (instance ?X Animal))");
        let sid = layer.store.roots[0];
        let tptp = sentence_to_tptp(sid, &layer, &opts());
        assert!(tptp.contains("=>"), "got: {}", tptp);
    }

    #[test]
    fn mention_suffix_lowercase() {
        let layer = layer_from("(instance subclass BinaryRelation)");
        let sid = layer.store.roots[0];
        let tptp = sentence_to_tptp(sid, &layer, &opts());
        assert!(tptp.contains("s__subclass__m"), "got: {}", tptp);
    }

    #[test]
    fn nested_predicate_as_term() {
        let layer = layer_from("(holdsDuring ?I (attribute ?X LegalPersonhood))");
        let sid = layer.store.roots[0];
        let tptp = sentence_to_tptp(sid, &layer, &opts());
        assert!(tptp.contains("s__holds(s__holdsDuring__m,"), "got: {}", tptp);
        assert!(tptp.contains("s__attribute(V__X,s__LegalPersonhood)"), "got: {}", tptp);
    }

    #[test]
    fn nested_logical_operator() {
        let layer = layer_from("(holdsDuring ?I (and (attribute ?X LegalPersonhood) (instance ?X Human)))");
        let sid = layer.store.roots[0];
        let tptp = sentence_to_tptp(sid, &layer, &opts());
        assert!(tptp.contains("s__holds(s__holdsDuring__m,"), "got: {}", tptp);
        assert!(tptp.contains("s__and("), "missing s__and in: {}", tptp);
        assert!(!tptp.contains("&"), "found & inside term: {}", tptp);
    }

    #[test]
    fn bare_variable_as_formula() {
        let layer = layer_from("(=> (instance ?P Proposition) ?P)");
        let sid = layer.store.roots[0];
        let tptp = sentence_to_tptp(sid, &layer, &opts());
        assert!(tptp.contains("=> s__holds(V__P))"), "got: {}", tptp);
    }

    #[test]
    fn number_hidden_by_default() {
        let layer = layer_from("(lessThan ?X 42)");
        let sid = layer.store.roots[0];
        let tptp = sentence_to_tptp(sid, &layer, &opts());
        assert!(tptp.contains("n__42"), "got: {}", tptp);
    }

    #[test]
    fn kb_to_tptp_contains_axiom() {
        let layer = layer_from("(subclass Human Animal)");
        let axiom_ids: HashSet<SentenceId> = layer.store.roots.iter().copied().collect();
        let tptp = kb_to_tptp(&layer, "test", &opts(), &axiom_ids, &HashSet::new());
        assert!(tptp.contains(",axiom,"), "got: {}", tptp);
    }
}
