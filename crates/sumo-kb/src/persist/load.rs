// crates/sumo-kb/src/persist/load.rs
//
// Reconstruct a `KifStore` from the LMDB-backed persistent store.
//
// Ported from sumo-store/src/load.rs with key differences:
//   - Stable IDs: no Vec-position-as-ID trick; use `sent_idx` / `sym_idx` maps.
//   - `seed_counters()` is called after loading to set next IDs above DB max.
//   - Sub-sentences are stored inline in `StoredElement::Sub`; we allocate them
//     recursively into `KifStore` using `alloc_sentence`.

use std::collections::HashMap;

use crate::error::{KbError, Span};
use crate::kif_store::KifStore;
use crate::types::{Element, SentenceId, Sentence, Symbol, TaxEdge, TaxRelation};
use super::env::{LmdbEnv, StoredElement, StoredFormula};

// Public entry point

/// Load all formulas from `env` into a fresh `KifStore`.
///
/// Returns:
/// - The reconstructed `KifStore` (with `seed_counters` already called).
/// - A `HashMap<SentenceId, Option<String>>` mapping every loaded formula to
///   its session tag (`None` = axiom, `Some(s)` = session assertion).
pub(crate) fn load_from_db(
    env: &LmdbEnv,
) -> Result<(KifStore, HashMap<SentenceId, Option<String>>), KbError> {
    let txn = env.read_txn()?;

    // Load symbols
    let stored_syms = env.all_symbols(&txn)?;
    log::info!(target: "sumo_kb::persist",
        "load_from_db: loading {} symbols", stored_syms.len());

    let max_sym_id = stored_syms.iter().map(|s| s.id).max().unwrap_or(0);

    let mut symbols:     HashMap<String, SentenceId> = HashMap::new();
    let mut symbol_data: Vec<Symbol>                  = Vec::new();
    // We pre-size symbol_data to hold all symbols by ID position temporarily.
    // Then we build sym_idx from (stable_id → vec_pos).
    // However, to avoid huge allocations for sparse IDs, we insert in order.
    let mut sym_id_to_vec: HashMap<u64, usize> = HashMap::new();

    for sym in &stored_syms {
        symbols.insert(sym.name.clone(), sym.id);
        let vec_pos = symbol_data.len();
        sym_id_to_vec.insert(sym.id, vec_pos);
        symbol_data.push(Symbol {
            name:           sym.name.clone(),
            head_sentences: Vec::new(),
            all_sentences:  Vec::new(),
            is_skolem:      sym.is_skolem,
            skolem_arity:   sym.skolem_arity,
        });
    }

    // ── Load formulas ─────────────────────────────────────────────────────────
    let stored_formulas = env.all_formulas(&txn)?;
    log::info!(target: "sumo_kb::persist",
        "load_from_db: loading {} formulas", stored_formulas.len());

    let max_sent_id = stored_formulas.iter().map(|sf| sf.id).max().unwrap_or(0);

    let mut sentences:     Vec<Sentence>                       = Vec::new();
    let mut sent_idx_map:  HashMap<SentenceId, usize>          = HashMap::new();
    let mut roots:         Vec<SentenceId>                     = Vec::new();
    let mut sub_sentences: Vec<SentenceId>                     = Vec::new();
    let mut file_roots:    HashMap<String, Vec<SentenceId>>    = HashMap::new();
    let mut head_index:    HashMap<String, Vec<SentenceId>>    = HashMap::new();
    let mut session_map:   HashMap<SentenceId, Option<String>> = HashMap::new();

    for sf in &stored_formulas {
        let root_sid = allocate_formula(
            &mut sentences,
            &mut sent_idx_map,
            &mut sub_sentences,
            sf,
            &symbols,
        );
        roots.push(root_sid);
        file_roots.entry(sf.file.clone()).or_default().push(root_sid);
        session_map.insert(root_sid, sf.session.clone());

        // Head index
        if let Some(Element::Symbol(pred_id)) =
            sentences[*sent_idx_map.get(&root_sid).unwrap()].elements.first()
        {
            if let Some(sym_pos) = sym_id_to_vec.get(pred_id) {
                let pred_name = symbol_data[*sym_pos].name.clone();
                head_index.entry(pred_name).or_default().push(root_sid);
                symbol_data[*sym_pos].head_sentences.push(root_sid);
            }
        }
    }

    // ── Assemble KifStore ─────────────────────────────────────────────────────
    let mut store = KifStore::default();
    store.sentences     = sentences;
    store.symbols       = symbols;
    store.symbol_data   = symbol_data;
    store.roots         = roots;
    store.sub_sentences = sub_sentences;
    store.file_roots    = file_roots;
    store.head_index    = head_index;

    // Seed the stable-ID maps from what we loaded
    for (sid, vec_pos) in &sent_idx_map {
        store.insert_sent_idx(*sid, *vec_pos);
    }
    for (sym_id, vec_pos) in &sym_id_to_vec {
        store.insert_sym_idx(*sym_id, *vec_pos);
    }

    // Rebuild taxonomy edges
    rebuild_taxonomy(&mut store);

    // Seed counters so new IDs do not collide with loaded ones
    store.seed_counters(max_sym_id + 1, max_sent_id + 1);

    log::info!(target: "sumo_kb::persist",
        "load_from_db: {} sentences, {} symbols; counters seeded at sym={} sent={}",
        store.sentences.len(), store.symbols.len(),
        max_sym_id + 1, max_sent_id + 1);

    Ok((store, session_map))
}

// ── Allocate a StoredFormula into the KifStore ────────────────────────────────

/// Recursively allocate a `StoredFormula` and all its inline sub-formulas
/// into `sentences`, returning the `SentenceId` of the root.
fn allocate_formula(
    sentences:    &mut Vec<Sentence>,
    sent_idx_map: &mut HashMap<SentenceId, usize>,
    sub_sentences: &mut Vec<SentenceId>,
    sf:           &StoredFormula,
    symbols:      &HashMap<String, u64>,
) -> SentenceId {
    let elements: Vec<Element> = sf.elements.iter().map(|se| {
        stored_element_to_element(sentences, sent_idx_map, sub_sentences, se, symbols)
    }).collect();

    let vec_pos = sentences.len();
    let sid     = sf.id;
    sentences.push(Sentence {
        elements,
        file: sf.file.clone(),
        span: lmdb_span(sid),
    });
    sent_idx_map.insert(sid, vec_pos);
    sid
}

fn stored_element_to_element(
    sentences:    &mut Vec<Sentence>,
    sent_idx_map: &mut HashMap<SentenceId, usize>,
    sub_sentences: &mut Vec<SentenceId>,
    se:           &StoredElement,
    symbols:      &HashMap<String, u64>,
) -> Element {
    match se {
        StoredElement::Symbol(id)                    => Element::Symbol(*id),
        StoredElement::Variable { id, name, is_row } => Element::Variable {
            id: *id, name: name.clone(), is_row: *is_row,
        },
        StoredElement::Literal(lit)                  => Element::Literal(lit.clone()),
        StoredElement::Op(op)                        => Element::Op(op.clone()),
        StoredElement::Sub(sub_sf) => {
            let sub_sid = allocate_formula(sentences, sent_idx_map, sub_sentences, sub_sf, symbols);
            sub_sentences.push(sub_sid);
            Element::Sub(sub_sid)
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn lmdb_span(formula_id: SentenceId) -> Span {
    Span { file: format!("<lmdb:{}>", formula_id), line: 0, col: 0, offset: 0 }
}

fn rebuild_taxonomy(store: &mut KifStore) {
    store.tax_edges.clear();
    store.tax_incoming.clear();
    let root_sids: Vec<SentenceId> = store.roots.clone();
    for sid in root_sids {
        let vec_pos  = match store.sent_idx_map().get(&sid) { Some(&p) => p, None => continue };
        let sentence = &store.sentences[vec_pos];
        let head_sym = match sentence.head_symbol() { Some(id) => id, None => continue };
        let head_name = match store.symbols.iter().find(|(_, &id)| id == head_sym) {
            Some((name, _)) => name.clone(),
            None            => continue,
        };
        let rel = match TaxRelation::from_str(&head_name) { Some(r) => r, None => continue };
        let arg1 = match sentence.elements.get(1) {
            Some(Element::Symbol(id)) | Some(Element::Variable { id, is_row: false, .. }) => *id,
            _ => continue,
        };
        let arg2 = match sentence.elements.get(2) {
            Some(Element::Symbol(id)) | Some(Element::Variable { id, is_row: false, .. }) => *id,
            _ => continue,
        };
        let edge_idx = store.tax_edges.len();
        store.tax_edges.push(TaxEdge { from: arg2, to: arg1, rel });
        store.tax_incoming.entry(arg1).or_default().push(edge_idx);
    }
}
