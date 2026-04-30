// row_vars.rs
//
// Row variable expansion for SUMO/KIF formulas.
//
// ## What are row variables?
//
// KIF supports two kinds of variables:
//   ?VAR  -- a regular variable, bound to exactly one term.
//   @VAR  -- a *row variable*, bound to a sequence of zero or more terms.
//
// Row variables model variadicity.  A SUMO predicate like `ListFn` can take
// any number of arguments, and the axiom describing it is written once with
// a row variable rather than once per arity:
//
//   (=> (and (instance ?LIST List)
//            (equal ?LIST (ListFn @ROW)))
//       (forall (@ROW)
//   (inList ?ROWi ?LIST)))   ; ?ROWi is one element from @ROW
//
// Neither FOF nor TFF can express variadic quantification directly -- every
// formula must have a fixed number of arguments.  Row variables therefore
// require *expansion*: the single formula is unrolled into one concrete
// formula per possible arity.
//
// ## Expansion algorithm
// For each row variable `@VAR` in a formula, the variable is replaced with
// successively longer sequences of regular variables:
//
//   j = 1  ->  ?VAR2
//   j = 2  ->  ?VAR2 ?VAR3
//   j = 3  ->  ?VAR2 ?VAR3 ?VAR4
//   ...
//   j = MAX_ARITY  ->  ?VAR2 ?VAR3 ... ?VAR(MAX_ARITY+1)
//
// The index starts at 2.  The resulting variable names
// (`?ROW2`, `?ROW3`, ...) become ordinary KIF variables that the regular
// variable-binding machinery handles without any special treatment.
//
// When a formula contains *multiple* row variables they are expanded
// independently and the results are **cross-producted**: two row variables
// each expanded to MAX_ARITY arities produce MAX_ARITY^2 formulas.
//
// Example -- single row variable:
//
//   (P @ROW)  ->  (P ?ROW2)
//                (P ?ROW2 ?ROW3)
//                (P ?ROW2 ?ROW3 ?ROW4)
//                (P ?ROW2 ?ROW3 ?ROW4 ?ROW5)
//                (P ?ROW2 ?ROW3 ?ROW4 ?ROW5 ?ROW6)
//
// Example -- row variable in quantifier list and body (both occurrences expand):
//
//   (forall (@ROW) (P @ROW))
//     ->  (forall (?ROW2) (P ?ROW2))
//        (forall (?ROW2 ?ROW3) (P ?ROW2 ?ROW3))
//        ...
//
// ## Implementation approach
//
// Expansion operates on the parsed AST structure before symbol extraction.
//  this is done similar to how macros are done in C so that, semantically,
//  row variables are expressed as normal variables and inserted to the symbol
//  table with appropriate type tracing 
//
// The expansion runs inside `kif_store::load_kif`, immediately after initial
// parsing.  By the time a formula enters the KifStore or SemanticLayer it
// contains no row variables.  Neither the FOF nor the TFF translation path
// needs to know they ever existed.
//
// ## Relationship to VariableArityRelation in TFF
//
// SUMO marks variadic predicates with `(instance P VariableArityRelation)`.
// After row-var expansion the predicate will appear with different argument
// counts across the expanded formulas.  In TFF every argument count needs its
// own type declaration (TFF is monomorphic -- you cannot write one signature
// for all arities).  `tff::ensure_declared` detects `arity == -1` and emits
// `s__P__1`, `s__P__2`, ... `s__P__MAX_ARITY` declarations.  The call-site in
// `translate::translate_sentence` appends `__N` to the predicate name to match.

use std::{collections::HashSet};

use crate::parse::{AstNode, Parser};

/// Maximum number of argument positions a row variable is expanded to.
///
/// A row variable `@VAR` is replaced by sequences of length 1 through
/// `MAX_ARITY`, producing `MAX_ARITY` concrete formulas per row variable
/// (and `MAX_ARITY^N` for N row variables).
///
/// Matches Java's `RowVars.MAX_ARITY = 5`.
/// TODO: add ability to modify this via CLI arg
pub const MAX_ARITY: usize = 5;

/// Return `true` if an [`AstNode`] tree contains any row variable (`@`-prefixed).
///
/// Used in `kif_store::load_kif` as a fast pre-check: if a top-level parsed
/// node has no row variables it is stored directly without going through the
/// string-expansion round-trip.  Only nodes that return `true` here are
/// actually processed by the row expansion
pub(crate) fn contains_row_var(node: &AstNode) -> bool {
    match node {
        AstNode::RowVariable { .. } => true,
        AstNode::List { elements, .. } => elements.iter().any(contains_row_var),
        _ => false,
    }
}

/// Collect all row-variable names (without the `@` prefix) from a KIF string.
///
/// Scans the raw KIF text character-by-character looking for `@` followed by
/// an alphanumeric/underscore/hyphen identifier.  Names are returned in
/// first-occurrence order and deduplicated.
///
/// Example: `"(P @ROW ?X @ROW)"` -> `["ROW"]` (one occurrence, deduped).
///
/// Works on strings rather than AST nodes so that it can be called immediately
/// after `AstNode::flat()` without a separate tree walk.
pub(crate) fn find_row_var_names(kif: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut chars = kif.char_indices().peekable();
    while let Some((_, ch)) = chars.next() {
        if ch == '@' {
            let mut name = String::new();
            while let Some(&(_, nc)) = chars.peek() {
                if nc.is_alphanumeric() || nc == '_' || nc == '-' {
                    name.push(nc);
                    chars.next();
                } else {
                    break;
                }
            }
            if !name.is_empty() && seen.insert(name.clone()) {
                names.push(name);
            }
        }
    }
    names
}

/// Expand all row variables in an AST.
///
/// ## Algorithm
///
/// The expansion processes a single AST root node and produces j^N resultant nodes where
/// j is the MAX_ARITY and N is the number of row variables present in the tree
///
///   j=1: (predicate @ROW)                 -> (predicate ?ROW2)
///   j=2: (predicate @ROW)                 -> (predicate ?ROW2) (predicate ?ROW2 ?ROW3)
///   j=3: (predicate @ROW)                 -> (predicate ?ROW2) (predicate ?ROW2 ?ROW3)
///                                           (predicate ?ROW2 ?ROW3 ?ROW4)
///   j=2: (and (predicateA @A)             -> (and (predicateA ?A2) (predicateA ?B2))
///             (predicateB @B))              (and (predicateA ?A2 ?A3) (predicateA ?B2))
///                                           (and (predicateA ?A2) (predicateA ?B2 ?B3))
///                                           (and (predicateA ?A2 ?A3) (predicateA ?B2 ?B3))
///
/// Because `result` is seeded with the original formula and replaced after
/// each row variable, processing a second row variable applies the same loop
/// to each of the formulas already in `result`.  This produces the
/// cross-product: with two row variables of `MAX_ARITY = 5` the output has
/// 5 x 5 = 25 formulas.
///
/// The replacement is iterative, it converts the ASTNode back into its string
/// representation, then finds all the ROW variables, and replaces them the 
/// appropriate number of expanded normal variables (@ROW => ?ROWN)
///
/// ## Return value
///
/// - No row variables: returns `vec![kif.to_owned()]` (single node tree).
/// - N row variables:  returns up to `MAX_ARITY^N` trees (cross-product).
///   In practice N=1 is the overwhelmingly common case.
///
/// The returned strings contain only regular `?VAR` variables; no `@`-prefixed
/// variables remain.  Each string is valid KIF and can be tokenised/parsed
/// normally.
pub fn expand_row_vars(node: &AstNode, parser: &Parser) -> Vec<AstNode> {
    // Base case -- no row variables, return the original node
    if !contains_row_var(node) {
        return vec![node.to_owned()];
    }

    // First convert the node back to a string
    let str_rep = node.to_string();
    let row_vars = find_row_var_names(str_rep.as_str());
    
    crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Debug, target: "sumo_kb", message: format!("Macro expansion: row variable expansion: expanding {}", str_rep) });

    // Seed result with the original formula
    let mut result: Vec<String> = vec![str_rep];

    // For each row variable, expand every string currently in result
    // by MAX_ARITY variants -- producing the cross product
    for row_var in &row_vars {
        let mut new_result: Vec<String> = Vec::with_capacity(result.len() * MAX_ARITY);

        for s in result {
            for j in 1..=MAX_ARITY {
                // j=1 -> ?VAR2
                // j=2 -> ?VAR2 ?VAR3
                // j=3 -> ?VAR2 ?VAR3 ?VAR4  etc.
                let expansion = (2..=(j + 1))
                    .map(|n| format!("?{}{}", row_var, n))
                    .collect::<Vec<_>>()
                    .join(" ");

                let expanded = s.replace(&format!("@{}", row_var), &expansion);
                new_result.push(expanded);
            }
        }

        result = new_result;
    }

    crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Debug, target: "sumo_kb", message: format!("Macro expansion: row variable expansion: expanded into {} new sentences", result.len()) });
    crate::emit_event!(crate::progress::ProgressEvent::Log { level: crate::progress::LogLevel::Debug, target: "sumo_kb", message: format!("{}", result.join("\n")) });

    // Parse each expanded string back into an AstNode, discarding any failures
    result.into_iter()
        .flat_map(|s| parser.parse(&s, &node.span().file).0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kif(kif: &str) -> AstNode {
        // Use the raw KIF tokenizer + parser directly, bypassing macros::expand.
        // If we went through Parser::Kif.parse() it would call macros::expand()
        // which runs expand_row_vars internally -- consuming the row variables
        // before the test can exercise expand_row_vars itself.
        let (tokens, _) = crate::parse::kif::tokenize(kif, "__inline__");
        let (nodes, _)  = crate::parse::kif::parse(tokens, "__inline__");
        nodes.into_iter().next().unwrap()
    }

    #[test]
    fn no_row_vars_unchanged() {
        let node = kif("(P ?X ?Y)");
        let expanded = expand_row_vars(&node, &Parser::Kif);
        assert!(expanded.get(0).is_some());
        assert_eq!(expanded.get(0).unwrap().to_string(), node.to_string());
    }

    #[test]
    fn single_row_var_five_arities() {
        let node = kif("(P @ROW)");
        let expanded: Vec<String> = expand_row_vars(&node, &Parser::Kif).into_iter().map(|n| { n.to_string() }).collect();
        assert_eq!(expanded.len(), 5);
        assert_eq!(expanded[0], "( P ?ROW2 )");
        assert_eq!(expanded[1], "( P ?ROW2 ?ROW3 )");
        assert_eq!(expanded[2], "( P ?ROW2 ?ROW3 ?ROW4 )");
        assert_eq!(expanded[3], "( P ?ROW2 ?ROW3 ?ROW4 ?ROW5 )");
        assert_eq!(expanded[4], "( P ?ROW2 ?ROW3 ?ROW4 ?ROW5 ?ROW6 )");
    }

    #[test]
    fn row_var_in_quantifier_and_body() {
        // @ROW appears in both the quantifier list and the body -- both must expand.
        // One row variable -> MAX_ARITY expansions (j = 1 ... MAX_ARITY).
        let node = kif("(forall (@ROW) (P @ROW))");
        let expanded = expand_row_vars(&node, &Parser::Kif);
        assert_eq!(expanded.len(), MAX_ARITY);
        // Display format wraps each list as `( head args... )`.
        assert_eq!(expanded[0].to_string(), "( forall ( ?ROW2 ) ( P ?ROW2 ) )");
        assert_eq!(expanded[1].to_string(), "( forall ( ?ROW2 ?ROW3 ) ( P ?ROW2 ?ROW3 ) )");
    }

    #[test]
    fn two_row_vars_cross_product() {
        // Two row vars -> MAX_ARITY x MAX_ARITY expansions.
        // Expansion order: the first row var (@ROW) is expanded in the outer
        // loop, the second (@ARGS) in the inner loop.  So ARGS varies fastest:
        //   index 0: ROW=1, ARGS=1  ->  (P ?ROW2)       (Q ?ARGS2)
        //   index 1: ROW=1, ARGS=2  ->  (P ?ROW2)       (Q ?ARGS2 ?ARGS3)
        //   ...
        //   index 5: ROW=2, ARGS=1  ->  (P ?ROW2 ?ROW3) (Q ?ARGS2)
        let node = kif("(=> (P @ROW) (Q @ARGS))");
        let expanded = expand_row_vars(&node, &Parser::Kif);
        assert_eq!(expanded.len(), MAX_ARITY * MAX_ARITY);
        assert_eq!(expanded[0].to_string(), "( => ( P ?ROW2 ) ( Q ?ARGS2 ) )");
        assert_eq!(expanded[1].to_string(), "( => ( P ?ROW2 ) ( Q ?ARGS2 ?ARGS3 ) )");
        assert_eq!(expanded[MAX_ARITY].to_string(), "( => ( P ?ROW2 ?ROW3 ) ( Q ?ARGS2 ) )");
    }

    #[test]
    fn find_names_deduplicates() {
        let names = find_row_var_names("(P @ROW ?X @ROW)");
        assert_eq!(names, vec!["ROW"]);
    }

    #[test]
    fn contains_row_var_detects_nested() {
        let kif = "(=> (P @ROW) true)";
        let (tokens, _) = crate::parse::kif::tokenize(kif, "<test>");
        let (nodes, _)  = crate::parse::kif::parse(tokens, "<test>");
        assert!(nodes.iter().any(contains_row_var));
    }
}