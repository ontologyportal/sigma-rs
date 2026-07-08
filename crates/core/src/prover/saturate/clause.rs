// crates/core/src/saturate/clause.rs
//
// Prover-local clause representation (plan D5).
//
// A clause is a flat list of signed literals; a literal is `(bool, AtomId)`
// where `AtomId = SentenceId` — the content hash of the atom's canonical
// `Sentence` form.  Atom bodies live in the prover-local [`AtomTable`]
// (a `DashMap`, NOT the shared sentence store: derived literals churn far
// too fast for the store's refcounted ingest path).  Content addressing
// gives atom-level dedup for free: the same canonical atom appearing in a
// thousand derived clauses is stored once.
//
// `Term` is the self-contained working tree used *during* clausification
// and inference — stored sentences reference subterms by id
// (`Element::Sub`), which is the wrong shape for substitution-heavy
// algorithms, so terms are lifted out of the store, transformed, and
// interned back into the `AtomTable` at canonicalization time.

use std::sync::Arc;

use dashmap::DashMap;
use smallvec::SmallVec;

use crate::parse::OpKind;
use crate::syntactic::SyntacticLayer;
use crate::types::{Element, ElementVec, InternedSym, Literal, Sentence, SentenceId, Symbol, SymbolId};

/// An atom's identity: the content hash of its canonical `Sentence` form.
/// Shares the `SentenceId` space deliberately — a ground atom that also
/// exists as a store sub-sentence hashes to the same id.
pub(crate) type AtomId = SentenceId;

/// Canonical clause identity: hash of the ordered `(polarity, AtomId)`
/// literal sequence after canonical variable renaming.  α-equivalent
/// clauses collapse to one key (the prover's dedup loop-guard).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ClauseKey(pub u64);

/// A signed literal: polarity + atom reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PLit {
    pub(crate) pos:  bool,
    pub(crate) atom: AtomId,
}

/// A canonicalized clause.  `lits` are in canonical order (negative
/// before positive, then structural); variables are renamed `V0..Vn`
/// in first occurrence over that order, so `key` is α-invariant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PClause {
    pub(crate) key:   ClauseKey,
    pub(crate) lits:  SmallVec<[PLit; 4]>,
    /// Number of distinct variables (canonical rename count) — the
    /// rename-apart offset basis for unification (next phase).
    pub(crate) nvars: u32,
}

impl PClause {
    pub(crate) fn is_unit(&self) -> bool { self.lits.len() == 1 }
    pub(crate) fn is_ground(&self) -> bool { self.nvars == 0 }
}

/// Self-contained term tree.  Mirrors `Sentence`/`Element` shape
/// (`App` = element list with the head at index 0) but holds subterms
/// inline instead of by store id, so substitution and renaming are
/// plain recursion.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Term {
    /// Logical variable, by (scope-qualified or synthetic) symbol id.
    Var(SymbolId),
    /// Ground symbol (shared `Arc<str>`).
    Sym(Symbol),
    /// String / numeric literal.
    Lit(Literal),
    /// Operator head — `OpKind::Equal` is the only operator that
    /// survives into atoms (connectives are consumed by clausification).
    Op(OpKind),
    /// Compound: `[head, arg, ...]`.
    App(Vec<Term>),
}

impl Term {
    /// `true` when the term contains no variables.
    pub(crate) fn is_ground(&self) -> bool {
        match self {
            Term::Var(_) => false,
            Term::App(elems) => elems.iter().all(Term::is_ground),
            _ => true,
        }
    }
}

/// Prover-local atom storage: `AtomId -> Sentence`, content-addressed.
///
/// Interior-mutable (`DashMap`) because atoms are interned from within
/// cache `generate` calls and (later) from the inference loop, both of
/// which hold only `&` access to the layer.  Inserts are idempotent —
/// the id IS the content — so concurrent duplicate interning is benign.
#[derive(Debug, Default)]
pub(crate) struct AtomTable {
    map: DashMap<AtomId, Arc<Sentence>, super::hash64::BuildContentHasher>,
}

impl AtomTable {
    /// Intern an atom (or nested function term) and every compound
    /// subterm, returning the atom's content-hash id.
    ///
    /// Non-`App` atoms (a bare propositional symbol) are wrapped into a
    /// single-element sentence, mirroring the prototype's `as_atom`.
    pub(crate) fn intern_atom(&self, t: &Term) -> AtomId {
        let elements: ElementVec = match t {
            Term::App(elems) => elems.iter().map(|e| self.element_of(e)).collect(),
            other => std::iter::once(self.element_of(other)).collect(),
        };
        let sent = Sentence { parent: Vec::new(), elements };
        let id = sent.hash();
        self.map.entry(id).or_insert_with(|| Arc::new(sent));
        id
    }

    /// One element of an atom's sentence body; compound subterms intern
    /// recursively and are referenced by id, exactly like store sentences.
    fn element_of(&self, t: &Term) -> Element {
        match t {
            Term::Var(id) => Element::Variable {
                id:        *id,
                // Canonical atoms carry canonical ids (`V0..Vn`, see
                // `canon`); the display name is reconstructed on demand.
                name:      format!("V{:x}", id),
                is_row:    false,
                var_index: 0,
            },
            Term::Sym(s)   => Element::Symbol(InternedSym(s.clone())),
            Term::Lit(l)   => Element::Literal(l.clone()),
            Term::Op(op)   => Element::Op(op.clone()),
            Term::App(_)   => Element::Sub(self.intern_atom(t)),
        }
    }

    /// Intern a SLOT-form atom (variables as dense slot ints, the
    /// working shape inference produces): slot `k` maps to the
    /// canonical variable id `canonical_var(k)`, so the produced
    /// sentence — and therefore the id — is byte-identical to interning
    /// the canonically renamed term.  The accept-time twin of the
    /// hash-only id `canonical_clause_hashed` computed at birth
    /// (debug-asserted equal at the `make` accept point).
    pub(crate) fn intern_slot_atom(&self, t: &Term) -> AtomId {
        let elements: ElementVec = match t {
            Term::App(elems) => elems.iter().map(|e| self.element_of_slot(e)).collect(),
            other => std::iter::once(self.element_of_slot(other)).collect(),
        };
        let sent = Sentence { parent: Vec::new(), elements };
        let id = sent.hash();
        self.map.entry(id).or_insert_with(|| Arc::new(sent));
        id
    }

    /// [`Self::element_of`] with the slot → canonical-variable-id
    /// translation (must stay byte-identical to it, `name` included —
    /// the stored sentences are the same ones the eager path built).
    fn element_of_slot(&self, t: &Term) -> Element {
        match t {
            Term::Var(slot) => {
                let id = super::canon::canonical_var_cached(*slot as usize);
                Element::Variable {
                    id,
                    name:      format!("V{:x}", id),
                    is_row:    false,
                    var_index: 0,
                }
            }
            Term::Sym(s)   => Element::Symbol(InternedSym(s.clone())),
            Term::Lit(l)   => Element::Literal(l.clone()),
            Term::Op(op)   => Element::Op(op.clone()),
            Term::App(_)   => Element::Sub(self.intern_slot_atom(t)),
        }
    }

    /// Resolve an atom or subterm id: prover-local table first, then the
    /// shared sentence store (ground atoms that already exist as store
    /// sub-sentences hash identically and need no duplicate copy here).
    /// Intern an already-built sentence (the detached-conjecture path:
    /// sub-sentences first, then the root — same id scheme as the
    /// store, so `resolve` finds them without any store round-trip).
    pub(crate) fn intern_sentence(&self, sent: Sentence) -> AtomId {
        let id = sent.hash();
        self.map.entry(id).or_insert_with(|| Arc::new(sent));
        id
    }

    pub(crate) fn resolve(&self, id: AtomId, syn: &SyntacticLayer) -> Option<Arc<Sentence>> {
        if let Some(s) = self.map.get(&id) {
            return Some(s.value().clone());
        }
        syn.sentence(id)
    }

    /// Lift a stored/interned sentence back into a self-contained [`Term`],
    /// resolving subterm references through `resolve`.  Inverse of
    /// [`Self::intern_atom`] (modulo variable display names).
    pub(crate) fn term_of(&self, id: AtomId, syn: &SyntacticLayer) -> Option<Term> {
        let sent = self.resolve(id, syn)?;
        let mut elems = Vec::with_capacity(sent.elements.len());
        for el in sent.elements.iter() {
            elems.push(match el {
                Element::Variable { id, .. } => Term::Var(*id),
                Element::Symbol(s)           => Term::Sym(s.0.clone()),
                Element::Literal(l)          => Term::Lit(l.clone()),
                Element::Op(op)              => Term::Op(op.clone()),
                Element::Sub(sid)            => self.term_of(*sid, syn)?,
            });
        }
        Some(Term::App(elems))
    }

    pub(crate) fn len(&self) -> usize { self.map.len() }
}

// -- hash-only content ids (hash-before-intern) --------------------------------
//
// The atom id IS the content hash of the canonical sentence form
// (`Sentence::hash` → `syntactic::sentence::content_hash`), so it is
// computable by driving the SAME `ElementHasher` byte scheme over the
// term tree — no `ElementVec` construction, no `Sentence` allocation,
// no `DashMap` probe.  `make` uses these for every consumer that needs
// only the ID (dedup keys, unit-table probes, equality-class keys);
// table insertion is deferred to clause acceptance.

use crate::syntactic::sentence::ElementHasher;

/// The id [`AtomTable::intern_atom`] would assign `t`, hash-only.
/// Variables hash by their RAW ids, exactly like `element_of` interns
/// them — use on pre-canonicalization terms (the ground probes) or any
/// term whose `Var` payload is already the id to store.
pub(crate) fn atom_content_id(t: &Term) -> AtomId {
    fn raw(v: SymbolId) -> SymbolId { v }
    match t {
        Term::App(elems) => hash_elements(elems, raw),
        other => {
            let mut h = ElementHasher::new(1);
            hash_element(other, &mut h, raw);
            h.finish()
        }
    }
}

/// The id [`AtomTable::intern_slot_atom`] would assign `t`, hash-only:
/// slot variables translate to canonical variable ids first.
pub(crate) fn slot_atom_content_id(t: &Term) -> AtomId {
    fn vmap(slot: SymbolId) -> SymbolId {
        crate::prover::saturate::canon::canonical_var_cached(slot as usize)
    }
    match t {
        Term::App(elems) => hash_elements(elems, vmap),
        other => {
            let mut h = ElementHasher::new(1);
            hash_element(other, &mut h, vmap);
            h.finish()
        }
    }
}

fn hash_elements(elems: &[Term], vmap: fn(SymbolId) -> SymbolId) -> u64 {
    let mut h = ElementHasher::new(elems.len());
    for e in elems {
        hash_element(e, &mut h, vmap);
    }
    h.finish()
}

fn hash_element(t: &Term, h: &mut ElementHasher, vmap: fn(SymbolId) -> SymbolId) {
    match t {
        Term::Var(v)   => h.variable(vmap(*v), false),
        Term::Sym(s)   => h.symbol(s.id()),
        Term::Lit(l)   => h.literal(l),
        Term::Op(op)   => h.op(op),
        Term::App(el)  => h.sub(hash_elements(el, vmap)),
    }
}
