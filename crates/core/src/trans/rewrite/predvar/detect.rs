use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::{atom_is_variables, PredVarSchema, PvGuard, SignatureKind};
use crate::parse::ast::OpKind;
use crate::semantics::consts::{
    ASSIGNMENT_FN, DOMAIN_SUBCLASS_RELATION, INSTANCE_RELATION, RANGE_SUB_REL_CLASS,
    SUBCLASS_RELATION, SUBRELATION_RELATION,
};
use crate::semantics::SemanticLayer;
use crate::syntactic::SyntacticLayer;
use crate::trans::rewrite::extract::var_appears_as_predicate;
use crate::trans::rewrite::subst::{collect_conjuncts, decompose_implication};
use crate::types::{Element, SentenceId, SymbolId};
use crate::Sentence;

impl PvGuard {
    /// match a sentence to a `PvGuard`. Returns `None` if the sentence
    /// does not match any guard's shape
    fn from_sent(syms: &SchemaSymbols, sent: Arc<Sentence>) -> Option<Self> {
        let head = match sent.elements.first() {
            Some(Element::Symbol(sym)) => sym.id(),
            _ => return None,
        };
        let args = &sent.elements[1..];
        if head == INSTANCE_RELATION.id() && args.len() == 2 {
            // Check for an Instance guard: `(instance ?X SYM)`
            if let (Some(Element::Variable { id: v, .. }), Some(Element::Symbol(c))) =
                (args.first(), args.get(1))
            {
                Some(PvGuard::Instance {
                    var: *v,
                    class: c.id(),
                })
            } else {
                None
            }
        } else if head == SUBRELATION_RELATION.id() && args.len() == 2 {
            if let (
                Some(Element::Variable { id: v1, .. }),
                Some(Element::Variable { id: v2, .. }),
            ) = (args.first(), args.get(1))
            {
                Some(PvGuard::Subrelation { v1: *v1, v2: *v2 })
            } else {
                None
            }
        } else if let Some(kind) = syms.signature_kind(head, args.len()) {
            // Only the all-variables form is a guard; a signature atom
            // with a constant argument is an ordinary body atom.
            let vars: Vec<SymbolId> = args
                .iter()
                .filter_map(|e| match e {
                    Element::Variable { id, .. } => Some(*id),
                    _ => None,
                })
                .collect();
            if vars.len() == args.len() {
                Some(PvGuard::Signature { kind, head, vars })
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Collect the variable symbols from the guard
    fn vars(&self) -> Vec<SymbolId> {
        match self {
            PvGuard::Instance { var, .. } => vec![*var],
            PvGuard::Subrelation { v1, v2 } => vec![*v1, *v2],
            PvGuard::Signature { vars, .. } => vars.clone(),
        }
    }
}

/// The symbol ids the schema detector recognises, resolved once per scan.
///
/// Taxonomy and signature roles come from `semantics::consts` and the
/// semantic layer's role table, so a shape-recognised (renamed) dialect is
/// handled the same way its caches handle it.  `equal` needs no entry: the
/// store keeps it as `OpKind::Equal`.
struct SchemaSymbols {
    /// `instance` / `subrelation`: the taxonomy guard heads.
    tax_ids: HashSet<SymbolId>,
    subclass: SymbolId,
    domain: SymbolId,
    domain_subclass: SymbolId,
    range: SymbolId,
    range_subclass: SymbolId,
    assignment_fn: SymbolId,
    /// Signature schemas are on unless `SIGMA_NO_SIGNATURE_RULES` is set --
    /// the off-switch for a KB whose declared signatures contradict its
    /// taxonomy (the derived type rules then expose that as inconsistency).
    signature_rules: bool,
}

impl SchemaSymbols {
    /// initialize the schema detector with the appropriate symbols from the
    /// provided `SemanticLayer`
    fn resolve(semantic: &SemanticLayer) -> Self {
        Self {
            tax_ids: [INSTANCE_RELATION.id(), SUBRELATION_RELATION.id()]
                .into_iter()
                .collect(),
            subclass: SUBCLASS_RELATION.id(),
            domain: semantic.domain_role(),
            domain_subclass: DOMAIN_SUBCLASS_RELATION.id(),
            range: semantic.range_role(),
            range_subclass: RANGE_SUB_REL_CLASS.id(),
            assignment_fn: ASSIGNMENT_FN.id(),
            signature_rules: std::env::var_os("SIGMA_NO_SIGNATURE_RULES").is_none(),
        }
    }

    /// Match a signature relation (e.g. `domain`, `range`, etc) based on its symbol
    /// and arity. A result of `None` indicates either an unknown signature relation
    /// or one with a bad arity
    fn signature_kind(&self, head: SymbolId, arity: usize) -> Option<SignatureKind> {
        if !self.signature_rules {
            return None;
        }
        match arity {
            3 if head == self.domain => Some(SignatureKind::Domain),
            3 if head == self.domain_subclass => Some(SignatureKind::DomainSubclass),
            2 if head == self.range => Some(SignatureKind::Range),
            2 if head == self.range_subclass => Some(SignatureKind::RangeSubclass),
            _ => None,
        }
    }

    fn signature_head(&self, kind: SignatureKind) -> SymbolId {
        match kind {
            SignatureKind::Domain => self.domain,
            SignatureKind::DomainSubclass => self.domain_subclass,
            SignatureKind::Range => self.range,
            SignatureKind::RangeSubclass => self.range_subclass,
        }
    }
}

/// Scan `implications` for predicate-variable schemas (see [`PredVarSchema`]). Implications
/// are basically sentences - all sentences have already been normalized to CAF (e.g. `(=> (and ...) (...))`)
pub(crate) fn detect_predvar_schemas(
    semantic: &SemanticLayer,
    implications: &[SentenceId],
) -> Vec<PredVarSchema> {
    // Initialize the detector symbol set
    let syntactic = &semantic.syntactic;
    let syms = SchemaSymbols::resolve(semantic);
    let mut out = Vec::new();

    // For each implication check if one of the antecedents matches a valid
    // schema precondition
    for &sid in implications {
        let Some((ant_sid, _con)) = decompose_implication(syntactic, sid) else {
            // Not an implication
            continue;
        };

        let mut guards: Vec<PvGuard> = Vec::new();
        let mut guard_vars: HashSet<SymbolId> = HashSet::new();
        // Decompose the conjunction in the antecedent. For each, match to a valid
        // schema guard
        for csid in collect_conjuncts(syntactic, ant_sid) {
            let Some(s) = syntactic.sentence(csid) else {
                continue;
            };
            // Attempt to extract schema guard from the antecedent
            if let Some(pv) = PvGuard::from_sent(&syms, s) {
                let vars = pv.vars();
                guards.push(pv);
                guard_vars.extend(vars);
            }
        }
        if guards.is_empty() {
            continue;
        }

        // At most one signature guard, binding distinct variables, and every
        // taxonomy guard on the same schema may only mention variables the
        // signature guard binds. That guard is the sole binding driver in
        // `find_predvar_bindings`; every other guard only filters its
        // candidates, so a schema outside this shape would never instantiate.
        let mut signatures = guards.iter().filter_map(|g| match g {
            PvGuard::Signature { kind, vars, .. } => Some((*kind, vars.as_slice())),
            _ => None,
        });
        let signature = signatures.next();
        if signatures.next().is_some() {
            continue;
        }
        if let Some((_, vars)) = signature {
            let distinct: HashSet<SymbolId> = vars.iter().copied().collect();
            if distinct.len() != vars.len() || !guard_vars.is_subset(&distinct) {
                continue;
            }
        }

        // Every variable in predicate or function-head position must be bound
        // by a guard, or the instantiated rule would not be first-order.
        // Quantifier-bound variables are excluded from the scan: a quantifier's
        // variable list structurally looks like an atom headed by its first
        // variable, and a bound variable can never be a predicate variable.
        let mut all_vars: HashMap<SymbolId, u32> = HashMap::new();
        syntactic.collect_vars(sid, &mut all_vars);
        let mut quantified: HashSet<SymbolId> = HashSet::new();
        syntactic.collect_bound_vars(sid, true, &mut quantified);
        let free_vars: Vec<SymbolId> = all_vars
            .keys()
            .copied()
            .filter(|v| !quantified.contains(v))
            .collect();
        let mut pred_vars: Vec<SymbolId> = free_vars
            .iter()
            .copied()
            .filter(|&v| var_appears_as_predicate(syntactic, sid, v))
            .collect();
        let function_vars: Vec<SymbolId> = free_vars
            .iter()
            .copied()
            .filter(|&v| var_appears_as_function(syntactic, sid, v, syms.assignment_fn))
            .collect();
        if pred_vars
            .iter()
            .chain(function_vars.iter())
            .any(|v| !guard_vars.contains(v))
        {
            continue;
        }
        pred_vars.sort_unstable();
        let mut bound_vars: Vec<SymbolId> = guard_vars.iter().copied().collect();
        bound_vars.sort_unstable();

        // Accepted shapes: one instance-guarded property schema, the
        // two-variable subrelation-propagation schema, a domain schema over
        // one predicate variable, or a range schema over one function variable.
        let has_subrel = guards
            .iter()
            .any(|g| matches!(g, PvGuard::Subrelation { .. }));
        let ok_shape = match signature {
            Some((SignatureKind::Domain | SignatureKind::DomainSubclass, _)) => {
                pred_vars.len() == 1 && function_vars.is_empty() && !has_subrel
            }
            Some((SignatureKind::Range | SignatureKind::RangeSubclass, _)) => {
                pred_vars.is_empty() && function_vars.len() == 1 && !has_subrel
            }
            None if has_subrel => pred_vars.len() == 2,
            None => pred_vars.len() == 1 && guard_vars.len() == 1,
        };
        if !ok_shape {
            continue;
        }

        let pv_set: HashSet<SymbolId> = pred_vars.iter().copied().collect();
        let purity = SchemaPurity {
            syms: &syms,
            signature,
            function_var: function_vars.first().copied(),
        };
        if !is_pure_predvar_schema(syntactic, sid, &pv_set, &purity) {
            continue;
        }

        let body_arity = if pred_vars.is_empty() {
            function_call_arity(syntactic, sid, syms.assignment_fn)
        } else {
            predvar_call_arity(syntactic, sid, &pv_set)
        };
        out.push(PredVarSchema {
            schema_sid: sid,
            pred_vars,
            bound_vars,
            guards,
            body_arity,
        });
    }
    out
}

/// The argument count at which a predicate variable is applied in the tree
/// rooted at `sid` - the first `(?REL args...)` or `(holds ?REL args...)` atom
/// found.  Row-var expansion stamps each schema variant at one fixed arity, so
/// the first occurrence is representative.
fn predvar_call_arity(
    syntactic: &SyntacticLayer,
    sid: SentenceId,
    pv_set: &HashSet<SymbolId>,
) -> Option<usize> {
    let sentence = syntactic.sentence(sid)?;
    // Direct application: (?REL a b ...).
    if matches!(sentence.elements.first(), Some(Element::Variable { id, .. }) if pv_set.contains(id))
    {
        return Some(sentence.elements.len().saturating_sub(1));
    }
    // holds-style: (holds ?REL a b ...).
    if let Some(holds_id) = syntactic.sym_id("holds") {
        if matches!(sentence.elements.first(), Some(Element::Symbol(sym)) if sym.id() == holds_id)
            && matches!(sentence.elements.get(1), Some(Element::Variable { id, .. }) if pv_set.contains(id))
        {
            return Some(sentence.elements.len().saturating_sub(2));
        }
    }
    sentence.elements.iter().find_map(|e| match e {
        Element::Sub(child) => predvar_call_arity(syntactic, *child, pv_set),
        _ => None,
    })
}

/// `true` iff `var` is applied as a function somewhere under `sid`, i.e. it is
/// the first argument of an `(AssignmentFn ?F args...)` term.
fn var_appears_as_function(
    syntactic: &SyntacticLayer,
    sid: SentenceId,
    var: SymbolId,
    assignment_fn: SymbolId,
) -> bool {
    let Some(sentence) = syntactic.sentence(sid) else {
        return false;
    };
    (matches!(sentence.elements.first(), Some(Element::Symbol(sym)) if sym.id() == assignment_fn)
        && matches!(sentence.elements.get(1), Some(Element::Variable { id, .. }) if *id == var))
        || sentence.elements.iter().any(|e| match e {
            Element::Sub(child) => var_appears_as_function(syntactic, *child, var, assignment_fn),
            _ => false,
        })
}

/// The argument count of the first `(AssignmentFn ?F args...)` term under `sid`
/// (the function schema's analogue of [`predvar_call_arity`]).
fn function_call_arity(
    syntactic: &SyntacticLayer,
    sid: SentenceId,
    assignment_fn: SymbolId,
) -> Option<usize> {
    let sentence = syntactic.sentence(sid)?;
    if matches!(sentence.elements.first(), Some(Element::Symbol(sym)) if sym.id() == assignment_fn)
        && matches!(sentence.elements.get(1), Some(Element::Variable { .. }))
    {
        return Some(sentence.elements.len().saturating_sub(2));
    }
    sentence.elements.iter().find_map(|e| match e {
        Element::Sub(child) => function_call_arity(syntactic, *child, assignment_fn),
        _ => None,
    })
}

/// What a schema's atoms may be, beyond predicate-variable applications.
struct SchemaPurity<'a> {
    syms: &'a SchemaSymbols,
    /// The schema's signature guard, if any: its exact atom is allowed, and
    /// so are `subclass` atoms (the `*Subclass` schemas conclude one).
    signature: Option<(SignatureKind, &'a [SymbolId])>,
    /// The range schemas' function variable: `(equal (AssignmentFn ?F ...) ?V)`
    /// is allowed for it.
    function_var: Option<SymbolId>,
}

/// `true` iff every atom under `sid` is a taxonomy guard, the schema's own
/// signature guard, a `subclass` conclusion of a signature schema, the range
/// schemas' assignment equality, or an application of a predicate variable in
/// `pv_set`, combined only with propositional connectives / `forall` (no
/// `exists`, no other symbols).
fn is_pure_predvar_schema(
    syntactic: &SyntacticLayer,
    sid: SentenceId,
    pv_set: &HashSet<SymbolId>,
    purity: &SchemaPurity<'_>,
) -> bool {
    let Some(s) = syntactic.sentence(sid) else {
        return false;
    };
    match s.elements.first() {
        // `forall` binds the tuple variables - recurse only into the body
        // (last Sub), not the bound-variable list (which structurally looks
        // like a predicate-var atom).
        Some(Element::Op(OpKind::ForAll)) => match s.elements.last() {
            Some(Element::Sub(body)) => is_pure_predvar_schema(syntactic, *body, pv_set, purity),
            _ => false,
        },
        Some(Element::Op(OpKind::Exists)) => false,
        // The range schemas' `(equal (AssignmentFn ?F ..) ?V)`; any other
        // equality is checked like the other connectives.
        Some(Element::Op(OpKind::Equal))
            if purity.function_var.is_some_and(|f| {
                is_assignment_equality(syntactic, &s.elements, f, purity.syms)
            }) =>
        {
            true
        }
        Some(Element::Op(_)) => s.elements.iter().all(|e| match e {
            Element::Sub(c) => is_pure_predvar_schema(syntactic, *c, pv_set, purity),
            _ => true,
        }),
        // Predicate-variable atom `(?REL ...)`.
        Some(Element::Variable { id, .. }) => pv_set.contains(id),
        Some(Element::Symbol(sym)) => {
            let id = sym.id();
            purity.syms.tax_ids.contains(&id)
                || (purity.signature.is_some() && id == purity.syms.subclass)
                || purity.signature.is_some_and(|(kind, vars)| {
                    id == purity.syms.signature_head(kind) && atom_is_variables(&s.elements, vars)
                })
        }
        _ => false,
    }
}

/// `true` iff `elements` is `(equal (AssignmentFn ?F args...) ?V)` (either
/// argument order) for the function variable `func`.
fn is_assignment_equality(
    syntactic: &SyntacticLayer,
    elements: &[Element],
    func: SymbolId,
    syms: &SchemaSymbols,
) -> bool {
    if elements.len() != 3 || !matches!(elements.first(), Some(Element::Op(OpKind::Equal))) {
        return false;
    }
    let is_application = |e: &Element| match e {
        Element::Sub(sid) => syntactic.sentence(*sid).is_some_and(|t| {
            matches!(t.elements.first(), Some(Element::Symbol(h)) if h.id() == syms.assignment_fn)
                && matches!(t.elements.get(1), Some(Element::Variable { id, .. }) if *id == func)
        }),
        _ => false,
    };
    let is_var = |e: &Element| matches!(e, Element::Variable { .. });
    (is_application(&elements[1]) && is_var(&elements[2]))
        || (is_var(&elements[1]) && is_application(&elements[2]))
}
