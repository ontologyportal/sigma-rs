use log;
use sumo_parser_core::{
    load_kif, KnowledgeBase,
};
use crate::cli::args::KbArgs;
use crate::cli::util::{build_store, maybe_save_cache, read_stdin, source_tag};
use crate::{parse_error, semantic_error};

// Validation subcommand
pub fn run_validate(formula: Option<String>, kb_args: KbArgs) -> bool {
    log::trace!(
        "run_validate(formula={:?},
	kb_args={:#?})",
        formula,
        kb_args
    );
    log::debug!("Entering Validation command");
    let store = match build_store(&kb_args) {
        Ok(s) => s,
        Err(..) => {
            return false;
        }
    };
    log::info!("Successfully loaded {} axioms from consituents", store.sentences.len());
    maybe_save_cache(&store, kb_args.cache.as_deref());

    let formula = formula.or_else(read_stdin);
    let mut kb = KnowledgeBase::new(store);

    match formula {
        Some(text) => {
            log::debug!("Validating formula: {}", &text);
            validate_formula(&mut kb, &text, source_tag())
        },
        None => validate_all_roots(&kb),
    }
}

/// Validate a single formula string against the KB.
pub fn validate_formula(
    kb: &mut KnowledgeBase,
    text: &str,
    tag: &str,
) -> bool {
    log::trace!(
        "validate_formula(kb={{KnowledgeBase}}), {:?}, {:?})",
        text,
        tag
    );
    log::debug!("Validating single formula: {}", text);
    // Warm semantic caches from existing KB sentences before checking new ones.
    kb.validate_kb_once();
    log::debug!("KB Validated");

    // Load the formula directly into the store (bypassing kb.load_kif which
    // would clear caches) so our chosen file tag appears in error spans.
    let parse_errors = load_kif(&mut kb.store, text, tag);
    let mut ok = true;
    for (span, e) in &parse_errors {
        parse_error!(span, e, text);
        ok = false;
    }
    if !ok {
        return false;
    }

    let sids: Vec<_> = kb.store.file_roots.get(tag).cloned().unwrap_or_default();

    if sids.is_empty() {
        log::error!("no sentences were parsed");
        return false;
    }

    for sid in sids {
        log::trace!("sid = {}", sid);
        let span = &kb.store.sentences[sid].span;
        if let Err(e) = kb.validate_sentence(sid) {
            semantic_error!(span, e, sid, kb);
            ok = false;
        }
    }
    ok
}

/// Validate every root sentence in the KB and report errors (files-only mode).
pub fn validate_all_roots(kb: &KnowledgeBase) -> bool {
    log::trace!("validate_all_roots(kb={{KnowledgeBase}})");
    let failures: Vec<_> = kb
        .validate_all()
        .into_iter()
        .collect();

    for (sid, e) in &failures {
        let sent = &kb.store.sentences[*sid];
        semantic_error!(sent.span, e, *sid, kb);
    }
    let total = kb.store.roots.len();
    let n_err = failures.len();
    if n_err == 0 {
        println!("{} formula(s) validated: all OK", total);
        true
    } else {
        log::warn!("{} formula(s) validated: {} error(s)", total, n_err);
        false
    }
}
