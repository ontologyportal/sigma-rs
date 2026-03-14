use log;
use inline_colorization::*;

use sumo_parser_core::KnowledgeBase;
use sumo_store::{CommitOptions, commit_kifstore, db_to_tptp_cnf};

use crate::ask::{run_ask_with_tptp, AskOptions};
use crate::cli::args::KbArgs;
use crate::cli::util::{load_kb_from_db, open_existing_db, read_stdin};

pub fn run_ask(
    formula: Option<String>,
    tell: Vec<String>,
    timeout: u32,
    session: String,
    kb_args: KbArgs,
    keep: bool,
) -> bool {
    log::debug!(
        "run_ask: formula={:?}, tell={}, timeout={}, session={:?}, db={}",
        formula.is_some(), tell.len(), timeout, session, kb_args.db.display()
    );

    let conjecture = match formula.or_else(read_stdin) {
        Some(f) => f,
        None => {
            log::error!(
                "ask requires a conjecture formula (supply as argument or via stdin)"
            );
            return false;
        }
    };

    // Open the existing database — the KB must have been initialised via `validate`
    let env = match open_existing_db(&kb_args) {
        Ok(e)  => e,
        Err(e) => { log::error!("{}", e); return false; }
    };

    // Reconstruct the in-memory KB for tell() validation and query parsing
    let mut kb = match load_kb_from_db(&env) {
        Ok(k)   => k,
        Err(()) => return false,
    };
    log::info!("ask: loaded {} axiom(s) from database", kb.store.roots.len());

    // Apply tell() assertions (validate against KB + commit to DB under session)
    for kif in &tell {
        log::debug!("ask: tell (session={:?}): {}", session, kif);
        let r = kb.tell(&session, kif);
        if !r.ok {
            for e in &r.errors { log::error!("tell error: {}", e); }
            return false;
        }
        // Commit the assertion to the database under the session
        if let Some(sid) = r.sentence_id {
            // Build a mini KifStore containing only this one assertion for commit
            let assertion_store = extract_assertion_store(&kb, sid);
            let opts = CommitOptions {
                max_clauses: kb_args.max_clauses,
                session:     Some(session.clone()),
            };
            if let Err(e) = commit_kifstore(&env, &assertion_store, &opts) {
                log::error!("Failed to commit assertion to database: {}", e);
                return false;
            }
        }
    }
    log::debug!("ask: applied {} tell assertion(s)", tell.len());

    // Generate TPTP CNF from the database (pre-computed clauses)
    let kb_tptp = match db_to_tptp_cnf(&env, "kb", Some(&session)) {
        Ok(t)  => t,
        Err(e) => { log::error!("TPTP generation failed: {}", e); return false; }
    };
    log::debug!("ask: generated {} chars of TPTP CNF", kb_tptp.len());

    let result = run_ask_with_tptp(
        &mut kb,
        &conjecture,
        &kb_tptp,
        AskOptions {
            vampire_path:  kb_args.vampire,
            timeout_secs:  Some(timeout),
            keep_tmp_file: keep,
            session:       Some(session),
            tmp_file:      None,
        },
    );

    if !result.errors.is_empty() {
        for e in &result.errors { log::error!("error: {}", e); }
        return false;
    }

    print!(
        "{style_bold}Theorem prover completed successfully: {style_reset}{}",
        result.raw_output
    );
    result.proved
}

/// Build a minimal `KifStore` containing only the sentence at `sid` and
/// all its sub-sentences, for committing a single tell() assertion.
pub fn extract_assertion_store_pub(
    kb:  &KnowledgeBase,
    sid: sumo_parser_core::store::SentenceId,
) -> sumo_parser_core::store::KifStore {
    extract_assertion_store(kb, sid)
}

fn extract_assertion_store(
    kb:  &KnowledgeBase,
    sid: sumo_parser_core::store::SentenceId,
) -> sumo_parser_core::store::KifStore {
    use sumo_parser_core::store::KifStore;

    // We need to copy the relevant sentences (root + subs) and build a
    // mini-store with the same symbol IDs.
    let mut mini = KifStore::default();

    // Copy entire symbol table (since sub-sentences can reference any symbol)
    mini.symbols     = kb.store.symbols.clone();
    mini.symbol_data = kb.store.symbol_data.clone();

    // Collect the sentence tree
    collect_sentences_recursive(&kb.store, sid, &mut mini.sentences, &mut mini.sub_sentences);

    // The root is always the last sentence we just added (added last)
    let root_sid = mini.sentences.len() as u64 - 1;
    mini.roots.push(root_sid);

    if let Some(head) = mini.sentences[root_sid as usize].head_symbol() {
        let head_name = mini.sym_name(head).to_owned();
        mini.head_index.entry(head_name).or_default().push(root_sid);
    }

    mini
}

fn collect_sentences_recursive(
    src:      &sumo_parser_core::store::KifStore,
    sid:      sumo_parser_core::store::SentenceId,
    dest:     &mut Vec<sumo_parser_core::store::Sentence>,
    subs_out: &mut Vec<sumo_parser_core::store::SentenceId>,
) -> sumo_parser_core::store::SentenceId {
    use sumo_parser_core::store::Element;

    let orig = &src.sentences[sid as usize];
    let mut new_elements = Vec::with_capacity(orig.elements.len());

    for elem in &orig.elements {
        if let Element::Sub(sub_sid) = elem {
            let new_sub_sid = collect_sentences_recursive(src, *sub_sid, dest, subs_out);
            subs_out.push(new_sub_sid);
            new_elements.push(Element::Sub(new_sub_sid));
        } else {
            new_elements.push(elem.clone());
        }
    }

    let new_sid = dest.len() as u64;
    dest.push(sumo_parser_core::store::Sentence {
        elements: new_elements,
        file:     orig.file.clone(),
        span:     orig.span.clone(),
    });
    new_sid
}
