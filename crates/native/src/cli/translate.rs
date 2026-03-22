use log;
use sumo_kb::TptpOptions;

use crate::cli::args::KbArgs;
use crate::cli::util::{open_or_build_kb, parse_lang, read_stdin, source_tag};
use crate::semantic_error;

pub fn run_translate(
    formula:      Option<String>,
    lang:         &str,
    show_numbers: bool,
    show_kif:     bool,
    session:      Option<&str>,
    kb_args:      KbArgs,
) -> bool {
    let formula = formula.or_else(read_stdin);

    let mut kb = match open_or_build_kb(&kb_args) {
        Ok(k)   => k,
        Err(()) => return false,
    };

    let tptp_lang = parse_lang(lang);
    let opts = TptpOptions {
        lang:             tptp_lang,
        hide_numbers:     !show_numbers,
        show_kif_comment: show_kif,
        ..TptpOptions::default()
    };

    match formula {
        Some(text) => {
            let tag    = source_tag();
            let sess   = session.unwrap_or(tag);
            let result = kb.load_kif(&text, tag, Some(sess));
            if !result.ok {
                for e in &result.errors { log::error!("{}", e); }
                return false;
            }
            let sids = kb.session_sids(sess);
            if sids.is_empty() {
                log::error!("no sentences parsed from input");
                return false;
            }
            for &sid in &sids {
                if let Err(e) = kb.validate_sentence(sid) {
                    semantic_error!(&e, kb);
                }
            }
            for sid in sids {
                if show_kif {
                    println!("% {}", kb.sentence_kif_str(sid));
                }
                println!("{}", kb.format_sentence_tptp(sid, &opts));
            }
            true
        }
        None => {
            for (_, err) in &kb.validate_all() {
                semantic_error!(err, kb);
            }
            print!("{}", kb.to_tptp(&opts, session));
            true
        }
    }
}
