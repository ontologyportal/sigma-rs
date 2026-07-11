//! `semantic::inferred_class` cache: memoised type inference — the most specific
//! SUMO class(es) for a symbol.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::{Element, OpKind, Sentence, SentenceId, SymbolId};
use crate::cache::{CacheBehavior, EntryCache};
use crate::semantics::SemanticLayer;
use crate::semantics::consts::INSTANCE_RELATION;
use crate::semantics::types::{ClassInference, ClassScope, RelationDomain, RelationRange, Scope, Scoped, ScopedClass, TaxRelation};
use crate::syntactic::caches::session::session_id;

/// Cache behavior for `semantic::inferred_class`: the most specific SUMO
/// class(es) for a symbol.
#[derive(Debug, Default)]
pub(crate) struct InferredClass;

impl CacheBehavior for InferredClass {
    type Parent = SemanticLayer;
    type Key    = Scoped<SymbolId>;
    type Value  = ClassInference;
    type Side = ();
    type SideSnapshot = ();

    const NAME: &'static str = "semantic::inferred_class";

    fn generate(&self, parent: &SemanticLayer, &Scoped { scope, key: sym }: &Scoped<SymbolId>) -> ClassInference {
        compute_infer_class(parent, sym, scope)
    }

    fn on_cycle(&self, _parent: &SemanticLayer, _key: &Scoped<SymbolId>) -> ClassInference {
        ClassInference::Unknown
    }

    fn consumes(&self) -> &'static [crate::cache::events::EventKind] {
        use crate::cache::events::EventKind;
        &[EventKind::TaxonomyChanged, EventKind::DomainRangeChanged,
          EventKind::RootAdded, EventKind::RootRemoved,
          EventKind::SessionReferenced]
    }

    fn reads(&self) -> &'static [&'static str] {
        &["syntactic::sentences", "semantic::domain", "semantic::range"]
    }

    fn react(
        &self,
        parent:  &SemanticLayer,
        events:  &[&crate::cache::events::Event],
        store:   &EntryCache<Scoped<SymbolId>, ClassInference>,
        _side:   &Self::Side,
    ) -> Vec<crate::cache::events::Event> {
        use crate::cache::events::Event;

        // A `subclass` change ripples through `has_ancestor`/`collapse` for
        // unrelated symbols, and a domain/range declaration change affects every
        // argument of a relation, so clear wholesale on either.
        if events.iter().any(|e| matches!(e,
            Event::TaxonomyChanged { .. } | Event::DomainRangeChanged { .. }))
        {
            store.clear();
            return Vec::new();
        }

        let mut evict: HashSet<SymbolId> = HashSet::new();
        let mut evict_scopes: HashSet<Scope> = HashSet::new();
        for e in events {
            match e {
                Event::SymbolsRetracted { syms } => evict.extend(syms.iter().copied()),
                Event::RootAdded { sid } => evict.extend(
                    inference_affected(parent, added_root_bodies(parent, *sid).iter().map(|a| a.as_ref())),
                ),
                Event::RootRemoved { sentences, .. } =>
                    evict.extend(inference_affected(parent, sentences.iter())),
                Event::SessionReferenced { session, .. } =>
                    { evict_scopes.insert(Scope::Session(session_id(session))); }
                _ => {}
            }
        }
        if !evict.is_empty() {
            // An affected symbol may be memoised under several scopes; drop it
            // from every scope's entry.
            store.retain(|scoped, _| !evict.contains(&scoped.key));
        }
        if !evict_scopes.is_empty() {
            store.retain(|scoped, _| !evict_scopes.contains(&scoped.scope));
        }
        Vec::new()
    }
}

impl SemanticLayer {
    /// Memoised type inference: the most specific SUMO class(es) for `sym`, in
    /// an explicit [`Scope`].
    ///
    /// Combines taxonomy (`instance` edges) with pattern-based usage (instance
    /// atoms, relation `domain`s, equality identity, function `range`s) over
    /// `sym`'s whole equality component.  Works for both ground symbols and
    /// scope-qualified variables; the per-formula counterpart is
    /// [`Self::classify_formula_scoped`].  Reasons over `Base` evidence unioned
    /// with the session's transient assertions when `scope` is a session.
    pub(crate) fn infer_class_scoped(&self, sym: SymbolId, scope: Scope) -> ClassInference {
        self.inferred_class.get(self, Scoped { scope, key: sym })
    }

    /// Classify every symbol/variable occurring in root formula `root_sid` by
    /// walking its sentence tree — the contextual counterpart to
    /// [`Self::infer_class_scoped`].  Each entry is the collapsed most-specific
    /// class.  Ground (unconditional) evidence is preferred over evidence that
    /// only holds inside the formula's logical structure (a rule hypothesis, a
    /// quantifier body).
    ///
    /// This is how variable classes are recovered: a variable's class lives in
    /// the atoms of its binding formula — an explicit `(instance ?V C)` guard, or
    /// its argument position in a relation with a declared `domain`.
    ///
    /// The domain/range/taxonomy evidence is resolved in an explicit [`Scope`]
    /// — session-declared `(domain …)` / `(range …)` axioms then classify the
    /// formula's variables too.
    ///
    /// Returns a map from each symbol/variable to its [`ScopedClass`].
    pub(crate) fn classify_formula_scoped(
        &self,
        root_sid: SentenceId,
        scope:    Scope,
    ) -> HashMap<SymbolId, ScopedClass> {
        let mut candidates: HashMap<SymbolId, Vec<(SymbolId, ClassScope)>> = HashMap::new();
        collect_class_candidates(self, root_sid, root_sid, false, false, scope, &mut candidates);

        // Ground (non-variable) argument symbols also carry their global
        // taxonomy class, even when asserted in a different root than this formula.
        let var_ids: HashSet<SymbolId> =
            self.syntactic.sentence_vars(root_sid).into_iter().map(|(id, _)| id).collect();
        let ground: Vec<SymbolId> =
            candidates.keys().copied().filter(|k| !var_ids.contains(k)).collect();
        for g in ground {
            let extra = match self.infer_class_scoped(g, scope) {
                ClassInference::Single(c)   => vec![c],
                ClassInference::Multiple(c) => c,
                _ => vec![],
            };
            let slot = candidates.entry(g).or_default();
            for c in extra { slot.push((c, ClassScope::Global)); }
        }

        candidates
            .into_iter()
            .map(|(sym, cands)| {
                // Prefer ground (unconditional) evidence; fall back to local.
                let (globals, locals): (Vec<_>, Vec<_>) =
                    cands.into_iter().partition(|(_, s)| matches!(s, ClassScope::Global));
                let classes: Vec<SymbolId> = if !globals.is_empty() {
                    globals.into_iter().map(|(c, _)| c).collect()
                } else {
                    locals.into_iter().map(|(c, _)| c).collect()
                };
                let class = if classes.is_empty() {
                    ClassInference::Unknown
                } else {
                    collapse_classes(self, &classes, Scope::Base)
                };
                (sym, ScopedClass { class })
            })
            .collect()
    }
}

/// Recursively walk root formula `root_sid`'s sentence tree (descending through
/// `Element::Sub` links), collecting class candidates for every symbol/variable
/// at a classifiable argument position.  `local` is `false` at the unconditional
/// top and flips `true` once a logical operator is crossed — the `Global` vs
/// `Local(root_sid)` distinction.
fn collect_class_candidates(
    layer:    &SemanticLayer,
    root_sid: SentenceId,
    sid:      SentenceId,
    local:    bool,
    negated:  bool,
    scope:    Scope,
    out:      &mut HashMap<SymbolId, Vec<(SymbolId, ClassScope)>>,
) {
    let Some(sentence) = layer.syntactic.sentence(sid) else { return };
    match sentence.elements.first() {
        Some(Element::Op(op)) => {
            // `(equal L R)` is an atom and `(and …)` asserts each conjunct at the
            // current scope; every other connective descends as Local.  `not`
            // flips polarity, so classifications under an odd number of negations
            // are dropped.
            if matches!(op, OpKind::Equal) {
                classify_equality(layer, root_sid, &sentence, local, negated, scope, out);
            }
            let child_local   = local || !matches!(op, OpKind::And | OpKind::Equal);
            let child_negated = negated ^ matches!(op, OpKind::Not);
            for el in &sentence.elements[1..] {
                if let Element::Sub(child) = el {
                    collect_class_candidates(layer, root_sid, *child, child_local, child_negated, scope, out);
                }
            }
        }
        // A relation atom: (head arg1 arg2 …).
        Some(Element::Symbol(head)) => {
            let head_id = head.id();
            let class_scope = if local { ClassScope::Local(root_sid) } else { ClassScope::Global };

            // A negated atom says what the target is NOT, so skip it.
            if !negated {
                if head_id == INSTANCE_RELATION.id() {
                    // (instance TARGET Class)
                    if let (Some(t), Some(Element::Symbol(c))) =
                        (target_id(sentence.elements.get(1)), sentence.elements.get(2))
                    {
                        out.entry(t).or_default().push((c.id(), class_scope));
                    }
                } else {
                    // Argument-domain: TARGET at position N of a relation with a
                    // declared domain → domain(head)[N].
                    let domain = layer.domain_scoped(head_id, scope);
                    if !domain.is_empty() {
                        for (pos, el) in sentence.elements[1..].iter().enumerate() {
                            if let (Some(t), Some(cid)) =
                                (target_id(Some(el)), domain.get(pos).and_then(|rd| rd.id()))
                            {
                                out.entry(t).or_default().push((cid, class_scope));
                            }
                        }
                    }
                }
            }
            // Recurse into nested sub-sentence arguments (function terms, etc.).
            for el in &sentence.elements[1..] {
                if let Element::Sub(child) = el {
                    collect_class_candidates(layer, root_sid, *child, local, negated, scope, out);
                }
            }
        }
        _ => {}
    }
}

/// `(equal L R)`: when one side is a function application `(Fn …)`, the other
/// side (a symbol or variable) takes `Fn`'s declared *range* as its class.
fn classify_equality(
    layer:    &SemanticLayer,
    root_sid: SentenceId,
    sentence: &Sentence,
    local:    bool,
    negated:  bool,
    scope:    Scope,
    out:      &mut HashMap<SymbolId, Vec<(SymbolId, ClassScope)>>,
) {
    // `(not (equal A B))` asserts the two are different — no class is shared.
    if negated { return; }
    let class_scope = if local { ClassScope::Local(root_sid) } else { ClassScope::Global };
    let (l, r) = (sentence.elements.get(1), sentence.elements.get(2));

    // (a) function-application on one side → the other side takes the function's
    //     declared range, e.g. `(equal Joseph (FatherOfFn Jesus))`.
    for (lhs, rhs) in [(l, r), (r, l)] {
        if let (Some(target), Some(Element::Sub(fn_sid))) = (target_id(lhs), rhs) {
            let Some(fn_sent) = layer.syntactic.sentence(*fn_sid) else { continue };
            if let Some(Element::Symbol(fhead)) = fn_sent.elements.first() {
                if let Some(range_class) = layer.range_scoped(fhead.id(), scope).id() {
                    out.entry(target).or_default().push((range_class, class_scope));
                }
            }
        }
    }

    // (b) symbol/variable on BOTH sides → each side inherits the other's
    //     taxonomy class (via `infer_class`).  Equality-derived classifications
    //     don't chain.
    if let (Some(a), Some(b)) = (target_id(l), target_id(r)) {
        push_candidates(out, a, layer.infer_class_scoped(b, scope), class_scope);
        push_candidates(out, b, layer.infer_class_scoped(a, scope), class_scope);
    }

    // (c) number literal on one side → the other side is that number's SUMO
    //     class: `(equal Value 40.0)` classifies `Value` as RealNumber,
    //     `(equal Count 3)` as Integer, `(equal Ratio 1/3)` as RationalNumber.
    for (lhs, rhs) in [(l, r), (r, l)] {
        if let (Some(target), Some(Element::Literal(crate::types::Literal::Number(n)))) =
            (target_id(lhs), rhs)
        {
            let class_name = crate::trans::sort::numeric_literal_class(n);
            if let Some(cid) = layer.syntactic.sym_id(class_name) {
                out.entry(target).or_default().push((cid, class_scope));
            }
        }
    }
}

/// Push a [`ClassInference`]'s class ids as candidates for `target` (no-op for
/// `Unknown`/`Class`, so no empty entry is created).
fn push_candidates(
    out:    &mut HashMap<SymbolId, Vec<(SymbolId, ClassScope)>>,
    target: SymbolId,
    class:  ClassInference,
    scope:  ClassScope,
) {
    let ids: Vec<SymbolId> = match class {
        ClassInference::Single(c)   => vec![c],
        ClassInference::Multiple(c) => c,
        _ => return,
    };
    let slot = out.entry(target).or_default();
    for c in ids { slot.push((c, scope)); }
}

/// The classifiable id of an element: a ground symbol or a (non-row) variable.
fn target_id(el: Option<&Element>) -> Option<SymbolId> {
    match el {
        Some(Element::Symbol(s)) => Some(s.id()),
        Some(Element::Variable { id, is_row: false, .. }) => Some(*id),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Inference logic (uncached)
//
// Known limitations:
//   * Negation is shallow: only atoms directly under `(not …)` are dropped.
//     `(not (and …))` / `(not (or …))` are rewritten to NNF at ingest, so this
//     covers the boolean fragment.
//   * TODO(disjunction): `(or (instance ?X Int) (instance ?X Rat))` is collapsed
//     to most-specific (a meet) when a disjunction calls for the least common
//     ancestor (a join) — it should yield `Number`, not `Multiple`.  This needs
//     the or-branch structure (the walk), not the flat pattern scan.
// ---------------------------------------------------------------------------

/// Compute (uncached) the [`ClassInference`] for `sym`.
fn compute_infer_class(layer: &SemanticLayer, sym: SymbolId, scope: Scope) -> ClassInference {
    // Resolve sym's whole symbol↔symbol equality component first, then gather
    // direct evidence for every member.  This keeps the result order-independent.
    let component = equality_component(layer, sym, scope);

    let mut classes: Vec<SymbolId> = Vec::new();
    let mut class_signal = false;
    for &m in &component {
        let is_var = is_variable_id(layer, m);
        let (mc, sig) = collect_direct_classes(layer, m, is_var, scope);
        classes.extend(mc);
        class_signal |= sig;
        if !is_var {
            classes.extend(taxonomy_instance_classes(layer, m, scope));
        }
    }

    if !classes.is_empty() {
        return collapse_classes(layer, &classes, scope);
    }

    // No instance/usage class found → is it itself a class?
    if class_signal {
        return ClassInference::Class;
    }
    for &m in &component {
        if is_variable_id(layer, m) { continue; }
        let parents = layer.parents_of_scoped(m, scope);
        if !parents.is_empty() && parents.iter().all(|(_, rel)| *rel == TaxRelation::Subclass) {
            return ClassInference::Class;
        }
        match infer_from_taxonomy_parents(layer, &parents, scope) {
            ClassInference::Unknown => {}
            other                   => return other,
        }
    }
    ClassInference::Unknown
}

/// Root sentence ids that mention any symbol in `contains`, visible in `scope`
/// — the candidate set the pattern-based inference passes match against.  For
/// `Base` this is the axiom occurrences (O(k) via `axiom_index`).  For a session
/// it adds that session's own transient (un-promoted) roots.  A `None`
/// `contains` returns `None` (the matcher scans all roots), preserving the
/// no-narrowing semantics.
fn scoped_contain_roots(
    layer:    &SemanticLayer,
    contains: &Option<HashSet<SymbolId>>,
    scope:    Scope,
) -> Option<HashSet<SentenceId>> {
    let syms = contains.as_ref()?;
    let mut roots: HashSet<SentenceId> = HashSet::new();
    for s in syms {
        roots.extend(layer.syntactic.axiom_sentences_of(*s).iter().copied());
    }
    if let Scope::Session(sid) = scope {
        for r in layer.syntactic.sessions.session_sentences_by_id(sid) {
            let mut occ = layer.syntactic.sentence_symbols(r);
            occ.extend(layer.syntactic.sentence_vars(r).into_iter().map(|(id, _)| id));
            if syms.iter().any(|s| occ.contains(s)) {
                roots.insert(r);
            }
        }
        // A staged removal hides the formula from all reasoning in this session,
        // so drop its active tombstoned roots from the candidate set.
        for tsid in layer.syntactic.sessions.active_tombstones(sid) {
            roots.remove(&tsid);
        }
    }
    Some(roots)
}

/// Scope-aware [`find_by_pattern_sub`](crate::syntactic::pattern) — resolves the
/// `contains` candidates through [`scoped_contain_roots`] (Base ∪ session
/// overlay) and matches `pat` over them.  A `None` candidate set falls back to
/// the unfiltered scan, matching the un-scoped behaviour.
fn scoped_find_sub(
    layer:    &SemanticLayer,
    pat:      &crate::syntactic::pattern::SentencePattern,
    contains: &Option<HashSet<SymbolId>>,
    scope:    Scope,
) -> Vec<(SentenceId, crate::syntactic::pattern::Bindings)> {
    let pats = layer.syntactic.patterns();
    match scoped_contain_roots(layer, contains, scope) {
        Some(roots) => pats.find_by_pattern_sub_in_roots(pat, roots),
        None        => pats.find_by_pattern_sub(pat, None),
    }
}

/// Root + descendant bodies of a freshly-added root `sid`, fetched from the
/// store (the bodies are present — the root was just interned).
fn added_root_bodies(layer: &SemanticLayer, sid: SentenceId) -> Vec<Arc<Sentence>> {
    let mut out = Vec::new();
    if let Some(root) = layer.syntactic.sentence(sid) { out.push(root); }
    let subs = layer.syntactic.subs_of(sid).unwrap_or_default();
    for d in subs {
        if let Some(b) = layer.syntactic.sentence(d) { out.push(b); }
    }
    out
}

/// The symbols whose cached inference an added/removed root's `bodies` (root +
/// sub-sentences) can affect — the *targets* of the root's inference-input atoms:
///
///   * `(instance T C)`                         → `T`
///   * `(R … args …)` where `R` has a declared
///     `domain`/`range` (and is not a taxonomy head) → each arg
///   * `(equal L R)`                            → both sides, plus the
///                                                 equality closure (transitive)
///
/// Returns empty when the root contains no inference-input atom, so a
/// `documentation` / `format` / domainless-relation root leaves the cache
/// untouched.
fn inference_affected<'a>(
    layer:  &SemanticLayer,
    bodies: impl IntoIterator<Item = &'a Sentence>,
) -> HashSet<SymbolId> {
    let mut affected: HashSet<SymbolId> = HashSet::new();
    let mut eq_seeds: Vec<SymbolId> = Vec::new();

    for s in bodies {
        match s.elements.first() {
            Some(Element::Symbol(head)) => {
                let hid = head.id();
                if layer.tax_role_of(hid) == Some(TaxRelation::Instance) {
                    // (instance T C) → T's class.
                    if let Some(t) = target_id(s.elements.get(1)) { affected.insert(t); }
                } else if layer.tax_role_of(hid).is_none()
                    && (!layer.domain(hid).is_empty() || layer.range(hid).id().is_some())
                {
                    // (R … args …) with a declared domain/range → every argument.
                    for el in &s.elements[1..] {
                        if let Some(t) = target_id(Some(el)) { affected.insert(t); }
                    }
                }
            }
            Some(Element::Op(OpKind::Equal)) => {
                // (equal L R) → both sides; seed the equality closure below.
                for el in &s.elements[1..] {
                    if let Some(t) = target_id(Some(el)) {
                        affected.insert(t);
                        eq_seeds.push(t);
                    }
                }
            }
            _ => {}
        }
    }

    // Equality is transitive: an added/removed `(equal …)` shifts every member of
    // the component, not only the two named symbols.
    for seed in eq_seeds {
        affected.extend(equality_component(layer, seed, Scope::Base));
    }
    affected
}

/// The set of symbols/variables transitively equal to `sym` via non-negated
/// `(equal …)` atoms whose other side is itself a symbol or variable (a
/// function-application side is direct `range` evidence, not an identity edge).
/// Includes `sym`.
fn equality_component(layer: &SemanticLayer, sym: SymbolId, scope: Scope) -> HashSet<SymbolId> {
    let mut seen: HashSet<SymbolId> = HashSet::from([sym]);
    let mut stack = vec![sym];
    while let Some(s) = stack.pop() {
        for n in equal_neighbors(layer, s, scope) {
            if seen.insert(n) { stack.push(n); }
        }
    }
    seen
}

/// Symbols/variables directly equal to `s` via a non-negated `(equal s X)` /
/// `(equal X s)` where `X` is itself a symbol or (non-row) variable.
fn equal_neighbors(layer: &SemanticLayer, s: SymbolId, scope: Scope) -> Vec<SymbolId> {
    use crate::syntactic::pattern::{MatchKey, PatternElement, SentencePattern};
    let contains: Option<HashSet<SymbolId>> = Some(std::iter::once(s).collect());
    let negated = negated_subs(layer, &contains, scope);

    let eq_pat = SentencePattern(vec![
        PatternElement::Exact(MatchKey::Op(OpKind::Equal)),
        PatternElement::AnyElement(0),
        PatternElement::AnyElement(1),
    ]);
    let mut out = Vec::new();
    for (sid, b) in scoped_find_sub(layer, &eq_pat, &contains, scope) {
        if negated.contains(&sid) { continue; }
        let (Some(l), Some(r)) = (b.elements.get(&0), b.elements.get(&1)) else { continue };
        for (side, other) in [(l, r), (r, l)] {
            if target_id(Some(side)) != Some(s) { continue; }
            if let Some(o) = target_id(Some(other)) {
                if o != s { out.push(o); }              // skip reflexive `(equal s s)`
            }
        }
    }
    out
}

/// Sub-sentences directly under a `(not …)` among the `contains` candidates —
/// their positive classification is dropped (a negated atom says what the target
/// is *not*).  `(not (and …))` / `(not (or …))` are already NNF-normalized at
/// ingest, so by here every negation sits directly on an atom.
fn negated_subs(layer: &SemanticLayer, contains: &Option<HashSet<SymbolId>>, scope: Scope) -> HashSet<SentenceId> {
    use crate::syntactic::pattern::{MatchKey, PatternElement, SentencePattern};
    let not_pat = SentencePattern(vec![
        PatternElement::Exact(MatchKey::Op(OpKind::Not)),
        PatternElement::AnySubSentence(0),
    ]);
    scoped_find_sub(layer, &not_pat, contains, scope)
        .into_iter()
        .filter_map(|(_, b)| b.sub_sids.get(&0).copied())
        .collect()
}

/// Direct (non-equality-identity) class evidence for `sym`, via the pattern
/// engine over every sentence mentioning it (roots + sub-sentences):
///
///   * `(instance sym C)`               → `C`
///   * `(R … sym …)` at each arg N       → `domain(R)[N]`
///   * `(equal sym (Fn …))`              → `range(Fn)`
///
/// Symbol↔symbol equality is not followed here — that is resolved once by
/// [`equality_component`].  Atoms directly under `(not …)` are skipped.
/// Returns `(class_ids, class_signal)`, where `class_signal` flags a
/// `domainSubclass` / `rangeSubclass` position (evidence `sym` is itself a class).
fn collect_direct_classes(
    layer:  &SemanticLayer,
    sym:    SymbolId,
    is_var: bool,
    scope:  Scope,
) -> (Vec<SymbolId>, bool) {
    use crate::syntactic::pattern::{MatchKey, PatternElement, SentencePattern};

    let contains: Option<HashSet<SymbolId>> = Some(std::iter::once(sym).collect());
    let negated = negated_subs(layer, &contains, scope);

    let mut classes: Vec<SymbolId> = Vec::new();
    let mut class_signal = false;

    // (instance sym C)
    let inst_pat = SentencePattern(vec![
        PatternElement::Exact(MatchKey::Symbol((*INSTANCE_RELATION).clone())),
        PatternElement::AnyElement(0),
        PatternElement::AnyElement(1),
    ]);
    for (sid, b) in scoped_find_sub(layer, &inst_pat, &contains, scope) {
        if negated.contains(&sid) { continue; }
        if target_id(b.elements.get(&0)) != Some(sym) { continue; }
        if let Some(Element::Symbol(c)) = b.elements.get(&1) {
            classes.push(c.id());
        }
    }

    // (R … sym …) — read the domain at every argument position sym occupies,
    // since a relation may mention sym more than once (a reflexive `(R sym sym)`).
    let target_key = if is_var {
        MatchKey::Var(sym)
    } else {
        match layer.syntactic.sym_name(sym) {
            Some(s) => MatchKey::Symbol(s),
            None    => return (classes, class_signal), // unknown ground id
        }
    };
    let dom_pat = SentencePattern(vec![
        PatternElement::AnyCapture(0),               // head (forces sym to be a non-head arg)
        PatternElement::Glob,
        PatternElement::Exact(target_key.clone()),   // sym occurs as an argument
        PatternElement::Glob,
    ]);
    for (sid, _b) in scoped_find_sub(layer, &dom_pat, &contains, scope) {
        if negated.contains(&sid) { continue; }
        let Some(s) = layer.syntactic.sentence(sid) else { continue };
        let Some(head_id) = s.head_symbol() else { continue };  // None for operator heads
        if layer.tax_role_of(head_id).is_some() { continue; } // taxonomy heads handled elsewhere
        let domain = layer.domain_scoped(head_id, scope);
        for (i, el) in s.elements[1..].iter().enumerate() {
            if target_id(Some(el)) != Some(sym) { continue; }
            match domain.get(i) {
                Some(RelationDomain::Domain(c))         => classes.push(*c),
                Some(RelationDomain::DomainSubclass(_)) => class_signal = true,
                _ => {}
            }
        }
    }

    // (equal sym (Fn …)) → the function's declared range; and
    // (equal sym <number>) → the literal's numeric class (RealNumber / Integer /
    // RationalNumber).  Symbol↔symbol equality is handled by `equality_component`.
    let eq_pat = SentencePattern(vec![
        PatternElement::Exact(MatchKey::Op(OpKind::Equal)),
        PatternElement::AnyElement(0),
        PatternElement::AnyElement(1),
    ]);
    for (sid, b) in scoped_find_sub(layer, &eq_pat, &contains, scope) {
        if negated.contains(&sid) { continue; }
        let (Some(l), Some(r)) = (b.elements.get(&0), b.elements.get(&1)) else { continue };
        for (side, other) in [(l, r), (r, l)] {
            if target_id(Some(side)) != Some(sym) { continue; }
            match other {
                Element::Sub(fsid) => {
                    if let Some(fhead) =
                        layer.syntactic.sentence(*fsid).and_then(|s| s.head_symbol())
                    {
                        match layer.range_scoped(fhead, scope) {
                            RelationRange::Range(c)         => classes.push(c),
                            RelationRange::RangeSubclass(_) => class_signal = true,
                            RelationRange::Unknown          => {}
                        }
                    }
                }
                Element::Literal(crate::types::Literal::Number(n)) => {
                    let class_name = crate::trans::sort::numeric_literal_class(n);
                    if let Some(cid) = layer.syntactic.sym_id(class_name) {
                        classes.push(cid);
                    }
                }
                _ => {}
            }
        }
    }

    (classes, class_signal)
}


/// Whether `sym` is a scope-qualified variable id rather than a ground symbol.
/// Variables are interned under names of the form `name__<scope-number>` — a
/// suffix ground SUMO symbols never carry.
fn is_variable_id(layer: &SemanticLayer, sym: SymbolId) -> bool {
    layer.syntactic.sym_name(sym).is_some_and(|s| {
        s.name().rsplit_once("__")
            .is_some_and(|(_, scope)| !scope.is_empty() && scope.bytes().all(|b| b.is_ascii_digit()))
    })
}


/// Collect all C where `(instance sym C)` is a direct taxonomy edge visible in
/// `scope` — the `Base` axioms unioned with the session overlay (via
/// [`parents_of_scoped`](SemanticLayer::parents_of_scoped)).
fn taxonomy_instance_classes(layer: &SemanticLayer, sym: SymbolId, scope: Scope) -> Vec<SymbolId> {
    layer
        .parents_of_scoped(sym, scope)
        .into_iter()
        .filter_map(|(b, r)| match r {
            TaxRelation::Instance => Some(b),
            _ => None,
        })
        .collect()
}

/// Collapse a non-empty slice of candidate class ids into a [`ClassInference`].
///
/// Removes dominated classes (those with a more-specific sibling in the slice)
/// and returns `Single` when one leaf remains, else `Multiple`.
pub(crate) fn collapse_classes(layer: &SemanticLayer, classes: &[SymbolId], scope: Scope) -> ClassInference {
    debug_assert!(!classes.is_empty());
    if classes.len() == 1 {
        return ClassInference::Single(classes[0]);
    }

    let mut leaves: Vec<SymbolId> = Vec::new();
    'outer: for &c in classes {
        for &d in classes {
            if d != c && layer.has_ancestor_scoped(d, c, scope) {
                // d is a more-specific descendant of c → c is dominated; skip it.
                continue 'outer;
            }
        }
        // Dedup on insertion: `classes` may contain the same id more than once.
        if !leaves.contains(&c) {
            leaves.push(c);
        }
    }

    match leaves.len() {
        0 => ClassInference::Unknown,
        1 => ClassInference::Single(leaves[0]),
        _ => ClassInference::Multiple(leaves),
    }
}

/// Infer class from Subrelation / SubAttribute taxonomy parents: `sym` inherits
/// the class of a symbol it is a `subrelation`/`subAttribute` of.
fn infer_from_taxonomy_parents(
    layer: &SemanticLayer,
    parents: &[(SymbolId, TaxRelation)],
    scope: Scope,
) -> ClassInference {
    let mut class_ids: Vec<SymbolId> = Vec::new();
    for (parent_id, rel) in parents {
        if *rel == TaxRelation::Subrelation || *rel == TaxRelation::SubAttribute {
            if let ClassInference::Single(c) = layer.infer_class_scoped(*parent_id, scope) {
                class_ids.push(c);
            }
        }
    }
    if class_ids.is_empty() {
        ClassInference::Unknown
    } else {
        collapse_classes(layer, &class_ids, scope)
    }
}

impl SemanticLayer {
    /// The most specific class in `classes` — the deepest in the subclass chain.
    /// When candidates aren't totally ordered (siblings), the winner is whichever
    /// `reduce` retains.  Returns `None` for an empty slice.
    #[allow(dead_code)]
    pub(crate) fn most_specific_class(&self, classes: &[SymbolId]) -> Option<SymbolId> {
        classes.iter().copied().reduce(|a, b| {
            if self.has_ancestor(a, b) { a } else { b }
        })
    }
}


#[cfg(test)]
mod tests;
