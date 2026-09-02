// crates/core/src/syntactic/caches/symbol.rs
//
// The symbol store, as a content-addressed `EagerMap`.
//
// `SymbolId` *is* `hash(name)` (see `intern`), so the keyed store maps
// `id -> name` (for rendering and collision detection) while the forward
// direction (`name -> id`) is a pure hash -- no counter, no name->id map.  Sparse
// Skolem metadata (`id -> arity`, only for Skolems) and a sorted-by-name
// prefix index both live in the cache's `side` (see [`SymbolSide`]).
//
// The cache is *passive*: it reacts to no events.  The sentence build path
// interns into it directly -- through a shared `&`, via the interior mutability
// of `EntryCache` -- which is why `intern` takes `&self`, not `&mut self`.

use std::collections::HashSet;
use std::sync::RwLock;

use crate::cache::{EagerMap, EagerMapBehavior, EntryCache};
use crate::syntactic::SyntacticLayer;
use crate::types::Symbol;
use crate::SymbolId;

/// Behavior for the `syntactic::symbols` store.  `Value` is the interned name;
/// `Side` is the sparse Skolem-arity index plus the sorted-name prefix index
/// (see [`SymbolSide`]).
#[derive(Debug, Default)]
pub(crate) struct SymbolCache;

impl EagerMapBehavior for SymbolCache {
    type Parent = SyntacticLayer;
    type Key = SymbolId;
    type Value = Symbol;
    type Side = SymbolSide;
    type SideSnapshot = std::collections::HashMap<SymbolId, Option<usize>>;

    const NAME: &'static str = "syntactic::symbols";
    // consumes / produces / react all default to inert: the store is written
    // imperatively by the sentence build path, not by reacting to events.
}

/// Side state for `syntactic::symbols`: the sparse Skolem-arity index
/// (`skolem_arity`) plus a lazily-rebuilt, sorted-by-lowercase-name index
/// (`by_name`) giving O(log n + matches) prefix lookup
/// ([`EagerMap::<SymbolCache>::symbols_with_prefix`]) instead of an O(n)
/// full-table scan.
#[derive(Debug, Default)]
pub(crate) struct SymbolSide {
    skolem_arity: EntryCache<SymbolId, Option<usize>>,
    by_name: RwLock<NameIndex>,
}

#[derive(Debug, Default)]
struct NameIndex {
    /// Sorted by `.0` (lowercase name). Skolem symbols are excluded from
    /// this index -- `kb::search` always filters them out of results anyway,
    /// so excluding them here shrinks the index and skips a redundant
    /// per-candidate check at query time.
    sorted: Vec<(String, SymbolId)>,
    dirty: bool,
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
            Some(existing) if existing != sym => {
                panic!("SymbolId collision {id:#x}: {sym:?} vs {existing:?}")
            }
            Some(_) => {} // already interned, same name
            None => {
                self.entries().update(id, sym);
                self.side().by_name.write().unwrap().dirty = true;
            }
        }
        id
    }

    /// Intern a Skolem symbol (CNF), recording its arity in the sparse side map.
    pub(crate) fn intern_skolem(&self, name: &str, arity: Option<usize>) -> SymbolId {
        let id = self.intern(Symbol::from(name));
        self.side().skolem_arity.modify_entry(id, |a| *a = arity);
        // The name index excludes Skolems; `intern` above may just have
        // rebuilt it (if this id is brand new) before this Skolem marking
        // ran, which would wrongly include it. Marking dirty again is cheap
        // (only reached on the rare Skolem-interning path, not every
        // `intern`) and guarantees the next rebuild excludes it correctly.
        self.side().by_name.write().unwrap().dirty = true;
        id
    }

    /// The name of `id`, if interned -- a cheap `Arc<str>` clone (refcount bump).
    fn sym_name(&self, id: SymbolId) -> Option<Symbol> {
        self.entries().get(&id)
    }

    /// The id for `name`, if it has been interned (`hash(name)` gated on the
    /// name actually being present, so callers keep their "unknown -> None").
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
        self.side().skolem_arity.contains_key(&id)
    }

    /// Arity of a Skolem function symbol; `None` for Skolem constants and for
    /// ordinary (non-Skolem) symbols.
    fn skolem_arity(&self, id: SymbolId) -> Option<usize> {
        self.side().skolem_arity.get(&id).flatten()
    }

    /// Evict every symbol whose id is *not* in `referenced` (orphan pruning
    /// after a removal batch), dropping its name and any Skolem side entry.
    /// Returns the ids removed.  `referenced` comes from the sentence store
    /// (`EagerMap::<SentenceCache>::referenced_symbols`).
    pub(crate) fn retain_referenced(&self, referenced: &HashSet<SymbolId>) -> HashSet<SymbolId> {
        let mut removed = HashSet::new();
        self.entries().retain(|id, _| {
            let keep = referenced.contains(id);
            if !keep {
                removed.insert(*id);
            }
            keep
        });
        self.side()
            .skolem_arity
            .retain(|id, _| referenced.contains(id));
        if !removed.is_empty() {
            self.side().by_name.write().unwrap().dirty = true;
        }
        removed
    }

    /// Every non-Skolem symbol whose name (case-insensitively) starts with
    /// `prefix_lc` (already lowercased), paired with its id, in ascending
    /// name order.  Rebuilds the sorted index first if it's stale (see
    /// [`SymbolSide`]'s doc comment); O(log n + matches) once built, O(n log
    /// n) on a stale rebuild.
    pub(crate) fn symbols_with_prefix(&self, prefix_lc: &str) -> Vec<(Symbol, SymbolId)> {
        let mut idx = self.side().by_name.write().unwrap();
        // `by_name` is a pure derived view of the id -> name map, so rather than
        // incrementally maintaining it on every `intern` (frequent -- once per
        // new symbol during parsing) it's rebuilt from scratch here on first
        // use after a mutation: one O(n log n) sort per stale window, not per
        // mutation, so a whole typing burst's worth of prefix queries shares
        // a single rebuild.
        if idx.dirty {
            let skolem = &self.side().skolem_arity;
            let mut sorted: Vec<(String, SymbolId)> = self
                .entries()
                .snapshot()
                .into_iter()
                .filter(|(id, _)| !skolem.contains_key(id))
                .map(|(id, sym)| (sym.name().to_ascii_lowercase(), id))
                .collect();
            sorted.sort_unstable();
            idx.sorted = sorted;
            idx.dirty = false;
        }
        let start = idx
            .sorted
            .partition_point(|(name, _)| name.as_str() < prefix_lc);
        idx.sorted[start..]
            .iter()
            .take_while(|(name, _)| name.starts_with(prefix_lc))
            .filter_map(|(_, id)| self.entries().get(id).map(|sym| (sym, *id)))
            .collect()
    }
}

impl SyntacticLayer {
    /// The id for `name`, if it has been interned.
    pub(crate) fn sym_id(&self, name: &str) -> Option<SymbolId> {
        self.symbols.sym_id(name)
    }

    /// The name of `id`, if interned.
    pub(crate) fn sym_name(&self, id: SymbolId) -> Option<Symbol> {
        self.symbols.sym_name(id)
    }

    /// Whether a given [`SymbolId`] is a CNF-generated Skolem symbol.
    pub(crate) fn is_skolem(&self, id: SymbolId) -> bool {
        self.symbols.is_skolem(id)
    }

    /// Non-Skolem symbol names starting with `prefix_lc` (already
    /// lowercased), paired with their id -- see
    /// [`EagerMap::<SymbolCache>::symbols_with_prefix`].
    pub(crate) fn symbols_with_prefix(&self, prefix_lc: &str) -> Vec<(Symbol, SymbolId)> {
        self.symbols.symbols_with_prefix(prefix_lc)
    }
}
