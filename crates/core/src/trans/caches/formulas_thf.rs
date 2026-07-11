// crates/core/src/trans/caches/formulas_thf.rs
//
// `translation::formulas_thf` — per-root-sentence THF lowering (higher-order,
// bi-sorted).  Built by [`TranslationLayer::lower_root_thf`]
// (`trans/lower_thf.rs`), which reads the `ho_signatures` cache.  Sentences
// that cannot lower cache a structured [`ThfDrop`] reason — auditable, never
// a silent absence.

use crate::cache::{CacheBehavior, EntryCache};
use crate::trans::lower_thf::ThfEntry;
use crate::trans::TranslationLayer;
use crate::types::SentenceId;

/// Behavior for the `translation::formulas_thf` cache.
#[derive(Debug, Default)]
pub(crate) struct FormulasThf;

impl CacheBehavior for FormulasThf {
    type Parent = TranslationLayer;
    type Key    = SentenceId;
    type Value  = ThfEntry;
    type Side = ();
    type SideSnapshot = ();

    const NAME: &'static str = "translation::formulas_thf";

    fn generate(&self, parent: &TranslationLayer, &sid: &SentenceId) -> ThfEntry {
        parent.lower_root_thf(sid)
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

    // The THF lowering reads the sentence body and the bi-sorted signatures
    // (which in turn read domain/range + taxonomy).
    fn reads(&self) -> &'static [&'static str] {
        &["syntactic::sentences", "translation::ho_signatures",
          "semantic::arity", "semantic::is_relation", "semantic::is_function"]
    }

    fn react(
        &self,
        _parent: &TranslationLayer,
        events:  &[&crate::cache::events::Event],
        store:   &EntryCache<SentenceId, ThfEntry>,
        _side:   &Self::Side,
    ) -> Vec<crate::cache::events::Event> {
        use crate::cache::events::Event;
        // Same invalidation shape as `formulas_tff`: taxonomy / domain-range
        // shifts can flip a `$o` position on any relation (there is no
        // reverse index to target the affected sentences), so clear; removed
        // roots evict; added roots fill LAZILY at the first ask via the
        // assembly prewarm — THF lowering is cheap (no classification), but
        // per-file `TaxonomyChanged` clears during a multi-file load would
        // still discard eager work batch after batch.
        let tax  = events.iter().any(|e| matches!(e, Event::TaxonomyChanged { .. }));
        let pure = events.iter().any(|e| matches!(e, Event::PureAddition));
        let dr   = events.iter().any(|e| matches!(e, Event::DomainRangeChanged { .. }));
        if (tax && !pure) || dr {
            store.clear();
        }
        for ev in events {
            if let Event::RootRemoved { sid, .. } = ev {
                store.evict_keys(&[*sid]);
            }
        }
        Vec::new()
    }
}

impl TranslationLayer {
    /// The [`ThfEntry`] for `sid`, lowering and caching on miss.
    #[cfg(feature = "ask")]
    pub(crate) fn formula_thf(&self, sid: SentenceId) -> ThfEntry {
        self.formulas_thf.get(self, sid)
    }
}
