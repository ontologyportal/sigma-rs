use std::collections::{HashMap, HashSet};

use super::{GuardArg, PredVarSchema, PvGuard, SignatureKind};
use crate::parse::ast::OpKind;
use crate::semantics::consts::{
    ASSIGNMENT_FN, DOMAIN_SUBCLASS_RELATION, INSTANCE_RELATION, RANGE_SUB_REL_CLASS,
    SUBRELATION_RELATION,
};
use crate::semantics::SemanticLayer;
use crate::syntactic::SyntacticLayer;
use crate::trans::rewrite::extract::var_appears_as_predicate;
use crate::trans::rewrite::subst::{collect_conjuncts, decompose_implication};
use crate::types::{Element, SentenceId, SymbolId};
use crate::Sentence;

impl PvGuard {
    /// Classify an antecedent atom as a guard over `ho_vars`. `None` when the
    /// atom mentions no higher-order variable as a direct argument, or is not
    /// flat (a compound argument makes it an ordinary body atom).
    fn from_sent(
        syms: &SchemaSymbols,
        sent: &Sentence,
        ho_vars: &HashSet<SymbolId>,
    ) -> Option<Self> {
        let Some(Element::Symbol(head)) = sent.elements.first() else {
            return None;
        };
        let head = head.id();
        let args = &sent.elements[1..];
        if !args
            .iter()
            .any(|e| matches!(e, Element::Variable { id, .. } if ho_vars.contains(id)))
        {
            return None;
        }
        match args {
            [Element::Variable { id: var, .. }, Element::Symbol(class)]
                if head == INSTANCE_RELATION.id() =>
            {
                return Some(PvGuard::Instance {
                    var: *var,
                    class: class.id(),
                });
            }
            [Element::Variable { id: v1, .. }, Element::Variable { id: v2, .. }]
                if head == SUBRELATION_RELATION.id() =>
            {
                return Some(PvGuard::Subrelation { v1: *v1, v2: *v2 });
            }
            _ => {}
        }
        if syms.is_signature_head(head) {
            if !syms.signature_rules {
                return None;
            }
            // The all-variables form binds from the signature caches; one
            // with a constant argument falls through to a fact guard.
            if let Some(kind) = syms.signature_kind(head, args.len()) {
                let vars: Vec<SymbolId> = args
                    .iter()
                    .filter_map(|e| match e {
                        Element::Variable { id, .. } => Some(*id),
                        _ => None,
                    })
                    .collect();
                if vars.len() == args.len() {
                    return Some(PvGuard::Signature { kind, vars });
                }
            }
        }
        let args: Option<Vec<GuardArg>> = args
            .iter()
            .map(|e| match e {
                Element::Variable { id, .. } => Some(GuardArg::Var(*id)),
                Element::Symbol(sym) => Some(GuardArg::Sym(sym.id())),
                Element::Literal(lit) => Some(GuardArg::Lit(lit.clone())),
                _ => None,
            })
            .collect();
        Some(PvGuard::Fact { head, args: args? })
    }

    /// The variables this guard binds or constrains.
    fn vars(&self) -> Vec<SymbolId> {
        match self {
            PvGuard::Instance { var, .. } => vec![*var],
            PvGuard::Subrelation { v1, v2 } => vec![*v1, *v2],
            PvGuard::Signature { vars, .. } => vars.clone(),
            PvGuard::Fact { args, .. } => args
                .iter()
                .filter_map(|arg| match arg {
                    GuardArg::Var(v) => Some(*v),
                    _ => None,
                })
                .collect(),
        }
    }
}

/// The symbol ids the schema detector recognises, resolved once per scan.
///
/// Signature roles come from `semantics::consts` and the semantic layer's
/// role table, so a shape-recognised (renamed) dialect is handled the same
/// way its caches handle it.
struct SchemaSymbols {
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
    /// Resolve the detector's symbols against `semantic`.
    fn resolve(semantic: &SemanticLayer) -> Self {
        Self {
            domain: semantic.domain_role(),
            domain_subclass: DOMAIN_SUBCLASS_RELATION.id(),
            range: semantic.range_role(),
            range_subclass: RANGE_SUB_REL_CLASS.id(),
            assignment_fn: ASSIGNMENT_FN.id(),
            signature_rules: std::env::var_os("SIGMA_NO_SIGNATURE_RULES").is_none(),
        }
    }

    fn is_signature_head(&self, head: SymbolId) -> bool {
        [
            self.domain,
            self.domain_subclass,
            self.range,
            self.range_subclass,
        ]
        .contains(&head)
    }

    /// Match a signature relation (e.g. `domain`, `range`, etc) based on its symbol
    /// and arity. A result of `None` indicates either an unknown signature relation
    /// or one with a bad arity
    fn signature_kind(&self, head: SymbolId, arity: usize) -> Option<SignatureKind> {
        match arity {
            3 if head == self.domain => Some(SignatureKind::Domain),
            3 if head == self.domain_subclass => Some(SignatureKind::DomainSubclass),
            2 if head == self.range => Some(SignatureKind::Range),
            2 if head == self.range_subclass => Some(SignatureKind::RangeSubclass),
            _ => None,
        }
    }
}

/// Scan `implications` for predicate-variable schemas (see [`PredVarSchema`]). Implications
/// are basically sentences - all sentences have already been normalized to CAF (e.g. `(=> (and ...) (...))`)
pub(crate) fn detect_predvar_schemas(
    semantic: &SemanticLayer,
    implications: &[SentenceId],
) -> Vec<PredVarSchema> {
    let syntactic = &semantic.syntactic;
    let syms = SchemaSymbols::resolve(semantic);
    let mut out = Vec::new();

    for &sid in implications {
        let Some((ant_sid, con_sid)) = decompose_implication(syntactic, sid) else {
            continue;
        };

        // Higher-order variables: free variables applied as a predicate or
        // function. Quantifier-bound variables are excluded from the scan: a
        // quantifier's variable list structurally looks like an atom headed by
        // its first variable, and a bound variable can never be bound by a
        // guard.
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
        let mut function_vars: Vec<SymbolId> = free_vars
            .iter()
            .copied()
            .filter(|&v| {
                !pred_vars.contains(&v)
                    && var_appears_as_function(syntactic, sid, v, syms.assignment_fn)
            })
            .collect();
        if pred_vars.is_empty() && function_vars.is_empty() {
            continue;
        }
        pred_vars.sort_unstable();
        function_vars.sort_unstable();
        let ho_vars: HashSet<SymbolId> = pred_vars.iter().chain(&function_vars).copied().collect();

        let mut guards = conjunct_guards(syntactic, &syms, ant_sid, &ho_vars);
        let mut inner = inner_implication(syntactic, con_sid);
        if let Some((inner_ant, _)) = inner {
            let inner_guards = conjunct_guards(syntactic, &syms, inner_ant, &ho_vars);
            if inner_guards.is_empty() {
                inner = None;
            }
            guards.extend(inner_guards);
        }
        let guard_sids: HashSet<SentenceId> = guards.iter().map(|(_, csid)| *csid).collect();
        let guards: Vec<PvGuard> = guards.into_iter().map(|(guard, _)| guard).collect();

        // Every higher-order variable must be bound by a guard, or the
        // instantiated rule would not be first-order. This is also sufficient
        // for every guard variable to get a value: a non-`instance` guard binds
        // all of its variables itself, and an `instance` guard is only formed
        // over a higher-order variable, which is enumerated from the problem
        // relations when no other guard binds it.
        let guard_vars: HashSet<SymbolId> = guards.iter().flat_map(PvGuard::vars).collect();
        if !ho_vars.is_subset(&guard_vars) {
            continue;
        }
        if !is_first_order_schema(syntactic, sid, &ho_vars) {
            continue;
        }

        let mut bound_vars: Vec<SymbolId> = guard_vars.into_iter().collect();
        bound_vars.sort_unstable();
        let call_arity: HashMap<SymbolId, usize> = pred_vars
            .iter()
            .filter_map(|&v| predvar_call_arity(syntactic, sid, v).map(|a| (v, a)))
            .chain(function_vars.iter().filter_map(|&v| {
                function_call_arity(syntactic, sid, v, syms.assignment_fn).map(|a| (v, a))
            }))
            .collect();
        out.push(PredVarSchema {
            schema_sid: sid,
            pred_vars,
            function_vars,
            bound_vars,
            guards,
            guard_sids,
            inner,
            call_arity,
        });
    }
    out
}

/// The guards among the conjuncts of `ant_sid`, with the conjunct each came
/// from.
fn conjunct_guards(
    syntactic: &SyntacticLayer,
    syms: &SchemaSymbols,
    ant_sid: SentenceId,
    ho_vars: &HashSet<SymbolId>,
) -> Vec<(PvGuard, SentenceId)> {
    collect_conjuncts(syntactic, ant_sid)
        .into_iter()
        .filter_map(|csid| {
            let s = syntactic.sentence(csid)?;
            PvGuard::from_sent(syms, &s, ho_vars).map(|guard| (guard, csid))
        })
        .collect()
}

/// The `(antecedent, consequent)` of a consequent shaped
/// `(forall (?V..) (=> A C))`, which lets `(=> G (forall (?V..) (=> A C)))` be
/// flattened to `(=> (and G A) C)`. Flattening cannot capture a variable: the
/// store interns each quantified variable under a scope-qualified id, so a
/// `?V` that shadows a same-named variable of `G` is already a distinct
/// variable.
fn inner_implication(
    syntactic: &SyntacticLayer,
    con_sid: SentenceId,
) -> Option<(SentenceId, SentenceId)> {
    let con = syntactic.sentence(con_sid)?;
    let [Element::Op(OpKind::ForAll), Element::Sub(_), Element::Sub(body)] =
        con.elements.as_slice()
    else {
        return None;
    };
    decompose_implication(syntactic, *body)
}

/// `true` iff substituting concrete symbols for `ho_vars` leaves a first-order
/// formula: every variable in head position under `sid` is one of `ho_vars`.
/// Quantifier variable lists are skipped (they structurally look like an atom
/// headed by their first variable).
fn is_first_order_schema(
    syntactic: &SyntacticLayer,
    sid: SentenceId,
    ho_vars: &HashSet<SymbolId>,
) -> bool {
    let Some(s) = syntactic.sentence(sid) else {
        return false;
    };
    let subs_ok = |elements: &[Element]| {
        elements.iter().all(|e| match e {
            Element::Sub(c) => is_first_order_schema(syntactic, *c, ho_vars),
            _ => true,
        })
    };
    match s.elements.first() {
        Some(Element::Op(OpKind::ForAll | OpKind::Exists)) => match s.elements.last() {
            Some(Element::Sub(body)) => is_first_order_schema(syntactic, *body, ho_vars),
            _ => false,
        },
        Some(Element::Variable { id, .. }) => ho_vars.contains(id) && subs_ok(&s.elements),
        Some(Element::Op(_) | Element::Symbol(_)) => subs_ok(&s.elements),
        _ => false,
    }
}

/// The argument count at which `var` is applied in the tree rooted at `sid` -
/// the first `(?REL args...)` or `(holds ?REL args...)` occurrence found.
/// Row-var expansion stamps each schema variant at one fixed arity, so the
/// first occurrence is representative.
fn predvar_call_arity(syntactic: &SyntacticLayer, sid: SentenceId, var: SymbolId) -> Option<usize> {
    let sentence = syntactic.sentence(sid)?;
    // Direct application: (?REL a b ...).
    if matches!(sentence.elements.first(), Some(Element::Variable { id, .. }) if *id == var) {
        return Some(sentence.elements.len().saturating_sub(1));
    }
    // holds-style: (holds ?REL a b ...).
    if let Some(holds_id) = syntactic.sym_id("holds") {
        if matches!(sentence.elements.first(), Some(Element::Symbol(sym)) if sym.id() == holds_id)
            && matches!(sentence.elements.get(1), Some(Element::Variable { id, .. }) if *id == var)
        {
            return Some(sentence.elements.len().saturating_sub(2));
        }
    }
    sentence.elements.iter().find_map(|e| match e {
        Element::Sub(child) => predvar_call_arity(syntactic, *child, var),
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
    function_call_arity(syntactic, sid, var, assignment_fn).is_some()
}

/// The argument count of the first `(AssignmentFn ?F args...)` term under
/// `sid` applying `var` (the function variables' analogue of
/// [`predvar_call_arity`]).
fn function_call_arity(
    syntactic: &SyntacticLayer,
    sid: SentenceId,
    var: SymbolId,
    assignment_fn: SymbolId,
) -> Option<usize> {
    let sentence = syntactic.sentence(sid)?;
    if matches!(sentence.elements.first(), Some(Element::Symbol(sym)) if sym.id() == assignment_fn)
        && matches!(sentence.elements.get(1), Some(Element::Variable { id, .. }) if *id == var)
    {
        return Some(sentence.elements.len().saturating_sub(2));
    }
    sentence.elements.iter().find_map(|e| match e {
        Element::Sub(child) => function_call_arity(syntactic, *child, var, assignment_fn),
        _ => None,
    })
}
