// -- translate.rs --------------------------------------------------------------
//
// Core SUMO/KIF -> TPTP translation.
//
// This module converts a loaded KifStore + SemanticLayer into a stream of
// TPTP axiom/hypothesis strings.  It supports two dialects:
//
//   FOF (First-Order Form)
//     * All terms are untyped ($i-like world).
//     * Predicate applications in formula position are encoded as
//       `s__holds(s__Pred__m, arg1, arg2, ...)` -- the "holds" reification trick
//       that lets higher-order SUMO axioms survive in a flat FOF theory.
//     * Arithmetic literals are optionally encoded as `n__N` symbols when
//       `hide_numbers` is set (numbers have no native meaning in FOF).
//
//   TFF (Typed First-Order Form)
//     * Every variable carries an explicit sort annotation: `$i`, `$int`,
//       `$rat`, or `$real`.
//     * Predicate symbols are declared as `(T1 * T2 * ...) > $o` and function
//       symbols as `(T1 * ...) > T_return` in a preamble of `tff(..., type, ...)`
//       lines that Vampire requires before their first use.
//     * Arithmetic built-ins (`$sum`, `$less`, `$abs`, ...) replace the SUMO
//       math predicates/functions when argument sorts are compatible.
//     * Sort promotion (`$to_real`, `$to_rat`) is inserted when arguments of
//       mixed numeric sorts appear in a single arithmetic expression.
//
// Entry points
// -------------
//   `sentence_to_tptp`  -- translate a single SentenceId to a formula string.
//   `kb_to_tptp`        -- render all KB axioms/assertions as a full TPTP file.
//
// Internal call graph
// -------------------
//   kb_to_tptp / sentence_to_tptp
//     +-- sentence_formula          (wraps free variables in ! / ? quantifier)
//          +-- translate_sentence   (dispatches on head element)
//               +-- translate_operator_sentence  (=>, and, forall, ...)
//               +-- translate_element            (leaf element -> TPTP string)

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::kif_store::KifStore;
use crate::semantic::{SemanticLayer, Sort};
use crate::parse::ast::OpKind;
use crate::types::{Element, Literal, SentenceId, SymbolId};
use super::names::{TPTP_SYMBOL_PREFIX, translate_symbol, translate_variable, translate_literal};
use super::options::{TptpLang, TptpOptions};
use super::tff::{TffContext, infer_var_types, tff_math_builtin, tff_comparison_builtin, is_numeric_sort};

// -- Free-variable collection --------------------------------------------------

// Collect all the variables which appear in the sentence that are NOT bound to an
//  existing existential (they bubble up to the top scope and need to be pulled out)
//
// `in_formula` tracks whether the current sentence is being translated as a TPTP
// formula (true) or as a TPTP term (false).  In term context, forall/exists are
// encoded as plain function terms (s__exists/s__forall), so their declared
// variables are NOT actually bound in the TPTP output -- they must be treated as
// free and collected for the top-level universal quantifier.
fn collect_free_vars(sid: SentenceId, store: &KifStore, out: &mut HashSet<u64>, bound: &mut HashSet<u64>, in_formula: bool) {
    let sent_idx = store.sent_idx(sid);
    let sentence  = &store.sentences[sent_idx];

    // Only bind quantifier variables when this sentence is in formula context.
    // In term context the quantifier becomes s__exists(...) and its variables are
    // free TPTP variables that need to be captured at the top level.
    let is_quantifier = matches!(sentence.elements.first(),
        Some(Element::Op(OpKind::ForAll)) | Some(Element::Op(OpKind::Exists)));

    if in_formula && is_quantifier {
        if let Some(Element::Sub(sub_sid)) = sentence.elements.get(1) {
            for bound_var in &store.sentences[store.sent_idx(*sub_sid)].elements {
                if let Element::Variable { id, .. } = bound_var { bound.insert(*id); }
            }
        }
    }

    // Sub-sentences of operator sentences (=>, and, forall, ...) remain in formula
    // context (if we are already in formula context).  Sub-sentences of predicate
    // sentences (hasPurpose, instance, ...) are in term context.
    let children_in_formula = in_formula && sentence.is_operator();

    for elem in &store.sentences[store.sent_idx(sid)].elements {
        match elem {
            Element::Variable { id, .. } if !bound.contains(id) => { out.insert(*id); }
            Element::Sub(sub) => collect_free_vars(*sub, store, out, bound, children_in_formula),
            _ => {}
        }
    }
}

// -- Translation context -------------------------------------------------------

/// Context threaded through all recursive TPTP translation calls.
///
/// `tff` is `None` for FOF output. For TFF output it holds a shared
/// `RefCell<TffContext>` so that `ensure_declared` can be called (mutably)
/// from any point in the recursive descent without changing every signature.
struct TransCtx<'a> {
    store:     &'a KifStore,
    opts:      &'a TptpOptions,
    layer:     &'a SemanticLayer,
    tff:       Option<&'a RefCell<TffContext>>,
    /// Per-sentence variable sort map.  Produced by `infer_var_types` and held
    /// here rather than in `TffContext` because it is sentence-scoped, not KB-scoped.
    /// Keys are scope-suffixed variable SymbolIds (e.g. the id for `"X__3"`).
    var_types: HashMap<SymbolId, &'static str>,
}

// -- Recursive translation -----------------------------------------------------

/// Translate a single [`Element`] to its TPTP string representation.
///
/// `as_formula` distinguishes two positions:
///   - `true`  -- formula position: the element contributes a truth value.
///               A bare symbol or variable becomes `s__holds(sym)` in FOF
///               (reification); operators retain their TPTP connective spelling.
///   - `false` -- term position: the element contributes an individual term.
///               Symbols and variables translate to their TPTP name directly.
///
/// Element variants:
///   `Symbol`   -- looked up in the store, then translated via `translate_symbol`.
///                In FOF formula-position, wrapped in `s__holds(...)`.
///                In TFF, `ensure_declared` is called (accumulates type decls).
///   `Variable` -- translated via `translate_variable`.
///                In FOF formula-position, wrapped in `s__holds(...)`.
///   `Literal`  -- forwarded to `translate_literal` (string quoting / number encoding).
///   `Sub`      -- recursive call to `translate_sentence`.
///   `Op`       -- in formula position, the raw connective name (e.g. `"and"`);
///                in term position, prefixed as a SUMO constant (s__and).
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
        Element::Variable { id, .. } => {
            let var_str = translate_variable(tc.layer.store.sym_name(*id));
            if as_formula {
                if tc.opts.lang == TptpLang::Tff {
                    // In TFF, a $i-sorted variable cannot be used directly as a formula
                    // (sort $i is not $o).  Lift to $o via s__holds__1(?V) -- the
                    // same reification used in FOF, but arity-suffixed for TFF.
                    // Numeric-sorted variables ($int/$real/$rat) in formula position
                    // are also invalid but extremely rare; leave them as-is and rely on
                    // the Vampire error to surface them individually.
                    let sort = tc.var_types.get(id).copied().unwrap_or("$i");
                    if sort == "$i" {
                        format!("{}holds__1({})", TPTP_SYMBOL_PREFIX, var_str)
                    } else {
                        var_str
                    }
                } else {
                    format!("{}holds({})", TPTP_SYMBOL_PREFIX, var_str)
                }
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

/// Translate `elem` as a $i-sorted term.
///
/// Numeric literals are encoded as `n__N` symbol constants rather than TFF
/// integer/real literals, since `$int`/`$real` are not compatible with `$i`.
/// Used as a fallback when arithmetic builtins can't be applied because one
/// of the argument variables is `$i`-sorted.
fn encode_as_i_term(elem: &Element, tc: &TransCtx) -> String {
    if let Element::Literal(Literal::Number(n)) = elem {
        format!("n__{}", n.replace('.', "_").replace('-', "_"))
    } else {
        translate_element(elem, tc, false)
    }
}

// -- TFF numeric sort helpers --------------------------------------------------

/// Return the TFF sort of `elem` without translating it.
///
/// Used by the arithmetic/comparison dispatch to compute per-argument sorts
/// before deciding whether sort promotion (`$to_real`, `$to_rat`) is needed.
///
/// For sub-expressions whose head is a TFF arithmetic builtin (`$sum`,
/// `$difference`, ...) the return sort is **polymorphic** -- it equals the
/// most general sort of the arguments, not the statically-declared SUMO
/// range type.  For example, `(SubtractionFn ?E ?S)` where both `?E` and
/// `?S` are `$int` produces `$int`, even though SUMO declares the range of
/// `SubtractionFn` as `RealNumber` (-> `$real`).  Using the static range
/// here would incorrectly promote integer literals in the enclosing
/// comparison, creating a mixed-sort error.
fn elem_tff_sort(elem: &Element, tc: &TransCtx) -> &'static str {
    match elem {
        Element::Variable { id, .. } => tc.var_types.get(id).copied().unwrap_or("$i"),
        Element::Literal(Literal::Number(n)) => {
            if n.contains('.') { "$real" } else { "$int" }
        }
        Element::Symbol(id) => {
            // Individual constant (non-function, non-relation symbol).
            // May have a numeric sort from instance axioms (e.g. Pi: $real).
            tc.layer.sort_annotations()
                .as_ref()
                .and_then(|sa| sa.symbol_individual_sorts.get(id).copied())
                .map(|s: Sort| s.tptp())
                .unwrap_or("$i")
        }
        Element::Sub(sid) => {
            let sentence = &tc.store.sentences[tc.store.sent_idx(*sid)];
            match sentence.elements.first() {
                Some(Element::Symbol(fn_id)) => {
                    let fn_name = tc.store.sym_name(*fn_id);
                    // TFF arithmetic builtins are polymorphic: their return sort
                    // equals the most general sort of their arguments.  Recurse
                    // into the argument list rather than using the static SUMO
                    // range declaration.
                    if tff_math_builtin(fn_name).is_some() {
                        let arg_sorts: Vec<&str> = sentence.elements[1..]
                            .iter()
                            .map(|e| elem_tff_sort(e, tc))
                            .collect();
                        return numeric_best_sort(&arg_sorts);
                    }
                    // SUMO function with a declared range: use the static sort.
                    tc.layer.sort_annotations()
                        .as_ref()
                        .and_then(|sa| sa.symbol_return_sorts.get(fn_id).copied())
                        .map(|s: Sort| s.tptp())
                        .unwrap_or("$i")
                }
                _ => "$i",
            }
        }
        _ => "$i",
    }
}

/// Pick the most general numeric sort from a slice of TFF sorts.
///
/// Ordering (most general first): `$real > $rat > $int`.
/// Returns `"$i"` when no numeric sort is present (consistent with the
/// Java `bestOfPair` which returns a non-numeric type unchanged).
fn numeric_best_sort(sorts: &[&str]) -> &'static str {
    let mut best = "$i";
    for &s in sorts {
        best = match (best, s) {
            (_, "$real")              => "$real",
            ("$i" | "$int", "$rat")  => "$rat",
            ("$i",          "$int")  => "$int",
            _                        => best,
        };
    }
    best
}

/// Wrap `expr` with a TFF sort-promotion function when `from` is narrower than `to`.
///
/// Mirrors the Java `numTypePromotion` which wraps sub-expressions with
/// `$to_real(...)` or `$to_rat(...)` to satisfy single-sort requirements of
/// `$less`, `$sum`, etc.
fn promote_to_sort(expr: String, from: &str, to: &str) -> String {
    match (from, to) {
        ("$int", "$real") => format!("$to_real({})", expr),
        ("$int", "$rat")  => format!("$to_rat({})",  expr),
        ("$rat", "$real") => format!("$to_real({})", expr),
        _                 => expr,
    }
}

/// When a SUMO function with Entity range (-> `$i`) appears in an equality with
/// a numeric-sorted term, TFF rejects the equality as a sort mismatch.
///
/// This mirrors Java's `makePredFromArgTypes` type-specialization: we create a
/// sort-suffixed variant of the function (e.g. `s__ListOrderFn__Re` for a
/// `$real`-returning version) and emit a TFF declaration for it.  The
/// specialized variant is a different function symbol in TFF but captures the
/// semantic intent (the Java does the same -- the two symbols are unrelated in
/// Vampire's proof search, but the axiom is at least type-correct).
///
/// Returns `None` if `elem` is not a Sub (function application) or if the
/// function's declared return sort is already numeric (no specialization needed).
fn specialize_entity_fn_for_numeric(
    elem:        &Element,
    target_sort: &'static str,
    tc:          &TransCtx,
) -> Option<String> {
    let sid = match elem { Element::Sub(s) => *s, _ => return None };
    let sentence = &tc.store.sentences[tc.store.sent_idx(sid)];
    let head_sym_id = match sentence.elements.first() {
        Some(Element::Symbol(id)) => *id,
        _ => return None,
    };

    // Only specialize functions whose declared return sort is $i (Entity).
    let declared_return = tc.layer.sort_annotations()
        .as_ref()
        .and_then(|sa| sa.symbol_return_sorts.get(&head_sym_id).copied())
        .unwrap_or(Sort::Individual);
    if declared_return != Sort::Individual { return None; }

    // Also skip TFF arithmetic builtins -- they're already polymorphic.
    let head_name = tc.store.sym_name(head_sym_id);
    if tff_math_builtin(head_name).is_some() { return None; }

    let sort_suffix = match target_sort {
        "$real" => "Re",
        "$rat"  => "Ra",
        "$int"  => "In",
        _       => return None,
    };

    // For variable-arity functions `ensure_declared` emits `s__F__N` variants
    // per arity (e.g. `s__GCDFn__2`, `s__GCDFn__3`).  The specialized variant
    // must match the same arity-versioned base name so Vampire sees a type
    // declaration that covers the actual call-site arity.
    let arg_count    = sentence.elements.len() - 1;
    let is_variadic  = tc.layer.arity(head_sym_id) == Some(-1);
    let tptp_base    = translate_symbol(head_name, true, Some(head_sym_id), tc.layer);
    let versioned    = if is_variadic { format!("{}__{}", tptp_base, arg_count) } else { tptp_base };
    let spec_name    = format!("{}__{}", versioned, sort_suffix);
    let axiom_label  = format!("type_{}", spec_name.to_lowercase());

    // Emit the TFF type declaration for the specialized variant (idempotent).
    if let Some(cell) = &tc.tff {
        let is_new = cell.borrow_mut().specialized_decls.insert(spec_name.clone());
        if is_new {
            // Extend declared arg sorts with rest-type carry-over to cover the
            // actual call arity, mirroring `ensure_declared`'s logic.
            let base_sorts = tc.layer.sort_annotations()
                .as_ref()
                .and_then(|sa| sa.symbol_arg_sorts.get(&head_sym_id).cloned())
                .unwrap_or_default();
            // Compute the actual TFF sort of each call-site argument so that
            // $i-declared positions occupied by numeric args get the correct
            // numeric sort in the declaration.  For example, `MaxFn` has all-$i
            // declared args, but when called as `(MaxFn ?X ?Y)` where ?X:$real,
            // ?Y:$real the declaration must be `($real * $real) > $real`.
            let actual_arg_sorts: Vec<&str> = sentence.elements[1..]
                .iter()
                .map(|e| elem_tff_sort(e, tc))
                .collect();
            let arg_sorts: Vec<&str> = if is_variadic && arg_count > 0 {
                let rest = base_sorts.last().copied().unwrap_or(Sort::Individual);
                (0..arg_count)
                    .map(|i| {
                        let decl = base_sorts.get(i).copied().unwrap_or(rest).tptp();
                        let actual = actual_arg_sorts.get(i).copied().unwrap_or("$i");
                        if decl == "$i" && is_numeric_sort(actual) { actual } else { decl }
                    })
                    .collect()
            } else {
                base_sorts.iter().enumerate().map(|(i, s): (usize, &Sort)| {
                    let decl = s.tptp();
                    let actual = actual_arg_sorts.get(i).copied().unwrap_or("$i");
                    if decl == "$i" && is_numeric_sort(actual) { actual } else { decl }
                }).collect()
            };
            let sig_str = if arg_sorts.is_empty() {
                target_sort.to_string()
            } else {
                format!("({}) > {}", arg_sorts.join(" * "), target_sort)
            };
            cell.borrow_mut().decl_lines.push(
                format!("tff({}, type, {}: {}).", axiom_label, spec_name, sig_str)
            );
        }
    }

    // Translate the arguments normally then assemble the specialized call.
    let fn_args: Vec<String> = sentence.elements[1..]
        .iter()
        .map(|e| translate_element(e, tc, false))
        .collect();
    Some(format!("{}({})", spec_name, fn_args.join(",")))
}

/// Select the polymorphic variant name for a relation/function call, if needed.
///
/// If the symbol has at least one numeric-ancestor domain position
/// (`has_poly_variant_args`) and the best TFF sort across the actual call
/// arguments is numeric, appends `__int`, `__rat`, or `__real` to `base_name`.
/// Otherwise returns `base_name` unchanged.
///
/// `base_name` is the already arity-versioned name (e.g. `s__ListFn__1` for a
/// variable-arity function or `s__SomePred` for a fixed-arity one).
fn poly_variant_name(
    base_name: String,
    sym_id:    SymbolId,
    arg_elems: &[Element],
    tc:        &TransCtx,
) -> String {
    if !tc.layer.has_poly_variant_args(sym_id) { return base_name; }
    let arg_sorts: Vec<&str> = arg_elems.iter().map(|e| elem_tff_sort(e, tc)).collect();
    let best = numeric_best_sort(&arg_sorts);
    if !is_numeric_sort(best) { return base_name; }
    // Only select the numeric variant when ALL args are numeric.  The poly
    // variant replaces every $i position with the target sort; if any arg is
    // $i-typed (e.g. a List-returning function whose range is Entity -> $i),
    // the variant declaration would still have a sort mismatch at that position.
    if arg_sorts.iter().any(|&s| s == "$i") { return base_name; }
    let suffix = match best { "$int" => "int", "$rat" => "rat", _ => "real" };
    format!("{}__{}", base_name, suffix)
}

/// Translate a single sentence (list node in the KIF store) to a TPTP string.
///
/// A KIF sentence is either:
///   - An **operator sentence** (head is `=>`, `and`, `forall`, ...) -> delegates
///     to `translate_operator_sentence`.
///   - A **predicate/function application** (head is a Symbol or Variable).
///
/// ## FOF path (predicate/function application)
///
/// Formula position (`as_formula = true`):
///   All predicate applications use the "holds" encoding:
///   `(instance ?X Animal)` -> `s__holds(s__instance__m, V__X, s__Animal)`.
///   The predicate symbol is passed in *mention form* (has_args=false, so the
///   `__m` suffix is appended) as the first argument to `s__holds`.
///
/// Term position (`as_formula = false`):
///   The sentence becomes a function application: `(ListFn a b)` -> `s__ListFn(a,b)`.
///   When the head is a Variable (higher-order application), `s__holds_app` is used.
///
/// ## TFF path (predicate/function application)
///
/// Formula position (`as_formula = true`):
///   * The head is declared via `ensure_declared` (idempotent).
///   * `(instance ?X NumericType)` is collapsed to `$true` to avoid sort conflicts
///     between $i (Entity domain) and numeric sorts.
///   * Math builtins (`AdditionFn`, `SubtractionFn`, ...) -> TPTP `$sum`, `$difference`, ...
///     with sort promotion and `AbsoluteValueFn` specialised for real/rat.
///   * Comparison builtins (`LessThanFn`, `GreaterThanOrEqualToFn`, ...) -> `$less`,
///     `$greatereq`, ... with sort promotion.
///   * Variable-arity predicates (arity == -1) are called as `s__Pred__N(...)` where
///     N is the actual argument count, matching the arity-specific TFF declarations
///     emitted by `ensure_declared`.
///   * Everything else -> direct predicate call `s__Pred(args...)`.
///
/// Term position (`as_formula = false`) in TFF:
///   * SUMO functions (-> $i) are emitted as term calls.
///   * SUMO relations/predicates (-> $o) in term position would produce a $o-sorted
///     term, which Vampire rejects.  They are instead reified via
///     `s__holds_app(s__Pred__m, args...)`.
///   * Variable-arity relations use the bare symbol (already declared `$i`) rather
///     than the `__m` form.
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

                // TFF: (instance ?X NumericType) can't be expressed as a typed
                // predicate call because $i (Entity domain) and numeric sorts are
                // incompatible.  The Java SUMOtoTFAform translator eliminates these
                // calls by substituting the defining arithmetic constraint.  We
                // approximate this by returning `$true` -- the call is dropped, and
                // the variable keeps its numeric sort from Pass 1 so arithmetic
                // builtins remain valid.
                if as_formula && head_name == "instance" && sentence.elements.len() == 3 {
                    if let (Element::Variable { .. }, Element::Symbol(type_id)) =
                        (&sentence.elements[1], &sentence.elements[2])
                    {
                        if tc.layer.sort_for(tc.store.sym_name(*type_id)) != crate::semantic::Sort::Individual {
                            return "$true".to_string();
                        }
                    }
                }

                // Block arithmetic builtins when any *variable* argument renders as
                // $i in TFF -- either explicitly annotated $i or absent from var_types
                // (absent variables default to $i in the quantifier prefix).
                //
                // Using builtins like $less/$greatereq with a $i variable produces
                // a Vampire TFF sort error ("$less not used with a single sort").
                // Only use builtins when all variable arguments have an explicit
                // numeric sort annotation ($int/$real/$rat).
                //
                // Sub (function application) arguments are NOT checked here: their
                // sort is determined by elem_tff_sort via range declarations, and
                // Entity-range functions ($i return) are handled by
                // specialize_entity_fn_for_numeric in the builtin path.
                let arg_elems = &sentence.elements[1..];
                let has_i_forced_var = tc.tff.as_ref().map(|_| {
                    arg_elems.iter().any(|e| {
                        if let Element::Variable { id, .. } = e {
                            // elem_tff_sort resolves absent vars to "$i" via unwrap_or.
                            elem_tff_sort(e, tc) == "$i"
                        } else {
                            false
                        }
                    })
                }).unwrap_or(false);

                // Arithmetic functions -- valid as terms in both as_formula and !as_formula.
                if let Some(builtin) = tff_math_builtin(head_name) {
                    if !has_i_forced_var {
                        // Compute each argument's TFF sort.  All args to $sum/$difference/...
                        // must share a single numeric sort; narrower sorts are promoted
                        // ($int -> $real via $to_real) and Entity-range functions ($i) in a
                        // numeric context are replaced by type-specialised variants (__Re etc.).
                        let arg_sorts: Vec<&str> = arg_elems.iter()
                            .map(|e| elem_tff_sort(e, tc))
                            .collect();
                        let best = numeric_best_sort(&arg_sorts);
                        let promoted: Vec<String> = arg_elems.iter().zip(arg_sorts.iter())
                            .map(|(elem, &sort)| {
                                if sort == "$i" && is_numeric_sort(best) {
                                    // Entity-returning function in numeric position: specialise.
                                    specialize_entity_fn_for_numeric(elem, best, tc)
                                        .unwrap_or_else(|| translate_element(elem, tc, false))
                                } else {
                                    promote_to_sort(translate_element(elem, tc, false), sort, best)
                                }
                            })
                            .collect();
                        // SuccessorFn/PredecessorFn add an implicit `1` literal.
                        let implicit_one = || promote_to_sort("1".to_string(), "$int", best);
                        // DivisionFn: $quotient_e is integer-only.  For real/rat
                        // args use $quotient (the standard real-number quotient).
                        let effective_builtin = if head_name == "DivisionFn"
                            && matches!(best, "$real" | "$rat")
                        {
                            "$quotient"
                        } else {
                            builtin
                        };
                        return match head_name {
                            "SuccessorFn"   => format!("{}({},{})", effective_builtin, promoted[0], implicit_one()),
                            "PredecessorFn" => format!("{}({},{})", effective_builtin, promoted[0], implicit_one()),
                            // $abs is integer-only in Vampire.  For real/rat arguments,
                            // emulate |X| as $ite($greatereq(X, zero), X, $uminus(X)).
                            "AbsoluteValueFn" if matches!(best, "$real" | "$rat") => {
                                let zero = if best == "$rat" { "0/1" } else { "0.0" };
                                let arg = &promoted[0];
                                format!("$ite($greatereq({},{}),{},$uminus({}))", arg, zero, arg, arg)
                            }
                            _               => format!("{}({})", effective_builtin, promoted.join(",")),
                        };
                    }
                    // Fall through to normal SUMO-name translation below.
                }
                if as_formula {
                    // Comparison predicates -> TFF builtins (only when args are numeric).
                    if let Some(builtin) = tff_comparison_builtin(head_name) {
                        if !has_i_forced_var {
                            // All args to $less/$greatereq/... must share a single sort.
                            // Same Entity-function specialisation as the math builtin path.
                            let arg_sorts: Vec<&str> = arg_elems.iter()
                                .map(|e| elem_tff_sort(e, tc))
                                .collect();
                            let best = numeric_best_sort(&arg_sorts);
                            let promoted: Vec<String> = arg_elems.iter().zip(arg_sorts.iter())
                                .map(|(elem, &sort)| {
                                    if sort == "$i" && is_numeric_sort(best) {
                                        specialize_entity_fn_for_numeric(elem, best, tc)
                                            .unwrap_or_else(|| translate_element(elem, tc, false))
                                    } else {
                                        promote_to_sort(translate_element(elem, tc, false), sort, best)
                                    }
                                })
                                .collect();
                            return format!("{}({})", builtin, promoted.join(","));
                        }
                        // $i-forced variable -- use a $i-argument dummy variant of
                        // the comparison predicate so the output remains valid TFF.
                        //
                        // The TFF comparison builtins ($less, $greater, ...) are
                        // skipped by `ensure_declared`, so `s__lessThan` has no type
                        // declaration.  We emit an `__i`-suffixed variant on demand
                        // (e.g. `s__lessThan__i: ($i * $i) > $o`) and use that name.
                        // Numeric literals are encoded as `n__N` symbols ($i-sorted).
                        //
                        // Guard: only use the __i variant if ALL args can be rendered
                        // as $i.  Sub-expressions that return a numeric sort (e.g.
                        // `ListLengthFn` returning $int) cannot be coerced to $i.
                        // When such a mixed-sort comparison is detected we emit $true
                        // (W016 degradation -- the formula is unprovable anyway since
                        // the variable's sort is inconsistent with its arithmetic use).
                        let all_args_i = arg_elems.iter().all(|e| match e {
                            Element::Variable { .. } => true,          // already $i
                            Element::Literal(Literal::Number(_)) => true, // -> n__N ($i)
                            _ => !is_numeric_sort(elem_tff_sort(e, tc)),  // Sub must be $i
                        });
                        if !all_args_i {
                            log::warn!(
                                target: "sumo_kb::tptp",
                                "W016 tff-sort-conflict: '{}' has mixed $i/numeric args \
                                 in fallback path; dropping comparison (-> $true)",
                                head_name
                            );
                            return "$true".to_string();
                        }
                        let base_tptp  = translate_symbol(head_name, true, Some(head_id), tc.layer);
                        let i_variant  = format!("{}__{}", base_tptp, "i");
                        if let Some(cell) = &tc.tff {
                            let is_new = cell.borrow_mut().specialized_decls.insert(i_variant.clone());
                            if is_new {
                                let n   = arg_elems.len();
                                let sig = match n {
                                    0 => "$o".to_string(),
                                    1 => "($i) > $o".to_string(),
                                    _ => format!("({}) > $o", vec!["$i"; n].join(" * ")),
                                };
                                cell.borrow_mut().decl_lines.push(format!(
                                    "tff({}, type, {}: {}).",
                                    format!("type_{}", i_variant.to_lowercase()), i_variant, sig
                                ));
                            }
                        }
                        let safe_args: Vec<String> = arg_elems.iter()
                            .map(|e| encode_as_i_term(e, tc))
                            .collect();
                        return format!("{}({})", i_variant, safe_args.join(","));
                    }
                    // Direct predicate call -- no holds encoding.
                    // Variable-arity relations use arity-suffixed names (s__Pred__N)
                    // so Vampire can match the sort-specific declaration emitted by
                    // ensure_declared.  Fixed-arity predicates use the plain name.
                    // If the predicate has polymorphic variants (numeric-ancestor
                    // domain class) and the best argument sort is numeric, select
                    // the sort-suffixed variant (e.g. s__Pred__N__int).
                    let base_str  = translate_symbol(head_name, true, Some(head_id), tc.layer);
                    let arity_str = if tc.layer.arity(head_id) == Some(-1) {
                        format!("{}__{}", base_str, args.len())
                    } else {
                        base_str
                    };
                    let head_str = poly_variant_name(
                        arity_str.clone(), head_id, &sentence.elements[1..], tc);
                    // Detect unresolvable mixed-sort calls on poly-variant predicates:
                    // poly_variant_name returned the base name (no variant selected)
                    // even though some argument is numeric.  The base $i declaration
                    // can't accept numeric-sorted arguments either (e.g. a variable
                    // confirmed-numeric by a ListOrderFn position).  Emit $true so
                    // the output is type-valid (the axiom is semantically weakened).
                    if head_str == arity_str
                        && tc.layer.has_poly_variant_args(head_id)
                        && sentence.elements[1..].iter().any(|e| is_numeric_sort(elem_tff_sort(e, tc)))
                    {
                        log::warn!(
                            target: "sumo_kb::tptp",
                            "W016 tff-sort-conflict: '{}' poly-variant has mixed \
                             $i/numeric args with no valid variant; dropping call (-> $true)",
                            head_name
                        );
                        return "$true".to_string();
                    }
                    return format!("{}({})", head_str, args.join(","));
                }
                // !as_formula in TFF: the head is in term position and must
                // produce a $i-sorted result.
                //
                // - SUMO functions already return $i -> standard term call.
                // - SUMO relations/predicates are declared `> $o`; a direct call
                //   would produce a $o-sorted term, rejected by Vampire when the
                //   enclosing predicate expects $i (e.g. holdsDuring, modalAttribute).
                //   Encode as holds_app(relation__m, args...) which is $i-sorted.
                // - Variable-arity relations (arity -1) are already declared as
                //   `$i` for the symbol itself, so use it directly as the first
                //   holds_app argument without an extra __m suffix.
                // - Unknown symbols: fall back to the direct term form and let
                //   Vampire infer the type from context.
                if tc.layer.is_function(head_id) {
                    let base_str  = translate_symbol(head_name, true, Some(head_id), tc.layer);
                    // Variable-arity functions: append __N (actual argument count)
                    // to match the arity-specific TFF declaration emitted by
                    // ensure_declared.  Fixed-arity functions use the plain name.
                    // Then apply polymorphic variant selection if needed.
                    let arity_str = if tc.layer.arity(head_id) == Some(-1) {
                        format!("{}__{}", base_str, args.len())
                    } else {
                        base_str
                    };
                    let head_str = poly_variant_name(
                        arity_str.clone(), head_id, &sentence.elements[1..], tc);
                    // `poly_selected` is true when poly_variant_name chose a numeric variant.
                    let poly_selected = head_str != arity_str;
                    // Promote narrower numeric args to the declared arg sort.
                    // E.g. `s__ReciprocalFn: ($real) > $real` called with $int ->
                    // wrap with `$to_real(...)`.
                    //
                    // When a numeric-declared arg position receives a `$i` actual arg
                    // (e.g. `TangentFn(?DEGREE: $i)` where TangentFn: $real > $real),
                    // create a `__entity` variant that accepts `$i` at those positions.
                    // This handles SUMO trig/transcendental functions whose args are
                    // physical quantities ($i) rather than pure TFF numbers.
                    //
                    // NOTE: Extract base_sorts before calling elem_tff_sort to avoid
                    // holding a Ref<SortAnnotations> across a nested borrow.
                    let (base_sorts, ret_sort_str): (Vec<Sort>, &'static str) = {
                        let sa = tc.layer.sort_annotations();
                        let sorts = sa.as_ref().and_then(|sa| sa.symbol_arg_sorts.get(&head_id).cloned()).unwrap_or_default();
                        let ret = sa.as_ref().and_then(|sa| sa.symbol_return_sorts.get(&head_id).copied())
                            .unwrap_or(Sort::Individual).tptp();
                        (sorts, ret)
                    };
                    let rest = base_sorts.last().copied().unwrap_or(crate::semantic::Sort::Individual);
                    // Compute actual sort for each arg.
                    let actual_sorts: Vec<&str> = sentence.elements[1..].iter()
                        .map(|e| elem_tff_sort(e, tc))
                        .collect();
                    // Detect any sort mismatch between declared and actual arg sorts.
                    // Two cases:
                    //   a) decl=numeric, actual=$i -> function expects $real but gets Entity
                    //      (e.g. TangentFn with physical-quantity variable)
                    //   b) decl=$i, actual=numeric -> function expects Entity but gets $int
                    //      (e.g. SpeedFn called with var typed $int by a greaterThan context)
                    // In both cases, create a sort-coerced variant using actual arg sorts.
                    let needs_coerced_variant = !poly_selected && base_sorts.iter().enumerate().any(|(i, s)| {
                        let decl = s.tptp();
                        let actual = actual_sorts.get(i).copied().unwrap_or("$i");
                        decl != actual && (is_numeric_sort(decl) || is_numeric_sort(actual))
                    });
                    if needs_coerced_variant {
                        // Build variant name from actual arg sorts (compact encoding).
                        let sort_code = |s: &str| match s {
                            "$int"  => "n",
                            "$rat"  => "a",
                            "$real" => "r",
                            _       => "i",
                        };
                        let sort_suffix = actual_sorts.iter().map(|s| sort_code(s)).collect::<String>();
                        let coerced_name = format!("{}__{}", head_str, sort_suffix);
                        if let Some(cell) = &tc.tff {
                            let is_new = cell.borrow_mut().specialized_decls.insert(coerced_name.clone());
                            if is_new {
                                // Use actual sorts for all arg positions.
                                let decl_sorts: Vec<&str> = actual_sorts.iter().copied().collect();
                                let sig = if decl_sorts.is_empty() {
                                    ret_sort_str.to_string()
                                } else {
                                    format!("({}) > {}", decl_sorts.join(" * "), ret_sort_str)
                                };
                                cell.borrow_mut().decl_lines.push(format!(
                                    "tff({}, type, {}: {}).",
                                    format!("type_{}", coerced_name.to_lowercase()),
                                    coerced_name, sig
                                ));
                            }
                        }
                        return format!("{}({})", coerced_name, args.join(","));
                    }
                    let promoted_args: Vec<String> = sentence.elements[1..].iter()
                        .zip(args.iter())
                        .enumerate()
                        .map(|(i, (elem, arg_str))| {
                            let decl = base_sorts.get(i).copied().unwrap_or(rest).tptp();
                            let actual = elem_tff_sort(elem, tc);
                            promote_to_sort(arg_str.clone(), actual, decl)
                        })
                        .collect();
                    return format!("{}({})", head_str, promoted_args.join(","));
                } else if tc.layer.is_relation(head_id) || tc.layer.is_predicate(head_id) {
                    let mention = if tc.layer.arity(head_id) == Some(-1) {
                        // Variable-arity: symbol itself is the $i mention constant.
                        translate_symbol(head_name, true, Some(head_id), tc.layer)
                    } else {
                        // Fixed-arity: use the __m form (has_args=false adds __m).
                        translate_symbol(head_name, false, Some(head_id), tc.layer)
                    };
                    // Warn if any arg has a numeric sort -- holds_app requires $i.
                    if tc.opts.lang == TptpLang::Tff {
                        for (i, e) in sentence.elements[1..].iter().enumerate() {
                            let s = elem_tff_sort(e, tc);
                            if is_numeric_sort(s) {
                                log::warn!(target: "sumo_kb::tptp",
                                    "W016 tff-sort-conflict: '{}' arg {} in term position \
                                     (holds_app) has numeric sort {} -- this will cause \
                                     a Vampire sort error",
                                    head_name, i + 1, s);
                            }
                        }
                    }
                    let mut app_args = vec![mention];
                    app_args.extend(args);
                    return format!("{}holds_app({})", TPTP_SYMBOL_PREFIX, app_args.join(","));
                } else {
                    // Unknown classification: direct term, Vampire infers sort.
                    let head_str = translate_symbol(head_name, true, Some(head_id), tc.layer);
                    return format!("{}({})", head_str, args.join(","));
                }
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

/// Translate a KIF operator sentence to TPTP.
///
/// Operator sentences have a KIF logical operator as their head:
/// `=>`, `<=>`, `and`, `or`, `not`, `forall`, `exists`, `equal`.
///
/// ## Term-position encoding (`as_formula = false`)
///
/// When an operator sentence appears as an argument to a predicate (e.g.
/// `(holdsDuring I (=> P Q))`) TPTP cannot represent it as a formula-valued
/// term.  We encode it as a plain function application:
///   `(and P Q)` -> `s__and(s__P, s__Q)`
/// Exception: `forall`/`exists` use proper quantifier syntax even in term
/// position, because KIF allows formulas-as-objects (higher-order) that TPTP
/// can only approximate with quantifier syntax.
///
/// ## Formula-position encoding (`as_formula = true`)
///
///   `and` / `or`   -> `(A & B & ...)` / `(A | B | ...)`
///   `not`          -> `~(A)`
///   `=>`           -> `(A => B)`
///   `<=>`          -> `((A => B) & (B => A))`  -- TPTP has `<=>` but the
///                     biconditional is spelled out for compatibility.
///   `equal`        -> `(A = B)`.  In TFF, if one side is $i-sorted and the
///                    other is numeric, the $i side is replaced by a
///                    sort-specialised function variant to avoid a sort mismatch.
///   `forall`/`exists` ->
///     FOF: `(! [V__X, V__Y] : (body))`  -- untyped variable list.
///     TFF: `(! [V__X: $int, V__Y: $i] : (body))` -- sorts from `var_types` map.
///          The body is translated first (may call `ensure_declared` via
///          borrow_mut); the variable list is read after so the borrow is safe.
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

    // If its not supposed to be translated as formula.
    // Quantifiers (forall/exists) must always use quantifier syntax even in term position
    // because KIF allows formulas-as-objects (higher-order) which TPTP can only approximate
    // with quantifier syntax. All other operators are encoded as function terms.
    if !as_formula && !matches!(op, OpKind::ForAll | OpKind::Exists) {
        // TFF: `equal` in term position (reified proposition) needs a sort-specific
        // declaration so Vampire can verify argument sorts.  The base declaration is
        // `s__equal: ($i * $i) > $i`; when an arg has a numeric sort (e.g. from
        // CardinalityFn returning $int) we create a sort-coerced variant, e.g.
        // `s__equal__ni: ($int * $i) > $i`.  All other operators (and, or, not, ...)
        // use the generic function-term encoding below.
        if tc.opts.lang == TptpLang::Tff && op == OpKind::Equal {
            let sort_code = |s: &str| match s {
                "$int" => "n", "$rat" => "a", "$real" => "r", _ => "i",
            };
            let actual_sorts: Vec<&str> = args.iter()
                .map(|e| elem_tff_sort(e, tc))
                .collect();
            let base_name = format!("{}equal", TPTP_SYMBOL_PREFIX);
            let variant_name = if actual_sorts.iter().all(|&s| s == "$i") {
                base_name.clone()
            } else {
                let suffix: String = actual_sorts.iter().map(|s| sort_code(s)).collect();
                format!("{}__{}", base_name, suffix)
            };
            if let Some(cell) = &tc.tff {
                let is_new = cell.borrow_mut().specialized_decls.insert(variant_name.clone());
                if is_new {
                    let sig = if actual_sorts.len() == 1 {
                        format!("({}) > $i", actual_sorts[0])
                    } else {
                        format!("({}) > $i", actual_sorts.join(" * "))
                    };
                    cell.borrow_mut().decl_lines.push(format!(
                        "tff({}, type, {}: {}).",
                        format!("type_{}", variant_name.to_lowercase()), variant_name, sig
                    ));
                }
            }
            let arg_strs: Vec<String> = args.iter()
                .map(|e| translate_element(e, tc, false))
                .collect();
            return format!("{}({})", variant_name, arg_strs.join(","));
        }
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
            // TFF: equality requires both sides to have the same sort.
            // When an Entity-range SUMO function (-> $i) is equated with a
            // numeric-sorted variable, create a type-specialized variant of
            // the function with the numeric return sort -- mirroring Java's
            // `makePredFromArgTypes` / type-suffix approach.
            if tc.opts.lang == TptpLang::Tff {
                let a_sort = elem_tff_sort(args[0], tc);
                let b_sort = elem_tff_sort(args[1], tc);
                if a_sort == "$i" && is_numeric_sort(b_sort) {
                    if let Some(a_str) = specialize_entity_fn_for_numeric(args[0], b_sort, tc) {
                        let b_str = translate_element(args[1], tc, false);
                        return format!("({} = {})", a_str, b_str);
                    } else {
                        log::warn!(target: "sumo_kb::tptp",
                            "W016 tff-sort-conflict: equality has $i term vs {} but \
                             cannot coerce left side; dropping (-> $true)",
                            b_sort);
                        return "$true".to_string();
                    }
                } else if b_sort == "$i" && is_numeric_sort(a_sort) {
                    if let Some(b_str) = specialize_entity_fn_for_numeric(args[1], a_sort, tc) {
                        let a_str = translate_element(args[0], tc, false);
                        return format!("({} = {})", a_str, b_str);
                    } else {
                        log::warn!(target: "sumo_kb::tptp",
                            "W016 tff-sort-conflict: equality has $i term vs {} but \
                             cannot coerce right side; dropping (-> $true)",
                            a_sort);
                        return "$true".to_string();
                    }
                } else if is_numeric_sort(a_sort) && is_numeric_sort(b_sort) && a_sort != b_sort {
                    // Both sides numeric but different sorts (e.g. $int vs $real).
                    // Promote the narrower side to match the wider sort.
                    let target = numeric_best_sort(&[a_sort, b_sort]);
                    let a_str = promote_to_sort(translate_element(args[0], tc, false), a_sort, target);
                    let b_str = promote_to_sort(translate_element(args[1], tc, false), b_sort, target);
                    return format!("({} = {})", a_str, b_str);
                }
            }
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
            // Translate body first -- may call ensure_declared (borrow_mut on tc.tff).
            // The var-list is read afterwards so the borrow() on tc.tff is safe.
            let body = translate_element(args[1], tc, true);
            let vars: Vec<String> = match args[0] {
                Element::Sub(var_sid) => {
                    let var_elems = &tc.store.sentences[tc.store.sent_idx(*var_sid)].elements;
                    if tc.opts.lang == TptpLang::Tff {
                        // Typed variable list: `V__X: $int`
                        var_elems.iter().filter_map(|e| {
                            if let Element::Variable { id, .. } = e {
                                let sort = tc.var_types.get(id).copied().unwrap_or("$i");
                                Some(format!("{}: {}", translate_variable(tc.store.sym_name(*id)), sort))
                            } else { None }
                        }).collect()
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

// -- Public API ----------------------------------------------------------------

/// Translate and quantify a single sentence given a fully-constructed `TransCtx`.
///
/// Shared by `sentence_to_tptp` and `kb_to_tptp` so that TFF mode can inject a
/// shared `TffContext` without duplicating the free-variable wrapping logic.
fn sentence_formula(sid: SentenceId, tc: &TransCtx) -> String {
    let result = translate_sentence(sid, tc, true);

    let mut free_vars: HashSet<u64> = HashSet::new();
    let mut bound_vars: HashSet<u64> = HashSet::new();
    collect_free_vars(sid, tc.store, &mut free_vars, &mut bound_vars, true);
    if free_vars.is_empty() { return result; }

    let q = if tc.opts.query { "?" } else { "!" };

    // TFF: annotate free variables with their inferred sorts.
    let mut var_strs: Vec<String> = if tc.opts.lang == TptpLang::Tff {
        free_vars.iter()
            .map(|v| {
                let sort = tc.var_types.get(v).copied().unwrap_or("$i");
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
/// (`opts.query = true` -> existential). In TFF mode the variables are
/// annotated with their inferred sorts.
pub(crate) fn sentence_to_tptp(
    sid:   SentenceId,
    layer: &SemanticLayer,
    opts:  &TptpOptions,
) -> String {
    let (tff_cell, var_types) = if opts.lang == TptpLang::Tff {
        let mut ctx = TffContext::new();
        let vt = infer_var_types(sid, &layer.store, layer, &mut ctx, opts);
        (Some(RefCell::new(ctx)), vt)
    } else {
        (None, HashMap::new())
    };
    let tc = TransCtx { store: &layer.store, opts, layer, tff: tff_cell.as_ref(), var_types };
    sentence_formula(sid, &tc)
}

/// Render a set of root sentences as a TPTP string.
///
/// `all_roots`       -- ordered list of all root SentenceIds to render.
/// `axiom_ids`       -- sentences rendered with role `axiom`.
/// `assertion_ids`   -- sentences rendered with role `hypothesis`.
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
            let tc = TransCtx { store: &layer.store, opts, layer, tff: Some(cell), var_types: vt };
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
        if opts.show_kif_comment {
            let kif = crate::kif_store::sentence_to_plain_kif(sid, &layer.store);
            axiom_lines.push(format!("% {}", kif));
        }
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
