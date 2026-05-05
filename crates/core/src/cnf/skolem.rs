// crates/core/src/cnf/skolem.rs
//
// Skolemization specific functions for clausification

use crate::{SymbolId, syntactic::SyntacticLayer};
use super::TranslateCtx;

pub(super) fn intern_skolem(
    store: &mut SyntacticLayer,
    ctx:   &mut TranslateCtx,
    name:  &str,
    arity: usize,
) -> SymbolId {
    if let Some(&id) = ctx.name_to_id.get(name) {
        return id;
    }
    let id = store.intern_skolem(name, Some(arity));
    // Ensure the cached entry reflects the skolem interning (not a plain
    // intern that might otherwise have been done for a name collision).
    ctx.name_to_id.insert(name.to_string(), id);
    id
}

/// `true` for names that Vampire's NewCNF module uses for skolem
/// functors.  Matches both plain skolems (`sK0`, `sK1`) and the
/// disambiguated form (`sK0_7`) that some proof strategies emit.
pub(crate) fn is_skolem_name(name: &str) -> bool {
    name.starts_with("sK") || name.starts_with("sk_") // belt-and-braces
}