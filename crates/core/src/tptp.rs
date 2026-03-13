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
    let result = name.replace('.', "_").replace('-', "_");
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
            let inner = &s[0..s.len()];
            inner
                .chars()
                .filter(|&c| c != '\'')
                .map(|c| if matches!(c, '\n' | '\t' | '\r' | '\x0C') { ' ' } else { c })
                .collect()
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

fn collect_bound_vars(sid: SentenceId, store: &KifStore, out: &mut HashSet<String>) {
    let sentence = &store.sentences[sid];
    if let Some(op) = sentence.op() {
        if matches!(op, OpKind::ForAll | OpKind::Exists) {
            // Variable list is elements[1] (a Sub sentence)
            if let Some(Element::Sub(var_list)) = sentence.elements.get(1) {
                for e in &store.sentences[*var_list].elements {
                    if let Element::Variable { name, .. } = e {
                        out.insert(name.clone());
                    }
                }
            }
            // Recurse into body (elements[2])
            if let Some(Element::Sub(body)) = sentence.elements.get(2) {
                collect_bound_vars(*body, store, out);
            }
            return;
        }
    }
    for elem in &sentence.elements {
        if let Element::Sub(sub) = elem {
            collect_bound_vars(*sub, store, out);
        }
    }
}

fn free_vars(sid: SentenceId, store: &KifStore) -> Vec<String> {
    let mut all   = HashSet::new();
    let mut bound = HashSet::new();
    collect_all_vars(sid, store, &mut all);
    collect_bound_vars(sid, store, &mut bound);
    let mut result: Vec<String> = all.into_iter().filter(|v| !bound.contains(v)).collect();
    result.sort(); // deterministic output
    result
}

// ── Recursive translation ─────────────────────────────────────────────────────

fn translate_element(
    elem:  &Element,
    store: &KifStore,
    opts:  &TptpOptions,
    kb:    &KnowledgeBase,
) -> String {
    match elem {
        Element::Symbol(id) => {
            translate_symbol(store.sym_name(*id), false, Some(*id), kb)
        }
        Element::Variable { name, .. } => translate_variable(name),
        Element::Literal(lit) => translate_literal(lit, opts),
        Element::Sub(sid) => translate_sentence(*sid, store, opts, kb),
        Element::Op(op) => op.name().to_owned(),
    }
}

fn translate_sentence(
    sid:   SentenceId,
    store: &KifStore,
    opts:  &TptpOptions,
    kb:    &KnowledgeBase,
) -> String {
    let sentence = &store.sentences[sid];

    if sentence.is_operator() {
        return translate_operator_sentence(sid, store, opts, kb);
    }

    match sentence.elements.first() {
        Some(Element::Symbol(head_id)) => {
            // Regular predicate / function application.
            let head_id = *head_id;
            let head_name = store.sym_name(head_id);
            let head_str = translate_symbol(head_name, true, Some(head_id), kb);
            let args: Vec<String> = sentence.elements[1..]
                .iter()
                .map(|e| translate_element(e, store, opts, kb))
                .collect();
            format!("{}({})", head_str, args.join(","))
        }
        Some(Element::Variable { name, .. }) => {
            // Variable-headed sentence: (?REL A B) → s__holds(V__REL, A, B).
            // This is the standard SUMO higher-order encoding for FOF.
            let var_str = translate_variable(name);
            let args: Vec<String> = std::iter::once(var_str)
                .chain(sentence.elements[1..].iter().map(|e| translate_element(e, store, opts, kb)))
                .collect();
            format!("{}holds({})", TPTP_SYMBOL_PREFIX, args.join(","))
        }
        _ => String::new(),
    }
}

fn translate_operator_sentence(
    sid:   SentenceId,
    store: &KifStore,
    opts:  &TptpOptions,
    kb:    &KnowledgeBase,
) -> String {
    let sentence = &store.sentences[sid];
    let op = match sentence.op() {
        Some(op) => op.clone(),
        None     => return String::new(),
    };
    // args = elements after the leading Op
    let args: Vec<&Element> = sentence.elements[1..].iter().collect();

    match op {
        OpKind::And | OpKind::Or => {
            let tptp_op = if op == OpKind::And { "&" } else { "|" };
            let parts: Vec<String> = args
                .iter()
                .map(|e| translate_element(e, store, opts, kb))
                .collect();
            format!("({})", parts.join(&format!(" {} ", tptp_op)))
        }

        OpKind::Not => {
            let inner = translate_element(args[0], store, opts, kb);
            format!("~({})", inner)
        }

        OpKind::Implies => {
            let a = translate_element(args[0], store, opts, kb);
            let b = translate_element(args[1], store, opts, kb);
            format!("({} => {})", a, b)
        }

        OpKind::Iff => {
            let a = translate_element(args[0], store, opts, kb);
            let b = translate_element(args[1], store, opts, kb);
            format!("(({} => {}) & ({} => {}))", a, b, b, a)
        }

        OpKind::Equal => {
            let a = translate_element(args[0], store, opts, kb);
            let b = translate_element(args[1], store, opts, kb);
            format!("({} = {})", a, b)
        }

        OpKind::ForAll | OpKind::Exists => {
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
            let body = translate_element(args[1], store, opts, kb);
            format!("({} [{}] : ({}))", q, vars.join(", "), body)
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Translate a single root sentence to a TPTP formula string.
///
/// Free variables are wrapped in a top-level universal quantifier
/// (`opts.query = true` → existential).
pub fn sentence_to_tptp(
    sid:  SentenceId,
    kb:   &KnowledgeBase,
    opts: &TptpOptions,
) -> String {
    let result = translate_sentence(sid, &kb.store, opts, kb);

    // Wrap free variables
    let free = free_vars(sid, &kb.store);
    if free.is_empty() {
        return result;
    }

    let var_strs: Vec<String> = free.iter().map(|v| translate_variable(v)).collect();
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
        assert!(tptp.contains("s__subclass("), "got: {}", tptp);
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
    fn number_hidden_by_default() {
        let kb = kb_from("(lessThan ?X 42)");
        let sid = kb.store.roots[0];
        let tptp = sentence_to_tptp(sid, &kb, &opts());
        assert!(tptp.contains("n__42"), "got: {}", tptp);
        assert!(!tptp.contains(",42)"), "got: {}", tptp);
    }

    #[test]
    fn kb_to_tptp_contains_axiom() {
        let kb = kb_from("(subclass Human Animal)");
        let tptp = kb_to_tptp(&kb, "test", &opts(), None);
        assert!(tptp.contains(",axiom,"), "got: {}", tptp);
    }

    #[test]
    fn kb_to_tptp_assertion_is_hypothesis() {
        const BASE: &str = "
            (subclass Relation Entity)
            (subclass BinaryRelation Relation)
            (instance subclass BinaryRelation)
            (domain subclass 1 Class)
            (domain subclass 2 Class)
            (subclass Animal Entity)
        ";
        let mut store = KifStore::default();
        load_kif(&mut store, BASE, "base");
        let mut kb = KnowledgeBase::new(store);
        kb.tell("s1", "(subclass Cat Animal)");
        let tptp = kb_to_tptp(&kb, "test", &opts(), None);
        assert!(tptp.contains(",hypothesis,"), "got: {}", tptp);
        assert!(tptp.contains("% Assertions (tell)"), "got: {}", tptp);
    }

    #[test]
    fn kb_to_tptp_session_filter() {
        const BASE: &str = "
            (subclass Relation Entity)
            (subclass BinaryRelation Relation)
            (instance subclass BinaryRelation)
            (domain subclass 1 Class)
            (domain subclass 2 Class)
            (subclass Animal Entity)
        ";
        let mut store = KifStore::default();
        load_kif(&mut store, BASE, "base");
        let mut kb = KnowledgeBase::new(store);
        kb.tell("s1", "(subclass Cat Animal)");
        kb.tell("s2", "(subclass Dog Animal)");
        // Only s1's assertion shows as hypothesis
        let tptp = kb_to_tptp(&kb, "test", &opts(), Some("s1"));
        assert!(tptp.contains("s__Cat"), "got: {}", tptp);
        // s2's assertion is present in the store but not marked hypothesis
        assert!(!tptp.contains("s__Dog"), "s2 assertion should be excluded: {}", tptp);
    }
}
