use std::collections::{HashMap, HashSet};

use crate::kif_store::KifStore;
use crate::semantic::SemanticLayer;
use crate::types::{Element, Literal, SentenceId, SymbolId};
use super::names::translate_symbol;
use super::options::TptpOptions;

// ── TFF sort translation ──────────────────────────────────────────────────────

/// Map a SUMO type name to the most specific TFF primitive sort.
///
/// TFF has four primitive sorts: `$int`, `$rat`, `$real` (numeric), and `$i`
/// (generic individual).  SUMO types that are subclasses of `Integer`,
/// `RationalNumber`, or `RealNumber` map to the corresponding TFF sort;
/// everything else maps to `$i`.
///
/// The ordering Integer > RationalNumber > RealNumber ensures the most
/// specific sort is returned (e.g. `NonnegativeInteger` → `$int`, not `$rat`).
pub(crate) fn translate_sort(sumo_type: &str, layer: &SemanticLayer) -> &'static str {
    match sumo_type {
        "Integer"        => return "$int",
        "RationalNumber" => return "$rat",
        "RealNumber"     => return "$real",
        _                => {}
    }
    if let Some(id) = layer.store.sym_id(sumo_type) {
        if layer.has_ancestor_by_name(id, "Integer")        { return "$int"; }
        if layer.has_ancestor_by_name(id, "RationalNumber") { return "$rat"; }
        if layer.has_ancestor_by_name(id, "RealNumber")     { return "$real"; }
    }
    "$i"
}

// ── TFF type declarations ─────────────────────────────────────────────────────

/// Lazy accumulator for TFF type declarations.
///
/// Created empty at the start of `kb_to_tptp()` when `lang == Tff`.
/// Symbols are declared on first encounter via `ensure_declared()`.
/// After all sentences are translated, `decl_lines` contains the
/// complete preamble, ready to be prepended to the axiom output.
pub(crate) struct TffContext {
    /// Symbols already declared — O(1) dedup check, no string ops.
    pub(crate) declared:     HashSet<SymbolId>,
    /// Accumulated declaration lines in encounter order.
    pub(crate) decl_lines:   Vec<String>,
    /// relation/function SymbolId → arg sorts (filled by ensure_declared).
    pub(crate) signatures:   HashMap<SymbolId, Vec<&'static str>>,
    /// function SymbolId → TFF return sort (filled by ensure_declared).
    pub(crate) return_sorts: HashMap<SymbolId, &'static str>,
    /// Per-sentence variable sort map, set by infer_var_types before translation.
    pub(crate) var_types:    HashMap<SymbolId, &'static str>,
}

impl TffContext {
    pub(crate) fn new() -> Self {
        TffContext {
            declared:     HashSet::new(),
            decl_lines:   Vec::new(),
            signatures:   HashMap::new(),
            return_sorts: HashMap::new(),
            var_types:    HashMap::new(),
        }
    }

    /// Ensure an element has a type declaration if it is a symbol.
    ///
    /// Returns immediately for variables, literals, sub-sentences, and ops —
    /// only `Element::Symbol` produces a declaration. Idempotent per symbol.
    pub(crate) fn ensure_declared(
        &mut self,
        elem:  &Element,
        layer: &SemanticLayer,
        opts:  &TptpOptions,
    ) {
        let id = match elem {
            Element::Symbol(id) => *id,
            _ => return,
        };

        if self.declared.contains(&id) { return; }  // O(1), no alloc
        self.declared.insert(id);

        let name = layer.store.sym_name(id);
        if opts.excluded.contains(name) { return; }  // skip meta-predicates

        // Always populate signatures so infer_var_types can read domain info,
        // even for symbols that map to TFF builtins (no type declaration needed).
        if layer.is_function(id) {
            let arg_sorts = domain_to_sorts(layer, id);
            let ret_sort  = range_to_sort(layer, id);
            self.signatures.insert(id, arg_sorts);
            self.return_sorts.insert(id, ret_sort);
        } else if layer.is_relation(id) || layer.is_predicate(id) {
            if layer.arity(id) != Some(-1) {
                let arg_sorts = domain_to_sorts(layer, id);
                self.signatures.insert(id, arg_sorts);
            }
        }

        // TFF arithmetic/comparison builtins are handled by $sum/$less etc.;
        // no tff(..., type, ...) declaration is emitted for them.
        if tff_math_builtin(name).is_some() || tff_comparison_builtin(name).is_some() { return; }

        // String allocation happens here, once per unique symbol
        let tptp_name  = translate_symbol(name, true, Some(id), layer);
        let axiom_name = format!("type_{}", tptp_name.to_lowercase());

        let sig = if layer.is_function(id) {
            let arg_sorts = self.signatures.get(&id).cloned().unwrap_or_default();
            let ret_sort  = self.return_sorts.get(&id).copied().unwrap_or("$i");
            format!("{}: {}", tptp_name, format_function_sig(&arg_sorts, ret_sort))
        } else if layer.is_relation(id) || layer.is_predicate(id) {
            if layer.arity(id) == Some(-1) {
                format!("{}: $i", tptp_name)
            } else {
                let arg_sorts = self.signatures.get(&id).cloned().unwrap_or_default();
                format!("{}: {}", tptp_name, format_relation_sig(&arg_sorts))
            }
        } else {
            format!("{}: $i", tptp_name)
        };

        self.decl_lines.push(format!("tff({}, type, {}).", axiom_name, sig));
    }
}

fn domain_to_sorts(layer: &SemanticLayer, id: SymbolId) -> Vec<&'static str> {
    layer.domain(id).iter().map(|rd| {
        if rd.id() == u64::MAX { "$i" }
        else { translate_sort(layer.store.sym_name(rd.id()), layer) }
    }).collect()
}

fn range_to_sort(layer: &SemanticLayer, id: SymbolId) -> &'static str {
    match layer.range(id) {
        Ok(Some(rd)) => {
            if rd.id() == u64::MAX { "$i" }
            else { translate_sort(layer.store.sym_name(rd.id()), layer) }
        }
        _ => "$i",
    }
}

fn format_relation_sig(sorts: &[&str]) -> String {
    match sorts.len() {
        0 => "$o".to_string(),
        1 => format!("({}) > $o", sorts[0]),
        _ => format!("({}) > $o", sorts.join(" * ")),
    }
}

fn format_function_sig(sorts: &[&str], ret: &str) -> String {
    match sorts.len() {
        0 => ret.to_string(),
        1 => format!("({}) > {}", sorts[0], ret),
        _ => format!("({}) > {}", sorts.join(" * "), ret),
    }
}

// ── TFF arithmetic / comparison builtins ─────────────────────────────────────

/// Maps a SUMO function name to its TFF arithmetic builtin, if any.
pub(crate) fn tff_math_builtin(name: &str) -> Option<&'static str> {
    match name {
        "AdditionFn"       => Some("$sum"),
        "SubtractionFn"    => Some("$difference"),
        "MultiplicationFn" => Some("$product"),
        "DivisionFn"       => Some("$quotient_e"),
        "FloorFn"          => Some("$floor"),
        "CeilingFn"        => Some("$ceiling"),
        "RoundFn"          => Some("$round"),
        "AbsoluteValueFn"  => Some("$abs"),
        "RemainderFn"      => Some("$remainder_e"),
        "TruncateFn"       => Some("$truncate"),
        "SuccessorFn"      => Some("$sum"),
        "PredecessorFn"    => Some("$difference"),
        _                  => None,
    }
}

/// Maps a SUMO comparison predicate to its TFF builtin, if any.
pub(crate) fn tff_comparison_builtin(name: &str) -> Option<&'static str> {
    match name {
        "lessThan"             => Some("$less"),
        "greaterThan"          => Some("$greater"),
        "lessThanOrEqualTo"    => Some("$lesseq"),
        "greaterThanOrEqualTo" => Some("$greatereq"),
        _                      => None,
    }
}

// ── TFF variable type inference ───────────────────────────────────────────────

/// Recursively visit every sentence node in the tree rooted at `sid`.
///
/// `f` is called with each sentence's element slice. Traversal is depth-first,
/// parent before children.
fn walk_sentence<F>(sid: SentenceId, store: &KifStore, f: &mut F)
where
    F: FnMut(&[Element]),
{
    let elems = &store.sentences[store.sent_idx(sid)].elements;
    f(elems);
    for elem in elems {
        if let Element::Sub(sub_sid) = elem {
            walk_sentence(*sub_sid, store, f);
        }
    }
}

/// Returns a numeric specificity score for a TFF sort. Higher = more specific.
/// Used so the most precise sort wins when multiple passes produce candidates.
fn sort_specificity(sort: &str) -> u8 {
    match sort {
        "$int"  => 4,
        "$rat"  => 3,
        "$real" => 2,
        "$i"    => 1,
        _       => 0,
    }
}

/// Infer the TFF sort for each variable appearing in the sentence tree.
///
/// Pass 0  — call `ensure_declared` for all head symbols so `tff.signatures`
///           is populated before the inference passes read it.
/// Pass 1  — direct `(instance ?X TYPE)` patterns (strongest signal).
/// Pass 2  — argument position in predicates with known signatures.
/// Pass 3  — numeric literal co-occurrence (weak; does not override passes 1-2).
/// Default — `$i` for any variable not resolved by the above.
pub(crate) fn infer_var_types(
    sid:   SentenceId,
    store: &KifStore,
    layer: &SemanticLayer,
    tff:   &mut TffContext,
    opts:  &TptpOptions,
) -> HashMap<SymbolId, &'static str> {
    // Pass 0: populate tff.signatures for all head symbols in this sentence tree
    walk_sentence(sid, store, &mut |elems| {
        if let Some(head) = elems.first() {
            tff.ensure_declared(head, layer, opts);
        }
    });

    let mut types: HashMap<SymbolId, &'static str> = HashMap::new();

    // Pass 1: (instance ?X TYPE) → var gets translate_sort(TYPE)
    walk_sentence(sid, store, &mut |elems| {
        if elems.len() < 3 { return; }
        let is_instance = matches!(&elems[0],
            Element::Symbol(id) if store.sym_name(*id) == "instance");
        if !is_instance { return; }
        if let (Element::Variable { id: var_id, .. }, Element::Symbol(type_id)) =
            (&elems[1], &elems[2])
        {
            let sort = translate_sort(store.sym_name(*type_id), layer);
            let entry = types.entry(*var_id).or_insert("$i");
            if sort_specificity(sort) > sort_specificity(entry) {
                *entry = sort;
            }
        }
    });

    // Pass 2: variable at argument position N in a predicate with known signature
    walk_sentence(sid, store, &mut |elems| {
        let head_id = match elems.first() {
            Some(Element::Symbol(id)) => *id,
            _ => return,
        };
        let Some(sigs) = tff.signatures.get(&head_id) else { return; };
        let sigs = sigs.clone(); // avoid simultaneous borrow of tff + types
        for (i, elem) in elems[1..].iter().enumerate() {
            if let Element::Variable { id: var_id, .. } = elem {
                let sort = sigs.get(i).copied().unwrap_or("$i");
                let entry = types.entry(*var_id).or_insert("$i");
                if sort_specificity(sort) > sort_specificity(entry) {
                    *entry = sort;
                }
            }
        }
    });

    // Pass 3: numeric literal co-occurrence — only updates vars still at "$i"
    walk_sentence(sid, store, &mut |elems| {
        let lit_sort: Option<&'static str> = elems.iter().find_map(|e| match e {
            Element::Literal(Literal::Number(n)) => {
                if n.contains('.') { Some("$real") } else { Some("$int") }
            }
            _ => None,
        });
        let Some(lit_sort) = lit_sort else { return; };
        for elem in elems {
            if let Element::Variable { id: var_id, .. } = elem {
                let entry = types.entry(*var_id).or_insert("$i");
                if *entry == "$i" {
                    *entry = lit_sort;
                }
            }
        }
    });

    types
}
