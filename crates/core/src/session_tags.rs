//! Reserved session / file tags the KB uses for internal plumbing.
//!
//! Every string here is an ephemeral bucket name that tags sentences
//! while they're still "in flight" through ingest, query, or
//! reconcile.  Centralising the constants guarantees a typo in any
//! one call site can't silently create a phantom session that nothing
//! else finds.
//!
//! All tags share the leading-`__` convention so the reconcile-path
//! filter `file.starts_with("__")` (used in `AxiomSource::lookup`
//! and friends) excludes them from user-visible source attribution.

/// The ephemeral tag the SInE-select path parses the conjecture into
/// before running its BFS seed over the axiom set.  Cleared before
/// the prover is invoked.
pub const SESSION_SINE_QUERY:       &str = "__sine_query__";

/// Tag used by the subprocess-backed `ask` to re-parse the conjecture
/// for the native TPTP converter.  Scrubbed on both success and
/// failure paths.
pub const SESSION_QUERY:            &str = "__query__";

/// Tag used by `ask_embedded` to re-parse the conjecture for the
/// integrated-prover path.  Kept distinct from `__query__` so a
/// concurrent mix of subprocess + embedded asks on the same KB
/// doesn't collide (not currently possible, but cheap to defend
/// against).
pub const SESSION_QUERY_EMBEDDED:   &str = "__query_embedded__";

/// Session used by `reconcile_file` when feeding newly-added ASTs
/// through the normal ingest pipeline.  Promoted to axiom status
/// immediately via `make_session_axiomatic` before the reconcile
/// returns.
pub const SESSION_RECONCILE_ADD:    &str = "__reconcile_add__";

/// Session used by the native CLI's `open_or_build_kb_profiled` to
/// tag sentences from `-f` / `-d` files before promoting them in
/// bulk via `make_session_axiomatic` at the end of the load pass.
pub const SESSION_FILES:            &str = "__files__";

/// Session used by `build_kb_from_files` for in-memory assembly from
/// a `-f` / `-d` set when the LMDB path is being bypassed (no-db
/// mode).  The trailing promote converts these to axioms.
pub const SESSION_BASE:             &str = "__base__";

/// Session used by `sumo load` (both flush + reconcile paths) to
/// stage freshly-parsed sentences before committing them to LMDB.
pub const SESSION_LOAD:             &str = "__load__";
