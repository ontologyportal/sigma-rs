//! Reserved session / file tags used for internal ingest and query plumbing.
//!
//! Every string here is an ephemeral bucket name that tags sentences
//! while they're still "in flight" through ingest, query, or reconcile.
//!
//! All tags share the leading-`__` convention so a
//! `file.starts_with("__")` filter (as in the CLI's proof display)
//! excludes them from user-visible source attribution.

/// Ephemeral tag the SInE-select path parses the conjecture into
/// before running its BFS seed over the axiom set. Cleared before
/// the prover is invoked.
pub(crate) const SESSION_SINE_QUERY: &str = "__sine_query__";
