// crates/core/src/syntactic/caches/symbol.rs
//
// The symbol store, as a content-addressed `EagerMap`.
//
// `SymbolId` *is* `hash(name)` (see `intern`), so the keyed store maps
// `id → name` (for rendering and collision detection) while the forward
// direction (`name → id`) is a pure hash — no counter, no name→id map.  Sparse
// Skolem metadata (`id → arity`, only for Skolems) lives in the cache's `side`.
//
// The cache is *passive*: it reacts to no events.  The sentence build path
// interns into it directly — through a shared `&`, via the interior mutability
// of `EntryCache` — which is why `intern` takes `&self`, not `&mut self`.

use std::collections::HashSet;

use crate::SymbolId;
use crate::cache::{EagerMap, EagerMapBehavior, EntryCache};
use crate::syntactic::SyntacticLayer;
use crate::types::Symbol;

/// Behavior for the `syntactic::symbols` store.  `Value` is the interned name;
/// `Side` is the sparse Skolem-arity index (present iff the symbol is a Skolem).
#[derive(Debug, Default)]
pub(crate) struct SymbolCache;

impl EagerMapBehavior for SymbolCache {
    type Parent = SyntacticLayer;
    type Key    = SymbolId;
    type Value  = Symbol;
    type Side   = EntryCache<SymbolId, Option<usize>>;
    type SideSnapshot = std::collections::HashMap<SymbolId, Option<usize>>;

    const NAME: &'static str = "syntactic::symbols";
    // consumes / produces / react all default to inert: the store is written
    // imperatively by the sentence build path, not by reacting to events.
}

#[allow(dead_code)] // API exercised once the build path is wired through it
impl EagerMap<SymbolCache> {
    /// Intern a symbol name.  The id **is** `hash(name)` (content-addressed),
    /// so this is idempotent and lock-free in the forward direction.  Records
    /// the name (for rendering / collision detection) and panics on the
    /// astronomically rare 64-bit collision between two *distinct* names rather
    /// than silently conflating them.
    pub(super) fn intern(&self, sym: Symbol) -> SymbolId {
        let id = sym.id();
        match self.entries().get(&id) {
            Some(existing) if existing != sym =>
                panic!("SymbolId collision {id:#x}: {sym:?} vs {existing:?}"),
            Some(_) => {} // already interned, same name
            None => self.entries().update(id, sym),
        }
        id
    }

    /// Intern a Skolem symbol (CNF), recording its arity in the sparse side map.
    pub(crate) fn intern_skolem(&self, name: &str, arity: Option<usize>) -> SymbolId {
        let id = self.intern(Symbol::from(name));
        self.side().modify_entry(id, |a| *a = arity);
        id
    }

    /// The name of `id`, if interned — a cheap `Arc<str>` clone (refcount bump).
    fn sym_name(&self, id: SymbolId) -> Option<Symbol> {
        self.entries().get(&id)
    }

    /// The id for `name`, if it has been interned (`hash(name)` gated on the
    /// name actually being present, so callers keep their "unknown → None").
    fn sym_id(&self, name: &str) -> Option<SymbolId> {
        let id = Symbol::hash_name(name);
        self.has_symbol(id).then_some(id)
    }

    /// Whether `id` is a known symbol.
    fn has_symbol(&self, id: SymbolId) -> bool {
        self.entries().contains_key(&id)
    }

    /// Whether `id` is a CNF-generated Skolem symbol.
    fn is_skolem(&self, id: SymbolId) -> bool {
        self.side().contains_key(&id)
    }

    /// Arity of a Skolem function symbol; `None` for Skolem constants and for
    /// ordinary (non-Skolem) symbols.
    fn skolem_arity(&self, id: SymbolId) -> Option<usize> {
        self.side().get(&id).flatten()
    }

    /// Evict every symbol whose id is *not* in `referenced` (orphan pruning
    /// after a removal batch), dropping its name and any Skolem side entry.
    /// Returns the ids removed.  `referenced` comes from the sentence store
    /// (`EagerMap::<SentenceCache>::referenced_symbols`).
    pub(crate) fn retain_referenced(&self, referenced: &HashSet<SymbolId>) -> HashSet<SymbolId> {
        let mut removed = HashSet::new();
        self.entries().retain(|id, _| {
            let keep = referenced.contains(id);
            if !keep { removed.insert(*id); }
            keep
        });
        self.side().retain(|id, _| referenced.contains(id));
        removed
    }
}

impl SyntacticLayer {
    /// Whether `id` is a known symbol.
    pub(crate) fn has_symbol(&self, id: SymbolId) -> bool { self.symbols.has_symbol(id) }

    /// The id for `name`, if it has been interned.
    pub(crate) fn sym_id(&self, name: &str) -> Option<SymbolId> { self.symbols.sym_id(name) }

    /// The name of `id`, if interned.
    pub(crate) fn sym_name(&self, id: SymbolId) -> Option<Symbol> { self.symbols.sym_name(id) }

    /// Whether a given [`SymbolId`] is a CNF-generated Skolem symbol.
    pub(crate) fn is_skolem(&self, id: SymbolId) -> bool { self.symbols.is_skolem(id) }

    /// Arity of a Skolem function symbol; [`None`] for Skolem constants and for
    /// ordinary (non-Skolem) symbols.
    pub(crate) fn skolem_arity(&self, id: SymbolId) -> Option<usize> { self.symbols.skolem_arity(id) }
}