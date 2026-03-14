use log;
use sumo_parser_core::{kb_to_tptp, load_kif, sentence_to_tptp, KnowledgeBase, TptpOptions};
use sumo_store::{CommitOptions, commit_kifstore, db_to_tptp_cnf};

use crate::cli::args::KbArgs;
use crate::cli::util::{
    build_store_from_files, load_kb_from_db,
    open_existing_db, parse_lang, read_stdin, source_tag,
};
use crate::{parse_error, semantic_error};

pub fn run_translate(
    formula:      Option<String>,
    lang:         &str,
    show_numbers: bool,
    session:      Option<&str>,
    kb_args:      KbArgs,
) -> bool {
    let db_exists = kb_args.db.exists();
    let formula   = formula.or_else(read_stdin);

    if db_exists {
        // ── DB mode: emit TPTP CNF from the database ──────────────────────────
        log::info!("translate: DB mode — reading from {}", kb_args.db.display());
        run_translate_db(formula, session, kb_args)
    } else {
        // ── Legacy in-memory mode: parse KIF files and emit TPTP FOF ──────────
        log::info!("translate: in-memory mode (no DB found at {})", kb_args.db.display());
        run_translate_memory(formula, lang, show_numbers, session, kb_args)
    }
}

// ── DB-backed translate ───────────────────────────────────────────────────────

fn run_translate_db(
    formula: Option<String>,
    session: Option<&str>,
    kb_args: KbArgs,
) -> bool {
    let env = match open_existing_db(&kb_args) {
        Ok(e)  => e,
        Err(e) => { log::error!("{}", e); return false; }
    };

    // If a formula is given, treat it as a session assertion
    if let Some(text) = formula {
        let kb = match load_kb_from_db(&env) {
            Ok(k)   => k,
            Err(()) => return false,
        };
        let mut kb = kb;
        let sess = session.unwrap_or("translate");
        let r = kb.tell(sess, &text);
        if !r.ok {
            for e in &r.errors { log::error!("tell error: {}", e); }
            return false;
        }
        if let Some(sid) = r.sentence_id {
            let mini = crate::cli::ask::extract_assertion_store_pub(&kb, sid);
            let opts = CommitOptions {
                max_clauses: kb_args.max_clauses,
                session:     Some(sess.to_owned()),
            };
            if let Err(e) = commit_kifstore(&env, &mini, &opts) {
                log::error!("Failed to commit assertion: {}", e);
                return false;
            }
        }
    }

    match db_to_tptp_cnf(&env, "kb", session) {
        Ok(tptp) => { print!("{}", tptp); true }
        Err(e)   => { log::error!("TPTP CNF generation failed: {}", e); false }
    }
}

// ── In-memory translate (legacy, no DB) ──────────────────────────────────────

fn run_translate_memory(
    formula:      Option<String>,
    lang:         &str,
    show_numbers: bool,
    session:      Option<&str>,
    kb_args:      KbArgs,
) -> bool {
    let store = match build_store_from_files(&kb_args) {
        Ok(s)   => s,
        Err(()) => return false,
    };

    let tptp_lang = parse_lang(lang);
    let opts = TptpOptions {
        lang:         tptp_lang,
        hide_numbers: !show_numbers,
        ..TptpOptions::default()
    };

    let mut kb = KnowledgeBase::new(store);

    match formula {
        Some(text) => {
            let tag    = source_tag();
            let errors = load_kif(&mut kb.store, &text, tag);
            let mut ok = true;
            for (span, e) in &errors {
                parse_error!(span, e);
                ok = false;
            }
            if !ok { return false; }

            let sids: Vec<_> = kb.store.file_roots.get(tag).cloned().unwrap_or_default();
            if sids.is_empty() {
                log::error!("error: no sentences parsed from input");
                return false;
            }

            kb.validate_kb_once();
            for &sid in &sids {
                let span = kb.store.sentences[sid as usize].span.clone();
                if let Err(e) = kb.validate_sentence(sid) {
                    semantic_error!(span, e, sid, kb);
                }
            }
            for sid in sids {
                println!("{}", sentence_to_tptp(sid, &kb, &opts));
            }
            true
        }
        None => {
            for (sid, err) in &kb.validate_all() {
                let span = kb.store.sentences[*sid as usize].span.clone();
                semantic_error!(span, err, *sid, kb);
            }
            print!("{}", kb_to_tptp(&kb, "kb", &opts, session));
            true
        }
    }
}
