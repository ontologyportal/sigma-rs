// crates/core/src/trans/caches/ho_signatures.rs
//
// `translation::ho_signatures` — memoised THF (bi-sorted) signature per
// relation/function symbol, for the higher-order lowering (`lower_thf`).
//
// Phase-1 typing is deliberately coarse (the SUMO-in-THF "plain" scheme):
// every declared argument position is `$i` EXCEPT positions declared at the
// `Formula` class (or a subclass of it), which are `$o`; a function's range
// is `$i` unless Formula-classed (`$o`).  SUMO's class taxonomy otherwise
// stays as `instance` guards, exactly like the FOF encoding — so the only
// question this cache answers is "which positions of this symbol take a
// FORMULA rather than an individual".
//
// `KappaFn` is the one override: its KIF form `(KappaFn ?V φ)` binds a
// variable, so it types as `($i > $o) > $i` and the lowering builds the
// lambda syntactically.

use crate::SymbolId;
use crate::cache::{CacheBehavior, EagerMapBehavior, EntryCache};
use crate::cache::events::EventKind;
use crate::semantics::caches::domain::Domain;
use crate::semantics::caches::range::Range;
use crate::semantics::caches::tax_edges::TaxEdges;
use crate::semantics::types::Scope;
use crate::trans::ir::HoSort;
use crate::trans::TranslationLayer;

/// The SUMO class whose (sub)class-declared positions carry `$o`.
pub(crate) const FORMULA_CLASS: &str = "Formula";

/// The KappaFn relation name (typed `($i > $o) > $i`, lambda-binding).
pub(crate) const KAPPA_FN: &str = "KappaFn";

/// A symbol's THF signature: the declared argument sorts plus the return
/// sort for functions (`None` for predicates — they end in `$o`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoSignature {
    /// Per DECLARED argument position: `$o` for Formula-classed domains,
    /// `$i` otherwise.  Call sites with more arguments than declarations
    /// (variadic relations) extend with `$i`.
    pub args: Vec<HoSort>,
    /// `Some(ret)` for functions; `None` for predicates.
    pub ret: Option<HoSort>,
}

impl HoSignature {
    /// The arrow sort of the APPLIED constant at `call_arity` (extending
    /// declared positions with `$i` as needed): `a₁ > … > aₙ > ret`
    /// (`ret` = `$o` for predicates).
    pub fn arrow_sort(&self, call_arity: usize) -> HoSort {
        let mut args: Vec<HoSort> = self.args.iter().take(call_arity).cloned().collect();
        while args.len() < call_arity {
            args.push(HoSort::I);
        }
        HoSort::curry(&args, self.ret.clone().unwrap_or(HoSort::O))
    }
}

/// Behavior for the `translation::ho_signatures` cache (Base scope).
#[derive(Debug, Default)]
pub(crate) struct HoSignatures;

impl CacheBehavior for HoSignatures {
    type Parent = TranslationLayer;
    type Key    = SymbolId;
    type Value  = Option<HoSignature>;
    type Side   = ();
    type SideSnapshot = ();

    const NAME: &'static str = "translation::ho_signatures";

    fn generate(&self, parent: &TranslationLayer, &sym: &SymbolId) -> Option<HoSignature> {
        compute_ho_signature_scoped(parent, sym, Scope::Base)
    }

    fn consumes(&self) -> &'static [EventKind] {
        &[EventKind::TaxonomyChanged, EventKind::DomainRangeChanged]
    }

    fn reads(&self) -> &'static [&'static str] {
        &[Domain::NAME, Range::NAME, TaxEdges::NAME]
    }

    fn react(
        &self,
        _parent: &TranslationLayer,
        events:  &[&crate::cache::events::Event],
        store:   &EntryCache<SymbolId, Option<HoSignature>>,
        _side:   &Self::Side,
    ) -> Vec<crate::cache::events::Event> {
        use crate::cache::events::Event;
        // Wholesale clear, same reasoning as `symbol_sort`: the events name the
        // changed classes/relations, but a Formula-subclass edge change flips
        // the `$o`-ness of positions on relations the event does not name.
        let tax = events.iter().any(|e| matches!(e, Event::TaxonomyChanged { .. }));
        let dr  = events.iter().any(|e| matches!(e, Event::DomainRangeChanged { .. }));
        if tax || dr {
            store.clear();
        }
        Vec::new()
    }
}

/// Compute `sym`'s THF signature in `scope` — the shared body for the
/// (Base-scoped) cache and the uncached per-session path.  `None` for
/// symbols that are not relations in `scope` (they type as plain `$i`
/// constants).
pub(crate) fn compute_ho_signature_scoped(
    parent: &TranslationLayer,
    sym:    SymbolId,
    scope:  Scope,
) -> Option<HoSignature> {
    use crate::types::{RelationDomain, RelationRange};
    let syn = &parent.semantic.syntactic;

    // KappaFn override: `($i > $o) > $i`.
    if syn.sym_name(sym).is_some_and(|n| &*n.name() == KAPPA_FN) {
        return Some(HoSignature {
            args: vec![HoSort::Fn(Box::new(HoSort::I), Box::new(HoSort::O))],
            ret:  Some(HoSort::I),
        });
    }

    if !parent.semantic.is_relation_scoped(sym, scope) {
        return None;
    }

    let formula_id = syn.sym_id(FORMULA_CLASS);
    let is_formula_class = |cls: SymbolId| -> bool {
        match formula_id {
            Some(fid) => cls == fid || parent.semantic.has_ancestor(cls, fid),
            None => false,
        }
    };

    let args: Vec<HoSort> = parent
        .semantic
        .domain_scoped(sym, scope)
        .iter()
        .map(|d| match d {
            RelationDomain::Domain(cls) if is_formula_class(*cls) => HoSort::O,
            _ => HoSort::I,
        })
        .collect();

    let ret = if parent.semantic.is_function_scoped(sym, scope) {
        Some(match parent.semantic.range_scoped(sym, scope) {
            RelationRange::Range(cls) if is_formula_class(cls) => HoSort::O,
            _ => HoSort::I,
        })
    } else {
        None
    };

    Some(HoSignature { args, ret })
}

impl TranslationLayer {
    /// The THF signature for `sym` (Base memoised; session scopes compute
    /// directly — transient and small, like `sort_for_symbol_scoped`).
    pub(crate) fn ho_signature_scoped(
        &self,
        sym:   SymbolId,
        scope: Scope,
    ) -> Option<HoSignature> {
        match scope {
            Scope::Base => self.ho_signatures.get(self, sym),
            _ => compute_ho_signature_scoped(self, sym, scope),
        }
    }
}
