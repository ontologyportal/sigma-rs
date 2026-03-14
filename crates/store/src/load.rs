/// Reconstruct an in-memory `KifStore` from the LMDB-backed persistent store.
///
/// This allows the semantic-analysis layer (`KnowledgeBase`) to work with
/// formulas loaded from the database without re-parsing KIF files.
///
/// Sub-formulas stored inline in `StoredElement::Sub(…)` are recursively
/// allocated into the `KifStore` as sub-sentences, mirroring the original
/// parse-time structure.

use std::collections::HashMap;

use sumo_parser_core::store::{Element, KifStore, Literal as KifLiteral, Sentence, Symbol};
use sumo_parser_core::error::Span;
use log;

use crate::env::LmdbEnv;
use crate::schema::{FormulaId, StoredElement, StoredFormula, StoredLiteral};
use crate::StoreError;

/// Dummy `Span` used for formulas loaded from LMDB — source location is
/// discarded at commit time.
fn lmdb_span(formula_id: FormulaId) -> Span {
    Span {
        file:   format!("<lmdb:{}>", formula_id),
        line:   0,
        col:    0,
        offset: 0,
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Reconstruct a `KifStore` from all formulas stored in `env`.
///
/// Symbols are loaded with their persistent `SymbolId`s so that the
/// `KifStore`'s Vec-indexed `symbol_data` aligns with the IDs used in
/// `StoredFormula.elements`.
pub fn load_kifstore_from_db(env: &LmdbEnv) -> Result<KifStore, StoreError> {
    let txn = env.read_txn()?;

    // ── Load symbols ──────────────────────────────────────────────────────────
    let stored_syms = env.all_symbols(&txn)?;
    log::info!("load_kifstore_from_db: loading {} symbols", stored_syms.len());

    let max_id = stored_syms.iter().map(|s| s.id).max().unwrap_or(0);
    let mut symbol_data: Vec<Symbol> = vec![Symbol::default(); (max_id + 1) as usize];
    let mut symbols: HashMap<String, u64> = HashMap::new();

    for sym in &stored_syms {
        symbols.insert(sym.name.clone(), sym.id);
        symbol_data[sym.id as usize] = Symbol {
            name:           sym.name.clone(),
            head_sentences: Vec::new(), // rebuilt below
            all_sentences:  Vec::new(),
        };
    }

    // ── Load formulas ─────────────────────────────────────────────────────────
    let stored_formulas = env.all_formulas(&txn)?;
    log::info!("load_kifstore_from_db: loading {} formulas", stored_formulas.len());

    let mut sentences: Vec<Sentence> = Vec::new();
    let mut roots:     Vec<u64> = Vec::new();
    let mut sub_sentences: Vec<u64> = Vec::new();
    let mut file_roots: HashMap<String, Vec<u64>> = HashMap::new();
    let mut head_index: HashMap<String, Vec<u64>> = HashMap::new();

    for sf in &stored_formulas {
        let root_sid = allocate_formula(
            &mut sentences,
            &mut sub_sentences,
            sf,
            &symbols,
        );
        roots.push(root_sid);
        let file_tag = format!("<lmdb:{}>", sf.id);
        file_roots.entry(file_tag).or_default().push(root_sid);

        // Rebuild head index entry
        if let Some(pred_name) = head_name_of(&sentences, root_sid, &symbol_data) {
            head_index.entry(pred_name).or_default().push(root_sid);
            symbol_data[sentences[root_sid as usize]
                .elements.first()
                .and_then(|e| if let Element::Symbol(id) = e { Some(*id as usize) } else { None })
                .unwrap_or(0)]
                .head_sentences.push(root_sid);
        }
    }

    // ── Build KifStore ────────────────────────────────────────────────────────
    let mut store = KifStore::default();
    store.sentences    = sentences;
    store.symbols      = symbols;
    store.symbol_data  = symbol_data;
    store.roots        = roots;
    store.sub_sentences = sub_sentences;
    store.file_roots   = file_roots;
    store.head_index   = head_index;
    store.tax_edges    = Vec::new();
    store.tax_incoming = HashMap::new();

    // Rebuild taxonomy from the reconstructed sentences
    // (KifStore::rebuild_taxonomy is private, so we re-extract edges manually)
    rebuild_taxonomy(&mut store);

    log::info!(
        "load_kifstore_from_db: reconstructed store with {} sentences, {} symbols",
        store.sentences.len(),
        store.symbols.len()
    );
    Ok(store)
}

// ── Allocate sentences from a StoredFormula ───────────────────────────────────

/// Recursively allocate a `StoredFormula` and all its inline sub-formulas
/// into `sentences`, returning the `SentenceId` of the root.
fn allocate_formula(
    sentences:     &mut Vec<Sentence>,
    sub_sentences: &mut Vec<u64>,
    sf:            &StoredFormula,
    symbols:       &HashMap<String, u64>,
) -> u64 {
    let mut elements: Vec<Element> = Vec::new();

    for se in &sf.elements {
        let elem = stored_element_to_element(sentences, sub_sentences, se, symbols, sf.id);
        elements.push(elem);
    }

    let sid = sentences.len() as u64;
    sentences.push(Sentence {
        elements,
        file: format!("<lmdb:{}>", sf.id),
        span: lmdb_span(sf.id),
    });
    sid
}

fn stored_element_to_element(
    sentences:     &mut Vec<Sentence>,
    sub_sentences: &mut Vec<u64>,
    se:            &StoredElement,
    symbols:       &HashMap<String, u64>,
    _parent_id:    FormulaId,
) -> Element {
    match se {
        StoredElement::Symbol(id)                      => Element::Symbol(*id),
        StoredElement::Variable { id, name, is_row }   => Element::Variable {
            id:     *id,
            name:   name.clone(),
            is_row: *is_row,
        },
        StoredElement::Literal(StoredLiteral::Str(s))    => Element::Literal(KifLiteral::Str(s.clone())),
        StoredElement::Literal(StoredLiteral::Number(n)) => Element::Literal(KifLiteral::Number(n.clone())),
        StoredElement::Op(op)                            => Element::Op(op.clone()),
        StoredElement::Sub(sub_sf) => {
            let sub_sid = allocate_formula(sentences, sub_sentences, sub_sf, symbols);
            sub_sentences.push(sub_sid);
            Element::Sub(sub_sid)
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn head_name_of(
    sentences:   &[Sentence],
    sid:         u64,
    symbol_data: &[Symbol],
) -> Option<String> {
    sentences.get(sid as usize)?.elements.first().and_then(|e| {
        if let Element::Symbol(id) = e {
            symbol_data.get(*id as usize).map(|s| s.name.clone())
        } else {
            None
        }
    })
}

/// Rebuild taxonomy edges for a reconstructed `KifStore`.
fn rebuild_taxonomy(store: &mut KifStore) {
    use sumo_parser_core::store::TaxRelation;

    store.tax_edges.clear();
    store.tax_incoming.clear();

    let n = store.sentences.len() as u64;
    for sid in 0..n {
        let sentence = &store.sentences[sid as usize];
        let head_sym = match sentence.head_symbol() {
            Some(id) => id,
            None     => continue,
        };
        let head_name = store.sym_name(head_sym).to_owned();
        let rel = match TaxRelation::from_str(&head_name) {
            Some(r) => r,
            None    => continue,
        };
        let arg1 = match sentence.elements.get(1) {
            Some(Element::Symbol(id))                        => *id,
            Some(Element::Variable { id, is_row: false, .. }) => *id,
            _ => continue,
        };
        let arg2 = match sentence.elements.get(2) {
            Some(Element::Symbol(id))                        => *id,
            Some(Element::Variable { id, is_row: false, .. }) => *id,
            _ => continue,
        };
        let edge_idx = store.tax_edges.len();
        store.tax_edges.push(sumo_parser_core::store::TaxEdge { from: arg2, to: arg1, rel });
        store.tax_incoming.entry(arg1).or_default().push(edge_idx);
    }
}
