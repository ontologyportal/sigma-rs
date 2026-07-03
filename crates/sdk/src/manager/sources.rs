// crates/sdk/src/manager/sources.rs
//
// Resolve a KBManager's selected KB into an ingestible source list.

use crate::Source;

use super::KBManager;

impl KBManager {
    /// Resolve the selected KB's constituents into concrete [`Source`]s for
    /// ingestion.  `git = Some(uri)` re-roots the `Named` constituents to that
    /// repo (a wholesale swap); `None` resolves them locally against
    /// `base_dir`/`kb_dir`.  Pinned (absolute / `..`) constituents stay local
    /// either way.  See [`KB::resolve`](super::KB::resolve).
    pub fn resolve_sources(&self, git: Option<&str>) -> Vec<Source> {
        self.current_kb()
            .map(|kb| kb.resolve(&self.base_dir, &self.kb_dir, git))
            .unwrap_or_default()
    }

    /// Owned, ingestible sources for the selected KB, resolved locally (no git
    /// re-rooting).  Convenience for the common ingest path; pass `--git`
    /// explicitly through [`resolve_sources`](Self::resolve_sources) when a
    /// repo swap is wanted.
    pub fn current_sources_owned(&self) -> Vec<Source> {
        self.resolve_sources(None)
    }
}
