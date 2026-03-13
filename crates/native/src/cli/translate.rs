use log;
use sumo_parser_core::{
    kb_to_tptp, load_kif, sentence_to_tptp, KnowledgeBase, TptpOptions,
};
use crate::cli::args::KbArgs;
use crate::cli::util::{build_store, maybe_save_cache, parse_lang, read_stdin, source_tag};
use crate::{parse_error, semantic_error};

pub fn run_translate(
    formula: Option<String>,
    lang: &str,
    show_numbers: bool,
    session: Option<&str>,
    kb_args: KbArgs,
) -> bool {
    let store = match build_store(&kb_args) {
        Ok(s) => s,
        Err(..) => {
            return false;
        }
    };
    maybe_save_cache(&store, kb_args.cache.as_deref());

    let tptp_lang = parse_lang(lang);
    let opts = TptpOptions {
        lang: tptp_lang,
        hide_numbers: !show_numbers,
        ..TptpOptions::default()
    };

    let formula = formula.or_else(read_stdin);
    let mut kb = KnowledgeBase::new(store);

    match formula {
        Some(text) => {
            // Load into the store (not through kb.load_kif which clears caches)
            // so the KB semantic context is available for mention-suffix detection.
            let tag = source_tag();
            let errors = load_kif(&mut kb.store, &text, tag);
            let mut ok = true;
            for (span, e) in &errors {
                parse_error!(span, e);
                ok = false;
            }
            if !ok {
                return false;
            }

            let sids: Vec<_> = kb.store.file_roots.get(tag).cloned().unwrap_or_default();

            if sids.is_empty() {
                log::error!("error: no sentences parsed from input");
                return false;
            }

            // Run validation and report errors, but proceed with translation.
            kb.validate_kb_once();
            for &sid in &sids {
                let span = &kb.store.sentences[sid].span;
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
            // Files-only mode: emit the full KB as TPTP.
            for (sid, err) in &kb.validate_all() {
                let span = &kb.store.sentences[*sid].span;
                semantic_error!(span, err, *sid, kb);
            }
            print!("{}", kb_to_tptp(&kb, "kb", &opts, session));
            true
        }
    }
}
