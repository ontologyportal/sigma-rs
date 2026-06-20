// crates/core/src/trans/caches/formulas_fof.rs
//
// `translation::formulas_fof` — per-sentence converted formula in FOF mode
// (`hide_numbers = true`).  The `*_decls` fields are always empty (FOF emits no
// type declarations).  Suppressed sentences cache `None`.

use crate::cache::{CacheBehavior, EntryCache};
use crate::trans::TranslationLayer;
use crate::types::{CachedFormula, SentenceId};

/// Behavior for the `translation::formulas_fof` cache.
#[derive(Debug, Default)]
pub(crate) struct FormulasFof;

impl CacheBehavior for FormulasFof {
    type Parent = TranslationLayer;
    type Key    = SentenceId;
    type Value  = Option<CachedFormula>;
    type Side = ();
    type SideSnapshot = ();

    const NAME: &'static str = "translation::formulas_fof";

    fn generate(&self, parent: &TranslationLayer, &sid: &SentenceId) -> Option<CachedFormula> {
        // FOF mode: untyped predicates, no declarations, numbers hidden behind
        // opaque `$i` constants.  Same engine as TFF with `typed = false`.
        parent.lower_root(sid, /*typed*/ false)
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

    // Same converter as `formulas_tff` (FOF mode): reads the sentence body, term
    // sorts (`symbol_sort` / `numeric_sorts`), relation sorts
    // (`sort_annotations`), and the semantic classifiers.
    fn reads(&self) -> &'static [&'static str] {
        &["syntactic::sentences",
          "translation::lazy_sort", "translation::numeric_sorts", "translation::sort_annotations",
          "semantic::arity", "semantic::is_relation", "semantic::is_predicate", "semantic::is_function"]
    }

    fn react(
        &self,
        parent:  &TranslationLayer,
        events:  &[&crate::cache::events::Event],
        store:   &EntryCache<SentenceId, Option<CachedFormula>>,
        _side:   &Self::Side,
    ) -> Vec<crate::cache::events::Event> {
        use crate::cache::events::Event;
        // Structural invalidation first: a taxonomy or domain/range shift can
        // change any cached formula (classification / sorts), so clear.
        let tax  = events.iter().any(|e| matches!(e, Event::TaxonomyChanged { .. }));
        let pure = events.iter().any(|e| matches!(e, Event::PureAddition));
        let dr   = events.iter().any(|e| matches!(e, Event::DomainRangeChanged { .. }));
        if (tax && !pure) || dr {
            store.clear();
        }
        // Reactive eager translation: every ingested root — regardless of its
        // source (file load, tell, testcase ingest) — is translated as part of
        // the cascade that interned it.  Runs after the clear so this batch's
        // own roots survive; entries cleared above refill lazily on next read.
        // Removed roots evict (closing the per-sentence retraction leak).
        for ev in events {
            match ev {
                Event::RootAdded { sid } => {
                    store.update(*sid, self.generate(parent, sid));
                }
                Event::RootRemoved { sid, .. } => {
                    store.evict_keys(&[*sid]);
                }
                _ => {}
            }
        }
        Vec::new()
    }
}

impl TranslationLayer {
    /// The `CachedFormula` (FOF mode) for `sid`, converting and caching on miss.
    pub(crate) fn formula_fof(&self, sid: SentenceId) -> Option<CachedFormula> {
        self.formulas_fof.get(self, sid)
    }
}
