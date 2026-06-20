// crates/core/src/trans/caches/formulas_tff.rs
//
// `translation::formulas_tff` — per-root-sentence converted formula in TFF mode
// (typed predicates + gathered sort/fn/pred declarations).  Built by the native
// lowering engine ([`TranslationLayer::lower_root`], `trans/lower.rs`), which
// leans on the per-symbol caches (`sort_annotation`, `lazy_sort`,
// `numeric_sorts`).  Suppressed / higher-order / unconvertible sentences cache
// `None`.

use crate::cache::{CacheBehavior, EntryCache};
use crate::trans::TranslationLayer;
use crate::types::{CachedFormula, SentenceId};

/// Behavior for the `translation::formulas_tff` cache.
#[derive(Debug, Default)]
pub(crate) struct FormulasTff;

impl CacheBehavior for FormulasTff {
    type Parent = TranslationLayer;
    type Key    = SentenceId;
    type Value  = Option<CachedFormula>;
    type Side = ();
    type SideSnapshot = ();

    const NAME: &'static str = "translation::formulas_tff";

    fn generate(&self, parent: &TranslationLayer, &sid: &SentenceId) -> Option<CachedFormula> {
        // `lower_root` builds the TFF formula directly from the sentence body and
        // the typing caches.  Root-ness is enforced by callers (`build_problem`
        // only asks for selected root sids), so there is no per-call root scan.
        parent.lower_root(sid, /*typed*/ true)
    }

    fn consumes(&self) -> &'static [crate::cache::events::EventKind] {
        &[
            crate::cache::events::EventKind::TaxonomyChanged,
            crate::cache::events::EventKind::DomainRangeChanged,
            crate::cache::events::EventKind::PureAddition,
            crate::cache::events::EventKind::RootAdded,
            crate::cache::events::EventKind::RootRemoved,
        ]
    }

    // The lowering engine reads the sentence body, resolves term sorts via
    // `symbol_sort` (`lazy_sort`) and `numeric_sorts`, pulls relation arg/return
    // sorts from `sort_annotations`, and consults the semantic classifiers
    // (`arity`, `is_relation`, `is_function`).
    fn reads(&self) -> &'static [&'static str] {
        &["syntactic::sentences",
          "translation::lazy_sort", "translation::numeric_sorts", "translation::sort_annotations",
          "semantic::arity", "semantic::is_relation", "semantic::is_predicate", "semantic::is_function"]
    }

    fn react(
        &self,
        _parent: &TranslationLayer,
        events:  &[&crate::cache::events::Event],
        store:   &EntryCache<SentenceId, Option<CachedFormula>>,
        _side:   &Self::Side,
    ) -> Vec<crate::cache::events::Event> {
        use crate::cache::events::Event;
        // Structural invalidation first (see `formulas_fof`): taxonomy /
        // domain-range shifts can change any cached formula, so clear.
        let tax  = events.iter().any(|e| matches!(e, Event::TaxonomyChanged { .. }));
        let pure = events.iter().any(|e| matches!(e, Event::PureAddition));
        let dr   = events.iter().any(|e| matches!(e, Event::DomainRangeChanged { .. }));
        if (tax && !pure) || dr {
            store.clear();
        }
        // Removed roots evict.  Unlike the FOF cache, added roots are NOT
        // eagerly translated here: TFF typing is classification-heavy (per
        // ground symbol `infer_class` evidence scans), and the per-file
        // `TaxonomyChanged` clears during a multi-file load would force
        // re-deriving those classifications batch after batch — an O(files ×
        // roots) ingest blow-up (observed: minutes, load-order dependent).
        // The cache fills at first ask instead: `build_problem` prewarms all
        // selected sids in parallel, and the same react() invalidation keeps
        // entries correct thereafter.
        for ev in events {
            if let Event::RootRemoved { sid, .. } = ev {
                store.evict_keys(&[*sid]);
            }
        }
        Vec::new()
    }
}

impl TranslationLayer {
    /// The `CachedFormula` (TFF mode) for `sid`, converting and caching on miss.
    pub(crate) fn formula_tff(&self, sid: SentenceId) -> Option<CachedFormula> {
        self.formulas_tff.get(self, sid)
    }
}
