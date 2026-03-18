use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::kif_store::KifStore;
use crate::semantic::SemanticLayer;
use crate::parse::kif::OpKind;
use crate::types::{Element, SentenceId, SymbolId};
use super::names::{TPTP_SYMBOL_PREFIX, translate_symbol, translate_variable, translate_literal};
use super::options::{TptpLang, TptpOptions};
use super::tff::{TffContext, infer_var_types, tff_math_builtin, tff_comparison_builtin};

// ── Free-variable collection ──────────────────────────────────────────────────

// Collect all the variables which appear in the sentence that are NOT bound to an
//  existing existential (they bubble up to the top scope and need to be pulled out)
fn collect_free_vars(sid: SentenceId, store: &KifStore, out: &mut HashSet<u64>, bound: &mut HashSet<u64>) {
    let sent_idx = store.sent_idx(sid);
    // If the current sentence is an existential operation, the variables are bound,
    //  so add to a list of bound variables
    // We can assume that the sentence has a first element and it is not None
    if matches!(&store.sentences[sent_idx].elements.first(),
                Some(Element::Op(OpKind::ForAll)) | Some(Element::Op(OpKind::Exists))) {
        // The second element (index 1) is the var-list Sub; first element is the Op itself
        if let Some(Element::Sub(sub_sid)) = store.sentences[sent_idx].elements.get(1) {
            for bound_var in &store.sentences[store.sent_idx(*sub_sid)].elements {
                match bound_var {
                    Element::Variable { id, .. } => { bound.insert(*id); },
                    _ => {}
                }
            }
        };
    }
    // For each individual element in the sentence
    for elem in &store.sentences[store.sent_idx(sid)].elements {
        match elem {
            // if its a variable, collect it
            Element::Variable { id, .. } if !bound.contains(id) => { out.insert(*id); }
            // If its a sub-sentence, recurse into it
            Element::Sub(sub) => collect_free_vars(*sub, store, out, bound),
            _ => {}
        }
    }
}

// ── Translation context ───────────────────────────────────────────────────────

/// Context threaded through all recursive TPTP translation calls.
///
/// `tff` is `None` for FOF output. For TFF output it holds a shared
/// `RefCell<TffContext>` so that `ensure_declared` can be called (mutably)
/// from any point in the recursive descent without changing every signature.
struct TransCtx<'a> {
    store: &'a KifStore,
    opts:  &'a TptpOptions,
    layer: &'a SemanticLayer,
    tff:   Option<&'a RefCell<TffContext>>,
}

// ── Recursive translation ─────────────────────────────────────────────────────

fn translate_element(
    elem:       &Element,
    tc:         &TransCtx,
    as_formula: bool,
) -> String {
    match elem {
        Element::Symbol(id) => {
            // TFF: lazily declare every symbol we encounter.
            if let Some(cell) = &tc.tff {
                cell.borrow_mut().ensure_declared(elem, tc.layer, tc.opts);
            }
            let sym_str = translate_symbol(tc.store.sym_name(*id), false, Some(*id), tc.layer);
            // TFF has no holds-encoding; bare symbols are atomic propositions or terms.
            if as_formula && tc.opts.lang != TptpLang::Tff {
                format!("{}holds({})", TPTP_SYMBOL_PREFIX, sym_str)
            } else {
                sym_str
            }
        }
        Element::Variable { name, .. } => {
            let var_str = translate_variable(name);
            if as_formula && tc.opts.lang != TptpLang::Tff {
                format!("{}holds({})", TPTP_SYMBOL_PREFIX, var_str)
            } else {
                var_str
            }
        }
        Element::Literal(lit) => translate_literal(lit, tc.opts),
        Element::Sub(sid)     => translate_sentence(*sid, tc, as_formula),
        Element::Op(op) => {
            if as_formula {
                op.name().to_owned()
            } else {
                translate_symbol(op.name(), false, None, tc.layer)
            }
        }
    }
}

/// Translate a sentence to TPTP lang
fn translate_sentence(
    sid:        SentenceId,
    tc:         &TransCtx,
    as_formula: bool,
) -> String {
    let sentence = &tc.store.sentences[tc.store.sent_idx(sid)];
    if sentence.is_operator() {
        return translate_operator_sentence(sid, tc, as_formula);
    }

    match sentence.elements.first() {
        // Get the first element in the sentence
        Some(Element::Symbol(head_id)) => {
            // If a symbol, get the head and the args. Args are converted one-by-one
            let head_id   = *head_id;
            let head_name = tc.store.sym_name(head_id);
            let args: Vec<String> = sentence.elements[1..]
                .iter()
                .map(|e| translate_element(e, tc, false))
                .collect();

            // TFF: declare head, dispatch to builtins, emit direct predicate call.
            if tc.opts.lang == TptpLang::Tff {
                // Declare the head symbol (idempotent; skips builtins internally).
                if let Some(cell) = &tc.tff {
                    cell.borrow_mut().ensure_declared(&sentence.elements[0], tc.layer, tc.opts);
                }
                // Arithmetic functions — valid as terms in both as_formula and !as_formula.
                if let Some(builtin) = tff_math_builtin(head_name) {
                    return match head_name {
                        "SuccessorFn"   => format!("{}({},1)", builtin, args[0]),
                        "PredecessorFn" => format!("{}({},1)", builtin, args[0]),
                        _               => format!("{}({})", builtin, args.join(",")),
                    };
                }
                if as_formula {
                    // Comparison predicates → TFF builtins.
                    if let Some(builtin) = tff_comparison_builtin(head_name) {
                        return format!("{}({})", builtin, args.join(","));
                    }
                    // Direct predicate call — no holds encoding.
                    let head_str = translate_symbol(head_name, true, Some(head_id), tc.layer);
                    return format!("{}({})", head_str, args.join(","));
                }
                // !as_formula in TFF: normal term form.
                let head_str = translate_symbol(head_name, true, Some(head_id), tc.layer);
                return format!("{}({})", head_str, args.join(","));
            }

            if as_formula {
                let head_mention = translate_symbol(head_name, false, Some(head_id), tc.layer);
                let mut holds_args = vec![head_mention];
                holds_args.extend(args);
                format!("{}holds({})", TPTP_SYMBOL_PREFIX, holds_args.join(","))
            } else {
                // Create the sentence as a prolog function
                let head_str = translate_symbol(head_name, true, Some(head_id), tc.layer);
                format!("{}({})", head_str, args.join(","))
            }
        }
        Some(Element::Variable { id, .. }) => {
            // The relation is a variable, convert the variable
            let var_str = translate_variable(tc.store.sym_name(*id));
            let args: Vec<String> = std::iter::once(var_str)
                .chain(sentence.elements[1..].iter()
                    .map(|e| translate_element(e, tc, false)))
                .collect();
            if as_formula {
                format!("{}holds({})", TPTP_SYMBOL_PREFIX, args.join(","))
            } else {
                // Create a special method for that variable
                format!("{}holds_app({})", TPTP_SYMBOL_PREFIX, args.join(","))
            }
        }
        _ => String::new(),
    }
}

/// Translate a sentence which begins with an operator to desired TPTP lang
fn translate_operator_sentence(
    sid:        SentenceId,
    tc:         &TransCtx,
    as_formula: bool,
) -> String {
    let sentence = &tc.store.sentences[tc.store.sent_idx(sid)];
    // Get operator and its args
    let op = match sentence.op() {
        Some(op) => op.clone(),
        None     => unreachable!("An operator sentence MUST start with a known operator")
    };
    let args: Vec<&Element> = sentence.elements[1..].iter().collect();

    // If its not supposed to be translated as formula
    if !as_formula {
        let head_str = translate_symbol(op.name(), true, None, tc.layer);
        let arg_strs: Vec<String> = args.iter()
            .map(|e| translate_element(e, tc, false))
            .collect();
        return format!("{}({})", head_str, arg_strs.join(","));
    }

    // Match the operator type
    match op {
        // Conjunctive / Disjunctive
        OpKind::And | OpKind::Or => {
            let tptp_op = if op == OpKind::And { "&" } else { "|" };
            let parts: Vec<String> = args.iter()
                .map(|e| translate_element(e, tc, true))
                .collect(); // Translate the individual arguments
            format!("({})", parts.join(&format!(" {} ", tptp_op))) // join them with the appropriate operator
        }
        OpKind::Not => {
            let inner = translate_element(args[0], tc, true);
            format!("~({})", inner) // Wrap in a negation
        }
        OpKind::Implies => {
            let a = translate_element(args[0], tc, true);
            let b = translate_element(args[1], tc, true);
            format!("({} => {})", a, b) // Wrap with an implication
        }
        OpKind::Iff => {
            let a = translate_element(args[0], tc, true);
            let b = translate_element(args[1], tc, true);
            format!("(({} => {}) & ({} => {}))", a, b, b, a) // Biconditional work around
        }
        OpKind::Equal => {
            let a = translate_element(args[0], tc, false);
            let b = translate_element(args[1], tc, false);
            format!("({} = {})", a, b)
        }
        OpKind::ForAll | OpKind::Exists => {
            if !as_formula {
                let head_str = translate_symbol(op.name(), true, None, tc.layer);
                let vars_str = translate_element(args[0], tc, false);
                let body_str = translate_element(args[1], tc, false);
                return format!("{}({},{})", head_str, vars_str, body_str);
            }
            let q = if op == OpKind::ForAll { "!" } else { "?" };
            // Translate body first — may call ensure_declared (borrow_mut on tc.tff).
            // The var-list is read afterwards so the borrow() on tc.tff is safe.
            let body = translate_element(args[1], tc, true);
            let vars: Vec<String> = match args[0] {
                Element::Sub(var_sid) => {
                    let var_elems = &tc.store.sentences[tc.store.sent_idx(*var_sid)].elements;
                    if tc.opts.lang == TptpLang::Tff {
                        // Typed variable list: `V__X: $int`
                        let var_types_snapshot: Vec<(SymbolId, &'static str)> = if let Some(cell) = &tc.tff {
                            let tff = cell.borrow();
                            var_elems.iter().filter_map(|e| {
                                if let Element::Variable { id, .. } = e {
                                    Some((*id, tff.var_types.get(id).copied().unwrap_or("$i")))
                                } else { None }
                            }).collect()
                        } else {
                            var_elems.iter().filter_map(|e| {
                                if let Element::Variable { id, .. } = e { Some((*id, "$i")) }
                                else { None }
                            }).collect()
                        };
                        var_types_snapshot.into_iter()
                            .map(|(id, sort)| format!("{}: {}", translate_variable(tc.store.sym_name(id)), sort))
                            .collect()
                    } else {
                        var_elems.iter()
                            .filter_map(|e| {
                                if let Element::Variable { id, .. } = e {
                                    Some(translate_variable(tc.store.sym_name(*id)))
                                } else { None }
                            })
                            .collect()
                    }
                }
                _ => unreachable!("forall/exists sentences should have already been validated for the first sentence being comprised only of operators"),
            };
            if vars.is_empty() { body }
            else { format!("({} [{}] : ({}))", q, vars.join(", "), body) }
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Translate and quantify a single sentence given a fully-constructed `TransCtx`.
///
/// Shared by `sentence_to_tptp` and `kb_to_tptp` so that TFF mode can inject a
/// shared `TffContext` without duplicating the free-variable wrapping logic.
fn sentence_formula(sid: SentenceId, tc: &TransCtx) -> String {
    let result = translate_sentence(sid, tc, true);

    let mut free_vars: HashSet<u64> = HashSet::new();
    let mut bound_vars: HashSet<u64> = HashSet::new();
    collect_free_vars(sid, tc.store, &mut free_vars, &mut bound_vars);
    if free_vars.is_empty() { return result; }

    let q = if tc.opts.query { "?" } else { "!" };

    // TFF: annotate free variables with their inferred sorts.
    // The borrow() here is safe — translate_sentence (all borrow_muts) is done.
    let mut var_strs: Vec<String> = if tc.opts.lang == TptpLang::Tff {
        // Snapshot var_types out of the RefCell so no borrow persists during iteration.
        let snapshot: HashMap<u64, &'static str> = tc.tff.as_ref()
            .map(|cell| cell.borrow().var_types.clone())
            .unwrap_or_default();
        free_vars.iter()
            .map(|v| {
                let sort = snapshot.get(v).copied().unwrap_or("$i");
                format!("{}: {}", translate_variable(&tc.store.sym_name(*v)), sort)
            })
            .collect()
    } else {
        free_vars.iter()
            .map(|v| translate_variable(&tc.store.sym_name(*v)))
            .collect()
    };
    var_strs.sort();
    format!("( {} [{}] : ({}) )", q, var_strs.join(", "), result)
}

/// Translate a single root sentence to a TPTP formula string.
///
/// All free variables are wrapped in a top-level universal quantifier
/// (`opts.query = true` → existential). In TFF mode the variables are
/// annotated with their inferred sorts.
pub(crate) fn sentence_to_tptp(
    sid:   SentenceId,
    layer: &SemanticLayer,
    opts:  &TptpOptions,
) -> String {
    let tff_cell = if opts.lang == TptpLang::Tff {
        let mut ctx = TffContext::new();
        let vt = infer_var_types(sid, &layer.store, layer, &mut ctx, opts);
        ctx.var_types = vt;
        Some(RefCell::new(ctx))
    } else {
        None
    };
    let tc = TransCtx { store: &layer.store, opts, layer, tff: tff_cell.as_ref() };
    sentence_formula(sid, &tc)
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
    // Get a safe printable name of the KB
    let safe_name = kb_name.replace(|c: char| !c.is_alphanumeric() && c != '_', "_");
    // generic SUMO TPTP header
    let header = format!(
        "% Articulate Software\n\
         % www.ontologyportal.org www.articulatesoftware.com\n\
         % This software released under the GNU Public License <http://www.gnu.org/copyleft/gpl.html>.\n\
         % Translation of KB {}\n",
        safe_name
    );

    // Do some cloning. This is okay cause its just a vector of numericals
    let roots: Vec<SentenceId> = layer.store.roots.clone();

    // TFF mode: one shared TffContext accumulates type declarations across all sentences.
    let tff_cell: Option<RefCell<TffContext>> = if opts.lang == TptpLang::Tff {
        Some(RefCell::new(TffContext::new()))
    } else {
        None
    };

    // The axiom lines are collected separately so that TFF type declarations can be
    // prepended between the header and the axioms after the translation pass.
    let mut axiom_lines: Vec<String> = Vec::new();
    // Keep track of the lines already written
    let mut written: HashSet<String> = HashSet::new();
    let mut idx = 1usize;
    let mut assertion_header_written = false;

    // Iterate through the root axioms of the KB
    for sid in roots {
        // An assertion is treated as a hypothesis. They are tracked in root just like axioms
        //  but are also held in the assertion ID hash set so this is checking if the given
        //  sentence needs to be treated as a hypothesis
        let is_assertion = assertion_ids.contains(&sid);
        // Technically, if its not an assertion, its an axiom, but for future proofing (maybe
        //  eventually we are going to implement SinE) we can pass a subset of IDs
        let is_axiom     = axiom_ids.contains(&sid);
        // Skip if neither an axiom or assertion
        if !is_assertion && !is_axiom { continue; }

        // Skip excluded predicates
        let head_name = layer.store.sentences[layer.store.sent_idx(sid)]
            .head_symbol()
            .map(|id| layer.store.sym_name(id).to_owned());
        if let Some(ref name) = head_name {
            if opts.excluded.contains(name) { continue; }
        }

        // CONVERT!
        // TFF: use shared TffContext so declarations accumulate across all sentences.
        // FOF: delegate to sentence_to_tptp as before.
        let tptp = if let Some(cell) = &tff_cell {
            let vt = {
                let mut tff = cell.borrow_mut();
                infer_var_types(sid, &layer.store, layer, &mut *tff, opts)
            };
            cell.borrow_mut().var_types = vt;
            let tc = TransCtx { store: &layer.store, opts, layer, tff: Some(cell) };
            sentence_formula(sid, &tc)
        } else {
            sentence_to_tptp(sid, layer, opts)
        };

        // If a translation produces a duplicate, skip
        if tptp.is_empty() || written.contains(&tptp) {
            log::warn!("TPTP Duplicate found during translation, consider checking the axiom: sentence #{}", sid);
            continue;
        }
        written.insert(tptp.clone());

        // Comment to head off the assertion section
        // TODO: sort the sentences such that the assertions will appear together
        if is_assertion && !assertion_header_written {
            axiom_lines.push(String::new());
            axiom_lines.push("% Assertions (tell)".to_owned());
            assertion_header_written = true;
        }

        // Write the full TPTP line to include:
        //  lang (taken from translation op)
        //  identifier (derived from the KB name and the current line number)
        //  the role (axiom or assertion aka hypothesis)
        //  the formula from previous conversion
        let role = if is_assertion { "hypothesis" } else { "axiom" };
        axiom_lines.push(format!(
            "{}(kb_{}_{},{},({})). ",
            opts.lang.as_str(), safe_name, idx, role, tptp
        ));
        idx += 1;
    }

    // TFF: prepend accumulated type declarations between the header and the axioms.
    let mut lines: Vec<String> = vec![header, String::new()];
    if let Some(cell) = tff_cell {
        let decls = std::mem::take(&mut cell.borrow_mut().decl_lines);
        if !decls.is_empty() {
            lines.push("% Type declarations".to_owned());
            lines.extend(decls);
            lines.push(String::new());
        }
    }
    lines.extend(axiom_lines);
    lines.join("\n")
}
