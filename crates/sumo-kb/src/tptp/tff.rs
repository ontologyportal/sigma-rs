// -- tff.rs --------------------------------------------------------------------
//
// TFF-specific infrastructure: sort translation, type declarations, and
// variable sort inference.
//
// -- Why TFF needs special handling -------------------------------------------
//
// FOF (First-Order Form) treats every term as the same untyped sort `$i`.
// TFF (Typed First-order Form) introduces four incompatible base sorts:
//
//   $i     -- ontological individuals (the default SUMO universe)
//   $int   -- machine integers
//   $rat   -- rational numbers
//   $real  -- real numbers
//
// Vampire rejects a formula the moment a $int value is passed to a predicate
// expecting $i, or two sorts appear in the same arithmetic builtin.  To avoid
// this, every variable must be assigned the *most specific* sort consistent
// with all its uses, and every predicate/function must have a `tff(name, type,
// ...)` declaration that describes its argument and return sorts.
//
// -- Pipeline overview ---------------------------------------------------------
//
//  kb_to_tptp (translate.rs)
//   |
//   +-- creates one shared TffContext for the entire KB
//   |
//   +-- for each sentence:
//       |
//       +-- infer_var_types(sid, ...)
//       |    Pass 0: walk sentence tree, call ensure_declared for every head
//       |            symbol -> accumulates tff.decl_lines type declarations.
//       |            Sort data is read from SemanticLayer::sort_annotations()
//       |            (precomputed KB-wide from domain/range axioms).
//       |    VTI:    look up each variable's sort in SemanticLayer::var_type_inference()
//       |            (precomputed KB-wide from instance/domain axioms via LCA).
//       |            Only non-Individual (numeric) sorts are recorded; unconstrained
//       |            variables are left absent so they do not block arithmetic builtins.
//       |    Pass 3: numeric literal co-occurrence -- upgrades absent/$i variables
//       |            that share a sentence with a literal when the head has no
//       |            known sort signature.
//       |    Returns HashMap<SymbolId, &'static str> stored in TransCtx::var_types.
//       |
//       +-- translate_sentence (translate.rs)
//            Uses tc.var_types for quantifier sort annotations
//            Uses layer.sort_annotations() for argument/return sort checks
//            has_i_forced_var: true only when a variable has an explicit
//            Some("$i") entry in var_types (positively constrained, not defaulting)

use std::collections::{HashMap, HashSet};

use crate::kif_store::KifStore;
use crate::semantic::{SemanticLayer, Sort};
use crate::types::{Element, Literal, SentenceId, SymbolId};
use super::names::{translate_symbol, TPTP_MENTION_SUFFIX};
use super::options::TptpOptions;

// -- TFF type declarations -----------------------------------------------------

/// Numeric sort suffixes for polymorphic variant declarations.
///
/// For each symbol with a numeric-ancestor domain class (mapped to `$i` in TFF),
/// `ensure_declared` emits three additional variants -- one per numeric sort --
/// so Vampire accepts calls where the argument carries a numeric sort.
const NUMERIC_POLY_SORTS: &[(&str, &str)] = &[
    ("$int",  "int"),
    ("$rat",  "rat"),
    ("$real", "real"),
];

/// Lazy accumulator for TFF type declarations.
///
/// One shared instance lives for the entire KB translation.  Symbols are
/// declared on first encounter via `ensure_declared` (idempotent via the
/// `declared` set).  After all sentences are translated, `decl_lines` holds
/// the complete preamble, prepended to the axioms in the output file.
pub(crate) struct TffContext {
    /// Symbols already declared -- O(1) dedup check, no string ops.
    pub(crate) declared:          HashSet<SymbolId>,
    /// SymbolIds whose `__m` mention constant has already been declared.
    pub(crate) declared_mentions: HashSet<SymbolId>,
    /// Accumulated `tff(name, type, sig).` lines in encounter order.
    /// These become the type preamble in the output file.
    pub(crate) decl_lines:        Vec<String>,
    /// Tracks already-emitted arity-specialised variant declarations for
    /// Entity-range functions in numeric contexts (e.g. `ListOrderFn__Re`).
    /// Prevents duplicate `tff(type_...)` lines when the same Entity-range
    /// function appears multiple times in numeric equality or builtin position.
    pub(crate) specialized_decls: HashSet<String>,
}

impl TffContext {
    pub(crate) fn new() -> Self {
        TffContext {
            declared:          HashSet::new(),
            declared_mentions: HashSet::new(),
            decl_lines:        Vec::new(),
            specialized_decls: HashSet::new(),
        }
    }

    /// Ensure `elem` has a TFF type declaration if it is a `Symbol`.
    ///
    /// This is called in two places:
    ///  1. Pass 0 of `infer_var_types` -- walks the full sentence tree so that
    ///     `signatures` and `return_sorts` are fully populated before the sort
    ///     inference passes run.
    ///  2. `translate_element` / `translate_sentence` -- lazily for any symbol
    ///     encountered during translation that wasn't reached in Pass 0.
    ///
    /// # What gets emitted
    ///
    /// | Taxonomy classification | TFF declaration |
    /// |-------------------------|-----------------|
    /// | Function                | `s__F: (A * B) > R` |
    /// | Fixed-arity Relation    | `s__R: (A * B) > $o` plus `s__R__m: $i` |
    /// | Variable-arity Relation | `s__R__1` ... `s__R__MAX_ARITY` per-arity declarations plus bare `s__R: $i` |
    /// | Class / unknown         | `s__C: $i` (individual constant) |
    ///
    /// Excluded predicates (`documentation`, `domain`, ...) are skipped EXCEPT
    /// for structural meta-predicates (`domain`, `range`, `domainSubclass`,
    /// `rangeSubclass`) which DO appear nested inside implications and need a
    /// correct TFF declaration so Vampire can check their integer-typed args.
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

        if self.declared.contains(&id) { return; }  // idempotent
        self.declared.insert(id);

        let name = layer.store.sym_name(id);

        // -- Step 1: skip TFF declaration output for suppressed symbols --------
        //
        // Pure annotation predicates never appear as nested sub-formulas so
        // omitting their declarations is harmless.
        //
        // Structural meta-predicates (domain, range, ...) are excluded from the
        // KB axiom list but DO appear nested inside implications.  They need a
        // correct TFF declaration so Vampire can verify their integer arguments
        // (e.g. arg 2 of `domain` is PositiveInteger -> $int).
        if opts.excluded.contains(name) && !is_structural_meta(name) { return; }
        // TFF arithmetic builtins ($sum, $less, ...) are built-in to Vampire and
        // must not be re-declared -- Vampire would reject a duplicate declaration.
        if tff_math_builtin(name).is_some() || tff_comparison_builtin(name).is_some() { return; }

        // -- Step 2: build and push the TFF declaration string -----------------

        let tptp_name  = translate_symbol(name, true, Some(id), layer);
        let axiom_name = format!("type_{}", tptp_name.to_lowercase());

        let sa_guard = layer.sort_annotations();
        let sa = sa_guard.as_ref().unwrap();
        let has_poly = layer.has_poly_variant_args(id);

        let sig = if layer.is_function(id) {
            let arg_sorts = sa.symbol_arg_sorts.get(&id).cloned().unwrap_or_default();
            let ret_sort  = sa.symbol_return_sorts.get(&id).copied().unwrap_or(Sort::Individual);

            if layer.arity(id) == Some(-1) {
                // -- Variable-arity function -----------------------------------
                let fn_rest = arg_sorts.last().copied().unwrap_or(Sort::Individual);
                for n in 1..=crate::parse::macros::MAX_ROW_ARITY {
                    let arity_sorts: Vec<&str> = (0..n)
                        .map(|i| arg_sorts.get(i).copied().unwrap_or(fn_rest).tptp())
                        .collect();
                    let arity_name  = format!("{}__{}", tptp_name, n);
                    let axiom_label = format!("type_{}", arity_name.to_lowercase());
                    self.decl_lines.push(format!(
                        "tff({}, type, {}: {}).",
                        axiom_label, arity_name, format_function_sig(&arity_sorts, ret_sort.tptp())
                    ));
                    if has_poly && arity_sorts.iter().any(|&s| s == "$i") {
                        emit_poly_variants_fn(&mut self.decl_lines,
                            &arity_name, &arity_sorts, ret_sort.tptp());
                    }
                }
                // Bare constant for unapplied / term-position uses.
                format!("{}: $i", tptp_name)
            } else {
                // Fixed-arity function: `s__F: (A * B) > R`
                let arg_strs: Vec<&str> = arg_sorts.iter().map(|s| s.tptp()).collect();
                format!("{}: {}", tptp_name, format_function_sig(&arg_strs, ret_sort.tptp()))
            }

        } else if layer.is_relation(id) || layer.is_predicate(id) {
            if layer.arity(id) == Some(-1) {
                // -- Variable-arity relation -----------------------------------
                let base_sorts = sa.symbol_arg_sorts.get(&id).cloned().unwrap_or_default();
                let rel_rest = base_sorts.last().copied().unwrap_or(Sort::Individual);
                for n in 1..=crate::parse::macros::MAX_ROW_ARITY {
                    let arity_sorts: Vec<&str> = (0..n)
                        .map(|i| base_sorts.get(i).copied().unwrap_or(rel_rest).tptp())
                        .collect();
                    let arity_name  = format!("{}__{}", tptp_name, n);
                    let axiom_label = format!("type_{}", arity_name.to_lowercase());
                    self.decl_lines.push(format!(
                        "tff({}, type, {}: {}).",
                        axiom_label, arity_name, format_relation_sig(&arity_sorts)
                    ));
                    if has_poly && arity_sorts.iter().any(|&s| s == "$i") {
                        emit_poly_variants_rel(&mut self.decl_lines,
                            &arity_name, &arity_sorts);
                    }
                }
                // Bare constant declaration (for term-position / instance uses).
                format!("{}: $i", tptp_name)
            } else {
                // Fixed-arity relation/predicate: `s__R: (A * B) > $o`
                let arg_sorts = sa.symbol_arg_sorts.get(&id).cloned().unwrap_or_default();
                let arg_strs: Vec<&str> = arg_sorts.iter().map(|s| s.tptp()).collect();
                format!("{}: {}", tptp_name, format_relation_sig(&arg_strs))
            }
        } else {
            // Individual constant: look up sort from instance edges.
            // E.g. (instance Pi PositiveRealNumber) -> s__Pi: $real.
            let sort = sa.symbol_individual_sorts.get(&id)
                .copied()
                .unwrap_or(crate::semantic::Sort::Individual)
                .tptp();
            format!("{}: {}", tptp_name, sort)
        };

        self.decl_lines.push(format!("tff({}, type, {}).", axiom_name, sig));

        // Polymorphic variants for fixed-arity functions and relations.
        // (Variable-arity variants are emitted inside the arity loop above.)
        if has_poly {
            if layer.is_function(id) && layer.arity(id) != Some(-1) {
                let arg_sorts = sa.symbol_arg_sorts.get(&id).cloned().unwrap_or_default();
                let ret_sort  = sa.symbol_return_sorts.get(&id).copied().unwrap_or(Sort::Individual);
                let arg_strs: Vec<&str> = arg_sorts.iter().map(|s| s.tptp()).collect();
                if arg_strs.iter().any(|&s| s == "$i") {
                    emit_poly_variants_fn(&mut self.decl_lines,
                        &tptp_name, &arg_strs, ret_sort.tptp());
                }
            } else if (layer.is_relation(id) || layer.is_predicate(id)) && layer.arity(id) != Some(-1) {
                let arg_sorts = sa.symbol_arg_sorts.get(&id).cloned().unwrap_or_default();
                let arg_strs: Vec<&str> = arg_sorts.iter().map(|s| s.tptp()).collect();
                if arg_strs.iter().any(|&s| s == "$i") {
                    emit_poly_variants_rel(&mut self.decl_lines,
                        &tptp_name, &arg_strs);
                }
            }
        }

        // -- Step 4: emit __m mention constant for fixed-arity predicates ------
        //
        // When a fixed-arity predicate appears in *term* position -- e.g. as
        // the first argument of `holds` in higher-order axioms -- Vampire needs
        // to know its sort.  We emit `s__pred__m: $i` (the "mention constant")
        // for this purpose.
        //
        // Variable-arity relations already have `s__R: $i` from Step 3 above
        // and are used bare (not __m) in the holds_app path, so no extra decl.
        if (layer.is_relation(id) || layer.is_predicate(id))
            && layer.arity(id) != Some(-1)
            && self.declared_mentions.insert(id)
        {
            let mention_name  = format!("{}{}", tptp_name, TPTP_MENTION_SUFFIX);
            let mention_axiom = format!("type_{}", mention_name.to_lowercase());
            self.decl_lines.push(format!("tff({}, type, {}: $i).", mention_axiom, mention_name));
        }
    }
}


/// Emit `__int`, `__rat`, `__real` polymorphic variants for a TFF function.
///
/// For each numeric sort in `NUMERIC_POLY_SORTS`, replaces every `$i` position
/// in `base_arg_sorts` with the target sort and emits a new declaration.
/// Positions that are already numeric (e.g. `$int`) are left unchanged, so
/// mixed-signature functions get correctly typed variants.
fn emit_poly_variants_fn(
    decl_lines:    &mut Vec<String>,
    base_name:     &str,
    base_arg_sorts: &[&str],
    ret_sort:      &str,
) {
    for &(poly_sort, suffix) in NUMERIC_POLY_SORTS {
        let poly_sorts: Vec<&str> = base_arg_sorts.iter()
            .map(|&s| if s == "$i" { poly_sort } else { s })
            .collect();
        let poly_name  = format!("{}__{}", base_name, suffix);
        let poly_label = format!("type_{}", poly_name.to_lowercase());
        decl_lines.push(format!(
            "tff({}, type, {}: {}).",
            poly_label, poly_name, format_function_sig(&poly_sorts, ret_sort)
        ));
    }
}

/// Emit `__int`, `__rat`, `__real` polymorphic variants for a TFF relation.
///
/// Same logic as `emit_poly_variants_fn` but uses `format_relation_sig`
/// (result sort `$o`) instead of `format_function_sig`.
fn emit_poly_variants_rel(
    decl_lines:    &mut Vec<String>,
    base_name:     &str,
    base_arg_sorts: &[&str],
) {
    for &(poly_sort, suffix) in NUMERIC_POLY_SORTS {
        let poly_sorts: Vec<&str> = base_arg_sorts.iter()
            .map(|&s| if s == "$i" { poly_sort } else { s })
            .collect();
        let poly_name  = format!("{}__{}", base_name, suffix);
        let poly_label = format!("type_{}", poly_name.to_lowercase());
        decl_lines.push(format!(
            "tff({}, type, {}: {}).",
            poly_label, poly_name, format_relation_sig(&poly_sorts)
        ));
    }
}

/// Format a TFF relation signature: `(A * B) > $o`.
/// A 0-argument relation becomes just `$o` (a propositional constant).
fn format_relation_sig(sorts: &[&str]) -> String {
    match sorts.len() {
        0 => "$o".to_string(),
        1 => format!("({}) > $o", sorts[0]),
        _ => format!("({}) > $o", sorts.join(" * ")),
    }
}

/// Format a TFF function signature: `(A * B) > R`.
/// A 0-argument function becomes just `R` (a constant of sort R).
fn format_function_sig(sorts: &[&str], ret: &str) -> String {
    match sorts.len() {
        0 => ret.to_string(),
        1 => format!("({}) > {}", sorts[0], ret),
        _ => format!("({}) > {}", sorts.join(" * "), ret),
    }
}

/// Returns true for SUMO meta-predicates that appear nested inside implications
/// despite being in `opts.excluded`.
///
/// These need TFF type declarations because Vampire sees them in sub-formula
/// position and must be able to type-check their arguments.  The canonical
/// example: `(=> (domain ?R ?N ?C) ...)` where arg 2 is PositiveInteger -> $int.
fn is_structural_meta(name: &str) -> bool {
    matches!(name, "domain" | "range" | "domainSubclass" | "rangeSubclass")
}

// -- TFF arithmetic / comparison builtins -------------------------------------
//
// TFF has built-in interpreted symbols for arithmetic.  When a SUMO function
// or comparison maps to one of these, we emit the TFF builtin directly rather
// than an `s__`-prefixed SUMO name.  Vampire knows the type rules for all
// these symbols without any `tff(type, ...)` declaration.
//
// Important Vampire restrictions:
//  - `$abs` is $int-only.  For $real/$rat arguments we emit an ITE expression.
//  - `$remainder_e` is $int-only.
//  - `$floor`, `$ceiling`, `$round`, `$truncate` take $real/$rat and return $int.
//  - All arithmetic builtins require all arguments to have the *same* sort.
//    When sorts differ, the narrower sort is promoted via `$to_real`/`$to_rat`.

/// Maps a SUMO function name to its TFF arithmetic builtin, if any.
///
/// `SuccessorFn` / `PredecessorFn` don't have direct TFF builtins; they are
/// rewritten to `$sum(x, 1)` / `$difference(x, 1)` with an implicit literal.
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

/// Returns true for the three TFF numeric sorts: `$int`, `$rat`, `$real`.
pub(crate) fn is_numeric_sort(sort: &str) -> bool {
    matches!(sort, "$int" | "$rat" | "$real")
}

// -- TFF variable type inference -----------------------------------------------
//
// Every TFF variable must be annotated with a sort in its quantifier binding:
//   `! [V__X: $int, V__Y: $i] : ...`
//
// This function runs *before* each sentence is translated and produces a
// `HashMap<SymbolId, sort>` that `translate_sentence` reads when building
// quantifier prefixes.
//
// Variable sorts come from the precomputed `VarTypeInference` table
// (built once from the full KB in `SemanticLayer`).  A numeric literal
// co-occurrence pass (Pass 3) handles the residual case where no domain axiom
// exists for an arithmetic builtin -- e.g. `(lessThan ?X 0)` where `lessThan`
// is not declared with SUMO domain axioms.

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

/// Infer the TFF sort for each variable appearing in the sentence tree rooted
/// at `sid`.  Returns a map from variable SymbolId to TPTP sort string.
///
/// Sorts are read from the precomputed `VarTypeInference` table.
/// Pass 2 forces direct variable arguments of TFF arithmetic/comparison
/// builtins to `$int` when unconstrained.
/// Pass 3 (literal co-occurrence) fills in residual cases via numeric literals.
pub(crate) fn infer_var_types(
    sid:   SentenceId,
    store: &KifStore,
    layer: &SemanticLayer,
    tff:   &mut TffContext,
    opts:  &TptpOptions,
) -> HashMap<SymbolId, &'static str> {
    // -- Pass 0: emit TFF type declarations for all head symbols --------------
    //
    // Walk the sentence tree and call `ensure_declared` on every head symbol.
    // This populates `tff.decl_lines` with `tff(name, type, sig).` entries.
    // For a KB-wide TffContext most symbols are already declared, making
    // this idempotent and cheap.
    walk_sentence(sid, store, &mut |elems| {
        if let Some(head) = elems.first() {
            tff.ensure_declared(head, layer, opts);
        }
    });

    // -- Build var_types from precomputed VarTypeInference --------------------
    //
    // Only insert variables that have a *non-Individual* (i.e. numeric) sort.
    // Variables absent from var_types are treated as unconstrained and default
    // to "$i" at use sites -- but crucially, they are NOT treated as positively
    // forced to $i, so arithmetic builtins can still be applied to them.
    // (Variables merely defaulting to $i must not block $less/$sum/... -- only
    // variables *explicitly* known to be $i should do so.)
    let vti_guard = layer.var_type_inference();
    let vti = vti_guard.as_ref().unwrap();
    let mut types: HashMap<SymbolId, &'static str> = HashMap::new();
    walk_sentence(sid, store, &mut |elems| {
        for elem in elems {
            if let Element::Variable { id: var_id, .. } = elem {
                if let Some(sort) = vti.var_sorts.get(var_id).copied() {
                    if sort != Sort::Individual {
                        types.entry(*var_id).or_insert(sort.tptp());
                    }
                }
            }
        }
    });

    // -- Pass 2: arithmetic builtin arg forcing --------------------------------
    //
    // A variable appearing directly as an argument to a TFF math or comparison
    // builtin must be numeric.  VTI may not cover this (the builtin has no
    // SUMO domain axioms, or its domain class is a numeric-ancestor that maps
    // to $i in the base declaration).  Force any absent variable in such a
    // position to $int so that has_i_forced_var is false at translation time
    // and the TFF builtin ($sum, $less, ...) can fire instead of falling through
    // to the symbolic SUMO-name form.
    //
    // Uses or_insert: VTI entries ($real/$rat from range axioms) take
    // precedence over the $int default.
    walk_sentence(sid, store, &mut |elems| {
        let head_is_builtin = matches!(elems.first(),
            Some(Element::Symbol(id)) if {
                let name = store.sym_name(*id);
                tff_math_builtin(name).is_some() || tff_comparison_builtin(name).is_some()
            }
        );
        if !head_is_builtin { return; }
        for elem in &elems[1..] {
            if let Element::Variable { id: var_id, .. } = elem {
                types.entry(*var_id).or_insert("$int");
            }
        }
    });

    // -- Pass 3: numeric literal co-occurrence (weak) --------------------------
    //
    // Handles `(lessThan ?X 0)` when no domain axiom exists for `lessThan`.
    // Skipped when the head has a known signature in `SortAnnotations` --
    // domain-axiom inference is authoritative and must not be overridden.
    let sa_guard = layer.sort_annotations();
    let sa = sa_guard.as_ref().unwrap();
    walk_sentence(sid, store, &mut |elems| {
        let lit_sort: Option<&'static str> = elems.iter().find_map(|e| match e {
            Element::Literal(Literal::Number(n)) => {
                if n.contains('.') { Some("$real") } else { Some("$int") }
            }
            _ => None,
        });
        let Some(lit_sort) = lit_sort else { return; };

        let head_has_sig = matches!(elems.first(),
            Some(Element::Symbol(id)) if sa.symbol_arg_sorts.contains_key(id));
        if head_has_sig { return; }

        for elem in elems {
            if let Element::Variable { id: var_id, .. } = elem {
                let entry = types.entry(*var_id).or_insert("$i");
                if *entry == "$i" {
                    *entry = lit_sort;
                }
            }
        }
    });

    // -- Pass 3.5: equality sort propagation ------------------------------------
    //
    // For `(equal ?VAR EXPR)` or `(equal EXPR ?VAR)` where EXPR returns a
    // numeric sort, assign that sort to ?VAR if it is currently absent or $i.
    // This handles cases like `(equal ?Z (AdditionFn ?X 1))` where Pass 3
    // types ?X but not ?Z (the literal is nested, not a direct sibling).
    // Also handles SUMO functions with declared range (e.g. AbsoluteValueFn).
    walk_sentence(sid, store, &mut |elems| {
        if elems.len() != 3 { return; }
        let head_is_equal = matches!(elems.first(),
            Some(Element::Op(op)) if op.name() == "equal");
        if !head_is_equal { return; }
        let a = &elems[1];
        let b = &elems[2];
        let a_sort = pass4_elem_sort(a, &types, store, &sa);
        let b_sort = pass4_elem_sort(b, &types, store, &sa);
        // Propagate from numeric b to $i/absent a (variable).
        if let Element::Variable { id: var_id, .. } = a {
            if !is_numeric_sort(a_sort) && is_numeric_sort(b_sort) {
                let entry = types.entry(*var_id).or_insert(b_sort);
                if *entry == "$i" { *entry = b_sort; }
            }
        }
        // Propagate from numeric a to $i/absent b (variable).
        if let Element::Variable { id: var_id, .. } = b {
            if !is_numeric_sort(b_sort) && is_numeric_sort(a_sort) {
                let entry = types.entry(*var_id).or_insert(a_sort);
                if *entry == "$i" { *entry = a_sort; }
            }
        }
    });

    // -- Sub-pass A: collect variables with confirmed numeric arg positions ----
    //
    // Some variables are legitimately typed as $int/$rat/$real because they
    // appear at a strictly-numeric declared position (e.g. arg2 of ListOrderFn
    // which is declared PositiveInteger -> $int).  Pass 4 must not downgrade
    // these variables: doing so would break the numeric-position call even if
    // it fixes a $i-position conflict elsewhere.
    //
    // We collect the set of variable SymbolIds that appear at positions whose
    // declared sort is non-Individual (i.e. a specific numeric sort).
    let mut confirmed_numeric_vars: std::collections::HashSet<SymbolId> = std::collections::HashSet::new();
    walk_sentence(sid, store, &mut |elems| {
        let head_sym_id = match elems.first() {
            Some(Element::Symbol(id)) => *id,
            _ => return,
        };
        let arg_sorts = sa.symbol_arg_sorts.get(&head_sym_id).cloned().unwrap_or_default();
        if arg_sorts.is_empty() { return; }
        let rest = arg_sorts.last().copied().unwrap_or(Sort::Individual);
        for (i, elem) in elems[1..].iter().enumerate() {
            let declared = arg_sorts.get(i).copied().unwrap_or(rest);
            if declared == Sort::Individual { continue; } // $i -- not a confirmed numeric use.
            if let Element::Variable { id: var_id, .. } = elem {
                confirmed_numeric_vars.insert(*var_id);
            }
        }
    });

    // -- Pass 4: sort-conflict downgrade ---------------------------------------
    //
    // A variable assigned a numeric sort (from VTI, Pass 2, or Pass 3) may
    // appear in a predicate/function argument position whose declared sort is
    // $i, with no valid TFF declaration available to absorb the mismatch.
    //
    // Two sub-cases:
    //
    //   Non-poly-variant symbols: declared $i for the arg, no numeric variant
    //   exists at all.  Any numeric variable at a $i position is a conflict.
    //
    //   Poly-variant symbols: numeric variants ARE declared (`s__Rel__int` etc.)
    //   but they replace ALL $i positions with the target sort.  When the call
    //   site has MIXED sorts -- some $i-declared args are numeric variables, while
    //   others are $i-typed sub-expressions (e.g. List-returning functions) --
    //   no variant can satisfy all positions.  We detect this by checking whether
    //   every $i-declared position is actually numeric.  If they all are, the
    //   variant works; if any is $i (non-numeric), the numeric variables must be
    //   downgraded so the base $i declaration can be used instead.
    //
    // Resolution: downgrade conflicting variables to $i (remove from types) so
    // that both the quantifier annotation and the call site agree on $i.  Emit a
    // W016 warning.  Math/comparison builtins are always skipped -- they have
    // their own $i-forced fallback paths in `translate_sentence`.
    walk_sentence(sid, store, &mut |elems| {
        let head_sym_id = match elems.first() {
            Some(Element::Symbol(id)) => *id,
            _ => return,
        };
        let head_name = store.sym_name(head_sym_id);

        // Math/comparison builtins have dedicated $i-fallback logic.
        if tff_math_builtin(head_name).is_some() || tff_comparison_builtin(head_name).is_some() {
            return;
        }

        let arg_sorts = sa.symbol_arg_sorts.get(&head_sym_id).cloned().unwrap_or_default();
        if arg_sorts.is_empty() { return; }
        let rest = arg_sorts.last().copied().unwrap_or(Sort::Individual);

        let has_poly = layer.has_poly_variant_args(head_sym_id);
        if has_poly {
            // For poly-variant symbols: check whether ALL $i-declared positions
            // have numeric actual sorts.  If so, the variant handles it cleanly
            // and no downgrade is needed.  If any $i-declared position is $i
            // (e.g. a List-returning sub-expression), the variant can't be used
            // and we must downgrade any numeric variables instead.
            let all_numeric = elems[1..].iter().enumerate().all(|(i, elem)| {
                let declared = arg_sorts.get(i).copied().unwrap_or(rest);
                if declared != Sort::Individual { return true; }
                is_numeric_sort(pass4_elem_sort(elem, &types, store, &sa))
            });
            if all_numeric { return; } // Poly variant handles it -- no conflict.
            // Fall through to the downgrade loop below (same as non-poly case).
        }

        // Downgrade any numeric variable found in a $i-declared position,
        // unless it also appears at a strictly-numeric position elsewhere in
        // the sentence (confirmed_numeric_vars) -- downgrading such a variable
        // would create a new conflict at its numeric-position call site.
        for (i, elem) in elems[1..].iter().enumerate() {
            let declared = arg_sorts.get(i).copied().unwrap_or(rest);
            if declared != Sort::Individual { continue; } // Not a $i position.

            if let Element::Variable { id: var_id, .. } = elem {
                if confirmed_numeric_vars.contains(var_id) { continue; }
                if let Some(&sort_str) = types.get(var_id) {
                    if is_numeric_sort(sort_str) {
                        log::warn!(
                            target: "sumo_kb::tptp",
                            "W016 tff-sort-conflict: '{}' arg {} expects $i but \
                             variable '{}' has sort {}; downgrading to $i",
                            head_name, i + 1,
                            store.sym_name(*var_id), sort_str
                        );
                        types.remove(var_id);
                    }
                }
            }
        }
    });

    // -- Pass 4c: holds_app term-position downgrade ----------------------------
    //
    // When a SUMO predicate/relation appears as a NON-HEAD Sub argument (i.e.,
    // in term position), translate.rs wraps it as s__holds_app(pred__m, args...)
    // where ALL args must be $i.  Numeric variables in those positions must be
    // downgraded to $i regardless of their declared domain sort (which only
    // applies in formula position).
    //
    // We do NOT consult confirmed_numeric_vars here: holds_app $i is mandatory.
    //
    // IMPORTANT: Only Symbol-headed sentences AND Op(Equal) have their Sub args
    // in term position.  Other Op-headed sentences (=>, and, or, not, forall, ...)
    // translate their Sub arguments in *formula* position (as_formula=true),
    // so Sub args of those Op-headed sentences must NOT be downgraded here.
    // `equal` is the exception: it translates both sides with as_formula=false,
    // so a predicate Sub on either side becomes s__holds_app (term position).
    walk_sentence(sid, store, &mut |elems| {
        // Only trigger under a Symbol-headed parent or Op(Equal).
        // Other Op-headed parents (=>, and, or, ...) translate Sub args as formulas.
        let parent_in_term_pos = matches!(elems.first(), Some(Element::Symbol(_)))
            || matches!(elems.first(),
                Some(Element::Op(op)) if op.name() == "equal");
        if !parent_in_term_pos { return; }
        for elem in &elems[1..] {
            let Element::Sub(sub_sid) = elem else { continue; };
            let sub_elems = &store.sentences[store.sent_idx(*sub_sid)].elements;
            let head_needs_holds_app = match sub_elems.first() {
                Some(Element::Symbol(sub_head_id)) => {
                    // Predicates/relations in term position -> holds_app.
                    // Functions in term position -> typed function call (not holds_app).
                    !layer.is_function(*sub_head_id)
                        && (layer.is_relation(*sub_head_id)
                            || layer.is_predicate(*sub_head_id)
                            || tff_comparison_builtin(store.sym_name(*sub_head_id)).is_some())
                }
                Some(Element::Op(_)) => true, // Operators in term position -> holds_app-like.
                _ => false,
            };
            if !head_needs_holds_app { continue; }
            for sub_arg in &sub_elems[1..] {
                if let Element::Variable { id: var_id, .. } = sub_arg {
                    if let Some(&sort_str) = types.get(var_id) {
                        if is_numeric_sort(sort_str) {
                            types.remove(var_id);
                        }
                    }
                }
            }
        }
    });

    types
}

/// Determine the effective TFF sort of an element for Pass 3.5 / Pass 4.
///
/// Recursive approximation of `translate::elem_tff_sort` that runs without
/// a `TransCtx`.  Handles:
///   * Variable  -> look up in `types` (numeric sort or absent -> $i)
///   * Literal   -> $int or $real based on presence of '.'
///   * Sub/math  -> most general numeric sort of args (TFF builtin semantics)
///   * Sub/if    -> most general sort of true/false branches
///   * Sub/other -> static return sort from `sort_annotations` ($i if unknown)
///   * Other     -> $i (conservative)
fn pass4_elem_sort(
    elem:  &Element,
    types: &HashMap<SymbolId, &'static str>,
    store: &KifStore,
    sa:    &crate::semantic::SortAnnotations,
) -> &'static str {
    match elem {
        Element::Variable { id, .. } => types.get(id).copied().unwrap_or("$i"),
        Element::Literal(Literal::Number(n)) => {
            if n.contains('.') { "$real" } else { "$int" }
        }
        Element::Sub(sid) => {
            let elems = &store.sentences[store.sent_idx(*sid)].elements;
            match elems.first() {
                Some(Element::Symbol(fn_id)) => {
                    let name = store.sym_name(*fn_id);
                    // TFF math builtins: return sort is most general of arg sorts.
                    if tff_math_builtin(name).is_some() {
                        return pass4_args_numeric_sort(&elems[1..], types, store, sa);
                    }
                    // SUMO `if` -> most general sort of true/false branches (args 1 and 2).
                    if name == "if" && elems.len() == 4 {
                        let t = pass4_elem_sort(&elems[2], types, store, sa);
                        let f = pass4_elem_sort(&elems[3], types, store, sa);
                        return numeric_most_general(t, f);
                    }
                    // Range axiom lookup (covers SUMO functions with declared range).
                    sa.symbol_return_sorts
                        .get(fn_id)
                        .copied()
                        .map(|s| s.tptp())
                        .unwrap_or("$i")
                }
                _ => "$i",
            }
        }
        _ => "$i",
    }
}

/// Most general numeric sort among a list of elements, defaulting to $int.
fn pass4_args_numeric_sort(
    args:  &[Element],
    types: &HashMap<SymbolId, &'static str>,
    store: &KifStore,
    sa:    &crate::semantic::SortAnnotations,
) -> &'static str {
    let mut best = "$int";
    for arg in args {
        let s = pass4_elem_sort(arg, types, store, sa);
        best = numeric_most_general(best, s);
    }
    best
}

/// More general (wider) of two numeric sorts: $int < $rat < $real.
/// Non-numeric inputs are treated as $int for the comparison.
fn numeric_most_general(a: &'static str, b: &'static str) -> &'static str {
    if a == "$real" || b == "$real" { "$real" }
    else if a == "$rat" || b == "$rat" { "$rat" }
    else { "$int" }
}
