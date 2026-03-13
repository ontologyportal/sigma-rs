/// TPTP (FOF/TFF) output generation.
///
/// Mirrors the logic of `src/tptp.ts` in the TypeScript implementation.
use std::collections::HashSet;

use crate::kb::KnowledgeBase;
use crate::store::{Element, KifStore, Literal, SentenceId, SymbolId};
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
    /// Replace numeric literals with `n__N` tokens (default true).
    pub hide_numbers: bool,
    /// Head predicates whose sentences are omitted from KB output.
    pub excluded:     HashSet<String>,
}

impl Default for TptpOptions {
    fn default() -> Self {
        let mut default_excluded = HashSet::new();
        default_excluded.insert("documentation".to_string());
        default_excluded.insert("domain".to_string());
        default_excluded.insert("format".to_string());
        default_excluded.insert("termFormat".to_string());
        default_excluded.insert("externalImage".to_string());
        default_excluded.insert("relatedExternalConcept".to_string());
        default_excluded.insert("relatedInternalConcept".to_string());
        default_excluded.insert("formerName".to_string());
        default_excluded.insert("abbreviation".to_string());
        default_excluded.insert("conventionalShortName".to_string());
        default_excluded.insert("conventionalLongName".to_string());

        return TptpOptions { 
            lang: TptpLang::default(),
            query: false,
            hide_numbers: false, 
            excluded: default_excluded
        };
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
    kb:     &KnowledgeBase,
) -> bool {
    if let Some(id) = sym_id {
        if kb.is_relation(id) || kb.is_predicate(id) || kb.is_function(id) {
            return true;
        }
    }
    // Heuristic fallback
    name.chars().next().map(|c| c.is_lowercase()).unwrap_or(false)
        || name.ends_with(FN_SUFF)
}

fn translate_symbol(
    name:     &str,
    has_args: bool,
    sym_id:   Option<SymbolId>,
    kb:       &KnowledgeBase,
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
    
    let suffix = if !has_args && needs_mention_suffix(name, sym_id, kb) {
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
            // Sanitise whitespace and single-quotes
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
    for elem in &store.sentences[sid].elements {
        match elem {
            Element::Variable { name, .. } => { out.insert(name.clone()); }
            Element::Sub(sub) => collect_all_vars(*sub, store, out),
            _ => {}
        }
    }
}

// ── Recursive translation ─────────────────────────────────────────────────────

fn translate_element(
    elem:  &Element,
    store: &KifStore,
    opts:  &TptpOptions,
    kb:    &KnowledgeBase,
    as_formula: bool,
) -> String {
    match elem {
        Element::Symbol(id) => {
            let sym_str = translate_symbol(store.sym_name(*id), false, Some(*id), kb);
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
        Element::Sub(sid) => translate_sentence(*sid, store, opts, kb, as_formula),
        Element::Op(op) => {
            if as_formula {
                op.name().to_owned()
            } else {
                translate_symbol(op.name(), false, None, kb)
            }
        }
    }
}

fn translate_sentence(
    sid:   SentenceId,
    store: &KifStore,
    opts:  &TptpOptions,
    kb:    &KnowledgeBase,
    as_formula: bool,
) -> String {
    let sentence = &store.sentences[sid];

    if sentence.is_operator() {
        return translate_operator_sentence(sid, store, opts, kb, as_formula);
    }

    match sentence.elements.first() {
        Some(Element::Symbol(head_id)) => {
            // Regular predicate / function application.
            let head_id = *head_id;
            let head_name = store.sym_name(head_id);
            
            let args: Vec<String> = sentence.elements[1..]
                .iter()
                .map(|e| translate_element(e, store, opts, kb, false))
                .collect();

            if as_formula {
                // Formula usage: wrap in s__holds to avoid sort conflicts in provers.
                let head_mention = translate_symbol(head_name, false, Some(head_id), kb);
                let mut holds_args = vec![head_mention];
                holds_args.extend(args);
                format!("{}holds({})", TPTP_SYMBOL_PREFIX, holds_args.join(","))
            } else {
                // Term usage: direct application.
                let head_str = translate_symbol(head_name, true, Some(head_id), kb);
                format!("{}({})", head_str, args.join(","))
            }
        }
        Some(Element::Variable { name, .. }) => {
            // Variable-headed sentence: (?REL A B) → s__holds(V__REL, A, B).
            // This is the standard SUMO higher-order encoding for FOF.
            let var_str = translate_variable(name);
            let args: Vec<String> = std::iter::once(var_str)
                .chain(sentence.elements[1..].iter().map(|e| translate_element(e, store, opts, kb, false)))
                .collect();
            
            if as_formula {
                // Top-level or logical arg: use holds as a predicate
                format!("{}holds({})", TPTP_SYMBOL_PREFIX, args.join(","))
            } else {
                // Nested term: use holds_app as a function to avoid sort conflicts
                format!("{}holds_app({})", TPTP_SYMBOL_PREFIX, args.join(","))
            }
        }
        _ => String::new(),
    }
}

fn translate_operator_sentence(
    sid:   SentenceId,
    store: &KifStore,
    opts:  &TptpOptions,
    kb:    &KnowledgeBase,
    as_formula: bool,
) -> String {
    let sentence = &store.sentences[sid];
    let op = match sentence.op() {
        Some(op) => op.clone(),
        None     => return String::new(),
    };
    // args = elements after the leading Op
    let args: Vec<&Element> = sentence.elements[1..].iter().collect();

    if !as_formula {
        // Reify nested logical operators as function applications
        let head_str = translate_symbol(op.name(), true, None, kb);
        let arg_strs: Vec<String> = args.iter()
            .map(|e| translate_element(e, store, opts, kb, false))
            .collect();
        return format!("{}({})", head_str, arg_strs.join(","));
    }

    match op {
        OpKind::And | OpKind::Or => {
            let tptp_op = if op == OpKind::And { "&" } else { "|" };
            let parts: Vec<String> = args
                .iter()
                .map(|e| translate_element(e, store, opts, kb, true))
                .collect();
            format!("({})", parts.join(&format!(" {} ", tptp_op)))
        }

        OpKind::Not => {
            let inner = translate_element(args[0], store, opts, kb, true);
            format!("~({})", inner)
        }

        OpKind::Implies => {
            let a = translate_element(args[0], store, opts, kb, true);
            let b = translate_element(args[1], store, opts, kb, true);
            format!("({} => {})", a, b)
        }

        OpKind::Iff => {
            let a = translate_element(args[0], store, opts, kb, true);
            let b = translate_element(args[1], store, opts, kb, true);
            format!("(({} => {}) & ({} => {}))", a, b, b, a)
        }

        OpKind::Equal => {
            let a = translate_element(args[0], store, opts, kb, false);
            let b = translate_element(args[1], store, opts, kb, false);
            format!("({} = {})", a, b)
        }

        OpKind::ForAll | OpKind::Exists => {
            if !as_formula {
                // Reified quantifier: use functional form
                let head_str = translate_symbol(op.name(), true, None, kb);
                let vars_str = translate_element(args[0], store, opts, kb, false);
                let body_str = translate_element(args[1], store, opts, kb, false);
                return format!("{}({},{})", head_str, vars_str, body_str);
            }

            let q = if op == OpKind::ForAll { "!" } else { "?" };
            // args[0] = variable list sub-sentence, args[1] = body
            let vars: Vec<String> = match args[0] {
                Element::Sub(var_sid) => {
                    store.sentences[*var_sid].elements.iter().filter_map(|e| {
                        if let Element::Variable { name, .. } = e {
                            Some(translate_variable(name))
                        } else {
                            None
                        }
                    }).collect()
                }
                _ => Vec::new(),
            };
            let body = translate_element(args[1], store, opts, kb, true);
            if vars.is_empty() {
                body
            } else {
                format!("({} [{}] : ({}))", q, vars.join(", "), body)
            }
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Translate a single root sentence to a TPTP formula string.
///
/// All variables appearing anywhere in the sentence (including inside reified 
/// nested formulas) are wrapped in a top-level universal quantifier
/// (`opts.query = true` → existential).
pub fn sentence_to_tptp(
    sid:  SentenceId,
    kb:   &KnowledgeBase,
    opts: &TptpOptions,
) -> String {
    let result = translate_sentence(sid, &kb.store, opts, kb, true);

    // Quantify ALL variables at the top level
    let mut all_vars = HashSet::new();
    collect_all_vars(sid, &kb.store, &mut all_vars);
    if all_vars.is_empty() {
        return result;
    }

    let mut var_strs: Vec<String> = all_vars.into_iter().map(|v| translate_variable(&v)).collect();
    var_strs.sort(); // deterministic
    let q = if opts.query { "?" } else { "!" };
    format!("( {} [{}] : ({}) )", q, var_strs.join(", "), result)
}

/// Render the full KB as a TPTP string (axioms + assertions with hypothesis role).
///
/// `session` filters which assertions are tagged as `hypothesis`:
/// - `Some(key)` → only assertions from that session are hypotheses
/// - `None` → all assertions across all sessions are hypotheses
pub fn kb_to_tptp(
    kb:      &KnowledgeBase,
    kb_name: &str,
    opts:    &TptpOptions,
    session: Option<&str>,
) -> String {
    let safe_name = kb_name.replace(|c: char| !c.is_alphanumeric() && c != '_', "_");

    let header = format!(
        "% Articulate Software\n\
         % www.ontologyportal.org www.articulatesoftware.com\n\
         % This software released under the GNU Public License <http://www.gnu.org/copyleft/gpl.html>.\n\
         % Translation of KB {}\n",
        safe_name
    );

    let all_assertion_ids: HashSet<SentenceId> =
        kb.assertion_sentence_ids().into_iter().collect();

    let assertion_ids: HashSet<SentenceId> = match session {
        Some(s) => kb.assertion_sentence_ids_for_session(s).into_iter().collect(),
        None    => all_assertion_ids.clone(),
    };

    // When a session filter is active, skip assertions from all other sessions.
    let excluded_ids: HashSet<SentenceId> = if session.is_some() {
        all_assertion_ids.difference(&assertion_ids).copied().collect()
    } else {
        HashSet::new()
    };

    let roots: Vec<SentenceId> = kb.store.roots.clone();
    let mut lines: Vec<String> = vec![header, String::new()];
    let mut written: HashSet<String> = HashSet::new();
    let mut idx = 1usize;
    let mut assertion_header_written = false;

    for sid in roots {
        // Skip assertions that belong to a different session.
        if excluded_ids.contains(&sid) { continue; }

        // Skip excluded predicates
        let head_name = kb.store.sentences[sid]
            .head_symbol()
            .map(|id| kb.store.sym_name(id).to_owned());
        if let Some(ref name) = head_name {
            if opts.excluded.contains(name) { continue; }
        }

        let tptp = sentence_to_tptp(sid, kb, opts);
        if tptp.is_empty() || written.contains(&tptp) { continue; }
        written.insert(tptp.clone());

        let is_assertion = assertion_ids.contains(&sid);
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
    use crate::store::{KifStore, load_kif};

    fn kb_from(kif: &str) -> KnowledgeBase {
        let mut store = KifStore::default();
        load_kif(&mut store, kif, "test");
        KnowledgeBase::new(store)
    }

    fn opts() -> TptpOptions {
        TptpOptions { hide_numbers: true, ..TptpOptions::default() }
    }

    #[test]
    fn simple_predicate() {
        let kb = kb_from("(subclass Human Animal)");
        let sid = kb.store.roots[0];
        let tptp = sentence_to_tptp(sid, &kb, &opts());
        assert!(tptp.contains("s__holds("), "got: {}", tptp);
        assert!(tptp.contains("s__subclass__m"), "got: {}", tptp);
        assert!(tptp.contains("s__Human"), "got: {}", tptp);
        assert!(tptp.contains("s__Animal"), "got: {}", tptp);
    }

    #[test]
    fn free_variable_wrapper() {
        let kb = kb_from("(instance ?X Human)");
        let sid = kb.store.roots[0];
        let tptp = sentence_to_tptp(sid, &kb, &opts());
        assert!(tptp.contains("! [V__X]"), "got: {}", tptp);
    }

    #[test]
    fn query_mode_existential() {
        let kb = kb_from("(instance ?X Human)");
        let sid = kb.store.roots[0];
        let q_opts = TptpOptions { query: true, hide_numbers: true, ..TptpOptions::default() };
        let tptp = sentence_to_tptp(sid, &kb, &q_opts);
        assert!(tptp.contains("? [V__X]"), "got: {}", tptp);
    }

    #[test]
    fn empty_quantifier() {
        // Quantifier with empty var list should be simplified to just the body
        // NOTE: (exists () (subclass Human Animal)) is now valid because 
        // the head is 'exists' (an operator), and the second element is '()' (empty list)
        let kb = kb_from("(exists () (subclass Human Animal))");
        assert!(!kb.store.roots.is_empty(), "KB should have one root");
        let sid = kb.store.roots[0];
        let tptp = sentence_to_tptp(sid, &kb, &opts());
        assert!(!tptp.contains("? []"), "should not contain empty quantifier: {}", tptp);
        assert!(tptp.contains("s__holds("), "should contain body: {}", tptp);
    }

    #[test]
    fn implication() {
        let kb = kb_from("(=> (instance ?X Human) (instance ?X Animal))");
        let sid = kb.store.roots[0];
        let tptp = sentence_to_tptp(sid, &kb, &opts());
        assert!(tptp.contains("=>"), "got: {}", tptp);
    }

    #[test]
    fn mention_suffix_lowercase() {
        let kb = kb_from("(instance subclass BinaryRelation)");
        let sid = kb.store.roots[0];
        let tptp = sentence_to_tptp(sid, &kb, &opts());
        // subclass used as an arg → mention suffix
        assert!(tptp.contains("s__subclass__m"), "got: {}", tptp);
    }

    #[test]
    fn nested_predicate_as_term() {
        let kb = kb_from("(holdsDuring ?I (attribute ?X LegalPersonhood))");
        let sid = kb.store.roots[0];
        let tptp = sentence_to_tptp(sid, &kb, &opts());
        // holdsDuring is a predicate (at top level) -> holds
        assert!(tptp.contains("s__holds(s__holdsDuring__m,"), "got: {}", tptp);
        // attribute is a term (nested) -> direct application (function)
        assert!(tptp.contains("s__attribute(V__X,s__LegalPersonhood)"), "got: {}", tptp);
    }

    #[test]
    fn nested_logical_operator() {
        let kb = kb_from("(holdsDuring ?I (and (attribute ?X LegalPersonhood) (instance ?X Human)))");
        let sid = kb.store.roots[0];
        let tptp = sentence_to_tptp(sid, &kb, &opts());
        // outer holdsDuring wrapped in holds
        assert!(tptp.contains("s__holds(s__holdsDuring__m,"), "got: {}", tptp);
        // inner 'and' MUST be reified as a function application, not '&'
        assert!(tptp.contains("s__and("), "missing s__and in: {}", tptp);
        assert!(!tptp.contains("&"), "found & inside term in: {}", tptp);
    }

    #[test]
    fn bare_variable_as_formula() {
        let kb = kb_from("(=> (instance ?P Proposition) ?P)");
        let sid = kb.store.roots[0];
        let tptp = sentence_to_tptp(sid, &kb, &opts());
        // ?P at the end should be wrapped in s__holds
        assert!(tptp.contains("=> s__holds(V__P))"), "got: {}", tptp);
    }

    #[test]
    fn number_hidden_by_default() {
        let kb = kb_from("(lessThan ?X 42)");
        let sid = kb.store.roots[0];
        let tptp = sentence_to_tptp(sid, &kb, &opts());
        assert!(tptp.contains("n__42"), "got: {}", tptp);
    }

    #[test]
    fn kb_to_tptp_contains_axiom() {
        let kb = kb_from("(subclass Human Animal)");
        let tptp = kb_to_tptp(&kb, "test", &opts(), None);
        assert!(tptp.contains(",axiom,"), "got: {}", tptp);
    }
}
