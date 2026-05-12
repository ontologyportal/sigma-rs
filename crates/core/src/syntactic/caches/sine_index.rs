// crates/core/src/syntactic/caches/sine_index.rs
//
// `syntactic::sine_index` — the eagerly-maintained SInE (Sine Qua Non) axiom
// selection index.
//
// The index itself (`SineIndex`) and its mechanics — generality queries,
// `add_axiom` / `remove_axiom` / `rebuild_from` — live in the `sine` subsystem.
// This file holds the cache behavior, which is *event-driven*: the index tracks
// exactly the promoted axioms.
//
//   * `AxiomsPromoted` — a session's sentences became axioms → `add_axiom` each
//     (symbols derived from the store, which still holds the bodies).
//   * `RootRemoved`    — an axiom was retracted → `remove_axiom` (idempotent;
//     a no-op for sentences that were never axioms).

use crate::cache::{EagerBehavior, EagerIndex};
use crate::cache::events::{Event, EventKind};
use crate::syntactic::SyntacticLayer;
use crate::syntactic::sine::SineIndex;

/// Behavior for the `syntactic::sine_index` eager index.
#[derive(Debug, Default)]
pub(crate) struct SineCache;

impl EagerBehavior for SineCache {
    type Parent = SyntacticLayer;
    type Value  = SineIndex;

    const NAME: &'static str = "syntactic::sine_index";

    fn initial(&self) -> SineIndex {
        SineIndex::default()
    }

    fn consumes(&self) -> &'static [EventKind] {
        &[EventKind::AxiomsPromoted, EventKind::RootRemoved]
    }

    // `react` derives each promoted sid's symbols via `parent.sentence_symbols`,
    // which reads the sentence store; the sentence reactor must run first.
    fn reads(&self) -> &'static [&'static str] {
        &["syntactic::sentences"]
    }

    fn react(
        &self,
        parent: &SyntacticLayer,
        events: &[&Event],
        store:  &EagerIndex<SineIndex>,
    ) -> Vec<Event> {
        store.update_with(|idx| {
            for e in events {
                match e {
                    Event::AxiomsPromoted { sids } => {
                        // Same batch heuristic as the imperative
                        // `sine_add_axioms`: per-axiom incremental g_min
                        // updates for small edits (~9 s of a full-SUMO load
                        // was once 50k `add_axiom` threshold recomputes),
                        // bulk handling for large promotions.  A large batch
                        // is DEFERRED, not rebuilt here: rebuilding per
                        // promotion made a 49-file load pay 48 cumulative
                        // whole-index rebuilds (~7 s).  The deferred pairs
                        // fold in via `flush_pending` at the first read
                        // (`SyntacticLayer::sine_current`) — one rebuild per
                        // load, and symbol sets are captured now while the
                        // bodies are guaranteed present.
                        if sids.len() >= idx.bulk_threshold() {
                            idx.defer_axioms(
                                sids.iter().map(|&sid| (sid, parent.sentence_symbols(sid))),
                            );
                        } else {
                            for sid in sids {
                                idx.add_axiom(*sid, parent.sentence_symbols(*sid));
                            }
                        }
                    }
                    Event::RootRemoved { sid, .. } => idx.remove_axiom(*sid),
                    _ => {}
                }
            }
        });
        Vec::new()
    }
}
