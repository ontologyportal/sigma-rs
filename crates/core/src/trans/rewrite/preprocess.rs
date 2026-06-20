//! Type-hypothesis injection from `domain` axioms (preProcess stage).

use std::collections::{HashMap, HashSet};

use smallvec::smallvec;

use crate::parse::ast::OpKind;
use crate::semantics::SemanticLayer;
use crate::types::{Element, ElementVec, InternedSym, SentenceId, SymbolId};
use crate::syntactic::SyntacticLayer;
use super::augment::collect_conjuncts;

/// For each implication in `seed`, scan every use of free variables in
/// predicate/function argument positions, look up `domain(rel)[pos]` for
/// each use, pick the most-specific class across all uses of a variable,
/// and synthesise `(instance ?V C)` guards in the antecedent for variables
/// not already guarded by an equal-or-more-specific class.  Suppresses each
/// modified original.
///
/// Returns the freshly-allocated synthetic implication SIDs.
pub(crate) fn inject_domain_guards(
    semantic:   &SemanticLayer,
    seed:       &[SentenceId],
    suppressed: &mut HashSet<SentenceId>,
) -> Vec<SentenceId> {
    let instance_id = match semantic.syntactic.sym_id("instance") {
        Some(id) => id,
        None     => return Vec::new(),
    };

    // Phase 1 — plan injections under immutable access.
    struct Plan {
        sid:     SentenceId,
        ant_sid: SentenceId,
        con_sid: SentenceId,
        /// (var_id, class_id) pairs to synthesize as `(instance ?V C)` conjuncts.
        guards:  Vec<(SymbolId, SymbolId)>,
    }
    let mut plans: Vec<Plan> = Vec::new();

    for &sid in seed {
        if suppressed.contains(&sid) { continue; }

        let Some((ant_sid, con_sid)) = decompose_implication(&semantic.syntactic, sid) else {
            continue;
        };

        let mut var_uses: HashMap<SymbolId, Vec<SymbolId>> = HashMap::new();
        collect_var_uses(semantic, sid, &mut var_uses);

        let existing = collect_instance_guards(&semantic.syntactic, ant_sid, instance_id);

        let mut guards: Vec<(SymbolId, SymbolId)> = Vec::new();
        for (var_id, classes) in var_uses {
            let dedup: Vec<SymbolId> = {
                let mut seen = HashSet::new();
                classes.into_iter()
                    .filter(|c| *c != u64::MAX && seen.insert(*c))
                    .collect()
            };
            if dedup.is_empty() { continue; }

            let Some(class_id) = most_specific(semantic, &dedup) else { continue };

            // An existing `(instance ?V G)` guard subsumes our candidate if
            // class_id has G as an ancestor (candidate equally or more specific).
            if let Some(set) = existing.get(&var_id) {
                if set.iter().any(|g| *g == class_id || semantic.has_ancestor(class_id, *g)) {
                    continue;
                }
            }
            guards.push((var_id, class_id));
        }

        if !guards.is_empty() {
            // Sort for determinism.
            guards.sort_unstable_by_key(|&(v, c)| (v, c));
            plans.push(Plan { sid, ant_sid, con_sid, guards });
        }
    }

    // Phase 2 — apply plans with mutable access to the synthetic store.
    let mut new_sids: Vec<SentenceId> = Vec::new();
    let syntactic = &semantic.syntactic;
    for plan in plans {
        let guard_sids: Vec<SentenceId> = plan.guards.into_iter().map(|(var_id, class_id)| {
            let instance_sym = syntactic.sym_name(instance_id)
                .expect("`instance` symbol interned");
            let class_sym = syntactic.sym_name(class_id)
                .expect("guard class symbol interned");
            let elems: ElementVec = smallvec![
                Element::Symbol(InternedSym(instance_sym)),
                Element::Variable { id: var_id, name: String::new(), is_row: false, var_index: 0 },
                Element::Symbol(InternedSym(class_sym)),
            ];
            syntactic.push_synthetic_sentence(elems, plan.sid)
        }).collect();

        let existing_conjuncts = collect_conjuncts(syntactic, plan.ant_sid);

        // New antecedent: (and guard1 ... guardN existing_conjuncts...).
        let mut and_elems: ElementVec = ElementVec::with_capacity(
            1 + guard_sids.len() + existing_conjuncts.len(),
        );
        and_elems.push(Element::Op(OpKind::And));
        for &gs in &guard_sids {
            and_elems.push(Element::Sub(gs));
        }
        for &cs in &existing_conjuncts {
            and_elems.push(Element::Sub(cs));
        }
        let new_ant_sid = syntactic.push_synthetic_sentence(and_elems, plan.sid);

        // New implication: (=> new_ant con).
        let impl_elems: ElementVec = smallvec![
            Element::Op(OpKind::Implies),
            Element::Sub(new_ant_sid),
            Element::Sub(plan.con_sid),
        ];
        let new_impl_sid = syntactic.push_synthetic_sentence(impl_elems, plan.sid);

        // Suppress the original plus the intermediate fragments (augmented
        // antecedent + each guard): emitted standalone they are contradictory
        // bare conjunctions; they remain inlined via `new_impl_sid`.
        suppressed.insert(plan.sid);
        suppressed.insert(new_ant_sid);
        for gs in guard_sids {
            suppressed.insert(gs);
        }
        new_sids.push(new_impl_sid);
    }

    new_sids
}

/// Decompose `(=> Sub(ant) Sub(con))`.  Returns `None` for any other shape.
pub(super) fn decompose_implication(
    syntactic: &SyntacticLayer,
    sid:       SentenceId,
) -> Option<(SentenceId, SentenceId)> {
    let s = syntactic.sentence(sid)?;
    if !matches!(s.elements.first(), Some(Element::Op(OpKind::Implies))) {
        return None;
    }
    match (s.elements.get(1), s.elements.get(2)) {
        (Some(Element::Sub(a)), Some(Element::Sub(c))) => Some((*a, *c)),
        _ => None,
    }
}

/// Walk the sentence tree rooted at `sid`, recording every
/// `(var_id, domain(rel)[pos].id())` pair where a variable appears as an
/// argument to a Symbol-headed atom.  Quantifier scopes (`forall`/`exists`)
/// are not recursed into — bound vars there have distinct SymbolIds and
/// would not match any root-scope antecedent variable.
fn collect_var_uses(
    semantic: &SemanticLayer,
    sid:      SentenceId,
    out:      &mut HashMap<SymbolId, Vec<SymbolId>>,
) {
    let syntactic = &semantic.syntactic;
    let Some(sentence) = syntactic.sentence(sid) else { return };
    let first = sentence.elements.first();
    match first {
        Some(Element::Symbol(head_sym)) => {
            let head_id = head_sym.id();
            // Snapshot the domain so subsequent recursive calls don't reborrow.
            let domain = semantic.domain(head_id);
            for (i, elem) in sentence.elements[1..].iter().enumerate() {
                match elem {
                    Element::Variable { id, .. } => {
                        let class_id = domain.get(i).and_then(|d| d.id()).unwrap_or(u64::MAX);
                        if class_id != u64::MAX {
                            out.entry(*id).or_default().push(class_id);
                        }
                    }
                    Element::Sub(child) => {
                        collect_var_uses(semantic, *child, out);
                    }
                    _ => {}
                }
            }
        }
        Some(Element::Op(op)) => {
            // Stop at quantifier boundaries — those introduce new scopes
            // with their own SymbolIds.
            if matches!(op, OpKind::ForAll | OpKind::Exists) { return; }
            for elem in sentence.elements[1..].iter() {
                if let Element::Sub(child) = elem {
                    collect_var_uses(semantic, *child, out);
                }
            }
        }
        // Predicate-variable head `(?REL ?X ?Y)` or other shapes: nothing to do.
        _ => {}
    }
}

/// Collect every `(instance ?V C)` conjunct already present in `ant_sid`,
/// keyed by variable id.
fn collect_instance_guards(
    syntactic:   &SyntacticLayer,
    ant_sid:     SentenceId,
    instance_id: SymbolId,
) -> HashMap<SymbolId, HashSet<SymbolId>> {
    let mut out: HashMap<SymbolId, HashSet<SymbolId>> = HashMap::new();
    for csid in collect_conjuncts(syntactic, ant_sid) {
        let Some(s) = syntactic.sentence(csid) else { continue };
        if s.elements.len() != 3 { continue; }
        let head_ok = matches!(s.elements.first(),
            Some(Element::Symbol(sym)) if sym.id() == instance_id);
        if !head_ok { continue; }
        let var_id = match s.elements.get(1) {
            Some(Element::Variable { id, .. }) => *id,
            _ => continue,
        };
        let class_id = match s.elements.get(2) {
            Some(Element::Symbol(sym)) => sym.id(),
            _ => continue,
        };
        out.entry(var_id).or_default().insert(class_id);
    }
    out
}

/// Among `candidates`, find one `c` that is a descendant (via the taxonomy)
/// of every other candidate.  Returns `None` when no single winner exists
/// (cross-hierarchy ambiguity — caller should skip injection).
pub(super) fn most_specific(semantic: &SemanticLayer, candidates: &[SymbolId]) -> Option<SymbolId> {
    candidates.iter().copied().find(|&c| {
        candidates.iter().all(|&other| semantic.has_ancestor(c, other))
    })
}

