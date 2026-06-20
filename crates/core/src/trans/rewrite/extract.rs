//! Rewrite-rule extraction: numeric-subclass (Case 1) and predicate-variable
//! (Case 2) characterizations.

use crate::parse::ast::OpKind;
use crate::types::{Element, SentenceId, SymbolId};
use crate::syntactic::SyntacticLayer;
use crate::syntactic::pattern::{MatchKey, PatternElement, SentencePattern};

// ---------------------------------------------------------------------------
// RewriteRule
// ---------------------------------------------------------------------------

/// A rewrite rule: when an antecedent conjunct matches `pattern`, substitute
/// `template_var` → the captured variable in `consequent_sid` and add the
/// result as new conjuncts.
#[derive(Debug, Clone)]
pub(crate) struct RewriteRule {
    /// Unique index, used in the `(SentenceId, rule_idx)` applied-pair set to
    /// track which rules have already fired.
    pub id:             usize,
    /// Pattern matched against each antecedent sub-sentence conjunct.
    pub pattern:        SentencePattern,
    /// Full consequent from the template implication.  At augmentation time
    /// every occurrence of `template_var` is substituted with the variable
    /// captured from the matched antecedent conjunct.
    pub consequent_sid: SentenceId,
    /// The variable in the template (`?X` in `(instance ?X C) => CONSEQUENT`)
    /// that gets substituted with capture slot 0 during augmentation.
    pub template_var:   SymbolId,
    /// The source implication sentence that this rule was extracted from.
    /// Added to `TranslationLayer::suppressed` so it is not emitted to TPTP.
    pub source_sid:     SentenceId,
}

// ---------------------------------------------------------------------------
// Stage 2 — Rule Extraction (Case 1: numeric subclass characterizations)
// ---------------------------------------------------------------------------

/// Extract `RewriteRule`s from `implications` for Case 1 (numeric subclasses).
///
/// Looks for implications of the form:
///   `(=> (instance ?X C) Consequent)`
/// where `C` is a numeric subclass (present in `numeric_sorts`).
///
/// The rule carries the full `Consequent` sentence as-is.  At augmentation
/// time `substitute_var` replaces every occurrence of `?X` with the variable
/// captured from the matched antecedent.
pub(crate) fn extract_case1_rules(
    numeric_sorts: &crate::cache::EagerMap<crate::trans::caches::numeric_sorts::NumericSorts>,
    syntactic:     &SyntacticLayer,
    implications:  &[SentenceId],
) -> Vec<RewriteRule> {
    let mut rules: Vec<RewriteRule> = Vec::new();

    let instance_id = match syntactic.sym_id("instance") {
        Some(id) => id,
        None     => return rules,
    };

    for &sid in implications {
        let Some(sentence) = syntactic.sentence(sid) else { continue };

        // Must be (=> Sub(ant) Sub(con)).
        if !matches!(sentence.elements.first(), Some(Element::Op(OpKind::Implies))) {
            continue;
        }
        let (ant_sid, con_sid) = match (sentence.elements.get(1), sentence.elements.get(2)) {
            (Some(Element::Sub(a)), Some(Element::Sub(c))) => (*a, *c),
            _ => continue,
        };

        // Antecedent must be exactly (instance Variable(?X) Symbol(C)).
        let Some(ant_s) = syntactic.sentence(ant_sid) else { continue };
        if ant_s.elements.len() != 3 { continue; }

        if !matches!(ant_s.elements.first(),
            Some(Element::Symbol(sym)) if sym.id() == instance_id)
        { continue; }

        let var_id = match ant_s.elements.get(1) {
            Some(Element::Variable { id, .. }) => *id,
            _ => continue,
        };
        let class_id = match ant_s.elements.get(2) {
            Some(Element::Symbol(sym)) => sym.id(),
            _ => continue,
        };

        // Class must be a known numeric subclass.
        if numeric_sorts.get(&class_id).is_none() { continue; }

        // Only characterization sentences (a consequent with an arithmetic
        // atom) become rules; ordinary usage sentences like
        // `(=> (instance ?X C) (Pred ?X))` are augmented by the rules instead.
        if !has_arithmetic_content(syntactic, con_sid) { continue; }

        rules.push(RewriteRule {
            id:             rules.len(),
            pattern:        SentencePattern(vec![
                PatternElement::Exact(MatchKey::Symbol(
                    syntactic.sym_name(instance_id).expect("`instance` interned"))),
                PatternElement::AnyCapture(0),
                PatternElement::Exact(MatchKey::Symbol(
                    syntactic.sym_name(class_id).expect("guard class interned"))),
            ]),
            consequent_sid: con_sid,
            template_var:   var_id,
            source_sid:     sid,
        });
    }
    rules
}

/// Returns `true` if `sid` contains at least one recognizable arithmetic atom
/// anywhere in its sub-tree.
///
/// The recognized predicates are `greaterThan`, `greaterThanOrEqualTo`,
/// `lessThan`, `lessThanOrEqualTo`, and `equal`.
fn has_arithmetic_content(syntactic: &SyntacticLayer, sid: SentenceId) -> bool {
    let Some(sentence) = syntactic.sentence(sid) else { return false };

    match sentence.elements.first() {
        // (and …) — true if any conjunct has arithmetic content.
        Some(Element::Op(OpKind::And)) => {
            sentence.elements[1..].iter().any(|e| {
                if let Element::Sub(child_sid) = e {
                    has_arithmetic_content(syntactic, *child_sid)
                } else {
                    false
                }
            })
        }
        // Known arithmetic comparison predicates.
        Some(Element::Symbol(sym)) => matches!(
            &*sym.name(),
            "greaterThan" | "greaterThanOrEqualTo" | "lessThan" | "lessThanOrEqualTo"
        ),
        // (equal …) covers both direct equality and (equal (FnName …) literal) forms.
        Some(Element::Op(OpKind::Equal)) => true,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Stage 2 — Rule Extraction (Case 2: predicate variable characterizations)
// ---------------------------------------------------------------------------

/// Returns `true` if `var_id` appears as the **head** (predicate/functor
/// position, i.e. the first element) of any sub-sentence in the tree rooted
/// at `sid`, or as the first argument of the `holds` meta-predicate
/// (`(holds ?REL arg1 arg2)`).
pub(super) fn var_appears_as_predicate(syntactic: &SyntacticLayer, sid: SentenceId, var_id: SymbolId) -> bool {
    let Some(sentence) = syntactic.sentence(sid) else { return false };

    // Direct predicate position: (?REL arg1 arg2) — variable is the head.
    if matches!(sentence.elements.first(), Some(Element::Variable { id, .. }) if *id == var_id) {
        return true;
    }

    // holds-style: (holds ?REL arg1 arg2) — variable is first argument of the
    // `holds` meta-predicate (SUMO SUO-KIF higher-order application form).
    if let Some(holds_id) = syntactic.sym_id("holds") {
        if matches!(sentence.elements.first(), Some(Element::Symbol(sym)) if sym.id() == holds_id)
            && matches!(sentence.elements.get(1), Some(Element::Variable { id, .. }) if *id == var_id)
        {
            return true;
        }
    }

    sentence.elements.iter().any(|e| {
        if let Element::Sub(child_sid) = e {
            var_appears_as_predicate(syntactic, *child_sid, var_id)
        } else {
            false
        }
    })
}

/// Extract `RewriteRule`s from `implications` for Case 2 (predicate variables).
///
/// Looks for implications of the form:
///   `(=> (instance ?REL C) Consequent)`
/// where:
///   - `C` is **not** a numeric class (those are handled by Case 1), and
///   - `?REL` appears in predicate/head position somewhere in `Consequent`
///     (direct head `(?REL …)` or `holds`-style `(holds ?REL …)`).
///
/// These are relation/predicate class characterizations such as symmetry or
/// transitivity axioms.  The full `Consequent` is stored on the rule and
/// substituted at augmentation time, so no predicate filtering is needed here.
pub(crate) fn extract_case2_rules(
    numeric_sorts: &crate::cache::EagerMap<crate::trans::caches::numeric_sorts::NumericSorts>,
    syntactic:     &SyntacticLayer,
    implications:  &[SentenceId],
) -> Vec<RewriteRule> {
    let mut rules: Vec<RewriteRule> = Vec::new();

    let instance_id = match syntactic.sym_id("instance") {
        Some(id) => id,
        None     => return rules,
    };

    for &sid in implications {
        let Some(sentence) = syntactic.sentence(sid) else { continue };

        // Must be (=> Sub(ant) Sub(con)).
        if !matches!(sentence.elements.first(), Some(Element::Op(OpKind::Implies))) {
            continue;
        }
        let (ant_sid, con_sid) = match (sentence.elements.get(1), sentence.elements.get(2)) {
            (Some(Element::Sub(a)), Some(Element::Sub(c))) => (*a, *c),
            _ => continue,
        };

        // Antecedent must be exactly (instance Variable(?REL) Symbol(C)).
        let Some(ant_s) = syntactic.sentence(ant_sid) else { continue };
        if ant_s.elements.len() != 3 { continue; }

        if !matches!(ant_s.elements.first(),
            Some(Element::Symbol(sym)) if sym.id() == instance_id)
        { continue; }

        let var_id = match ant_s.elements.get(1) {
            Some(Element::Variable { id, .. }) => *id,
            _ => continue,
        };
        let class_id = match ant_s.elements.get(2) {
            Some(Element::Symbol(sym)) => sym.id(),
            _ => continue,
        };

        // Case 2: class must NOT be a numeric sort (those are Case 1).
        if numeric_sorts.get(&class_id).is_some() { continue; }

        // The variable must appear in predicate/head position in the consequent.
        // This distinguishes characterization sentences (which define relational
        // properties via the variable-as-predicate) from ordinary usage sentences
        // where the variable only appears in argument position.
        if !var_appears_as_predicate(syntactic, con_sid, var_id) { continue; }

        rules.push(RewriteRule {
            id:             rules.len(),
            pattern:        SentencePattern(vec![
                PatternElement::Exact(MatchKey::Symbol(
                    syntactic.sym_name(instance_id).expect("`instance` interned"))),
                PatternElement::AnyCapture(0),
                PatternElement::Exact(MatchKey::Symbol(
                    syntactic.sym_name(class_id).expect("guard class interned"))),
            ]),
            consequent_sid: con_sid,
            template_var:   var_id,
            source_sid:     sid,
        });
    }
    rules
}

