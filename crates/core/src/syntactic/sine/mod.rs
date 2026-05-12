// crates/core/src/syntactic/sine/mod.rs
//
// SInE (Sine Qua Non for Large Theory Reasoning) index, owned by SyntacticLayer.
//
// Given a large KB and a small conjecture, SInE selects a relevance-filtered
// subset of axioms to send to a theorem prover.  The algorithm, due to Hoder
// and Voronkov (CADE 2011), is a BFS over the D-relation: a link between
// symbols and the axioms for which they are the least-general (within a
// tolerance factor).
//
// Definitions
// -----------
// - Generality:  occ(s) = number of axioms in which symbol `s` appears.
// - D-relation:  s triggers axiom A iff
//                s ∈ syms(A)  AND  occ(s) ≤ t · min{occ(s') | s' ∈ syms(A)}
//   where t ≥ 1 is the tolerance factor.
// - Selection:   BFS from the conjecture's symbols, adding triggered axioms,
//                recursing on their symbols, until fixed point (or a depth cap).
//
// Submodules (split by responsibility):
//   params.rs — tuning knobs (`SineParams`, tolerance / budget defaults, scale_*)
//   index.rs  — the `SineIndex` data structure + incremental maintenance
//   select.rs — the selection BFS + auto-tolerance budget search
//   layer.rs  — the `SyntacticLayer` glue (symbol extraction + entry points)
//
// The engine (params / index / select) is store-agnostic — it operates on
// `SentenceId` / `SymbolId` only; `layer.rs` is the sole part coupled to the
// store.  `SyntacticLayer` owns a `sine: Eager<SineCache>` field and exposes the
// `sine_*` / `select_axioms*` wrapper methods.

mod params;
mod index;
mod select;
mod layer;

pub use params::*;
pub use index::SineIndex;

#[cfg(test)]
mod tests;
