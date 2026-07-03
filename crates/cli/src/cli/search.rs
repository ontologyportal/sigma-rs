// crates/cli/src/cli/search.rs
//
// `sumo search QUERY` — substring lookup against SUMO's curated
// natural-language fields (documentation, termFormat, format).
//
// Delegates the heavy lifting to `KnowledgeBase::search` from
// `sigmakee-rs-core`; this module's job is just to parse the CLI
// options, run the query, and render the result.

use crate::style::*;
use sigmakee_rs_sdk::{ManKind, SearchOpts, SearchSource, TopLayer};
use sigmakee_rs_sdk::{Session, manager::KBManager};

pub fn run_search<L>(
    session: Session<L>,
    _manager: KBManager,
    query:   String,
    kind:    Option<String>,
    lang:    Option<String>,
    limit:   usize,
) -> bool 
where L: TopLayer {
    // Parse --kind into the typed enum, or bail with a clear error if
    // the user typo'd a value clap can't validate (we accept the same
    // strings ManKind::as_str produces).
    let kind_filter = match kind.as_deref() {
        None              => None,
        Some("class")     => Some(ManKind::Class),
        Some("relation")  => Some(ManKind::Relation),
        Some("function")  => Some(ManKind::Function),
        Some("predicate") => Some(ManKind::Predicate),
        Some("instance")  => Some(ManKind::Instance),
        Some("individual") => Some(ManKind::Individual),
        Some(other) => {
            log::error!(
                "unknown --kind '{}'; expected one of: \
                 class, instance, relation, function, predicate, individual",
                other
            );
            return false;
        }
    };

    let opts = SearchOpts {
        kind:     kind_filter,
        language: lang.as_deref(),
        limit:    if limit == 0 { None } else { Some(limit) },
    };

    let Ok(hits) = session.search(&query, &opts) else {
        unreachable!("Search currently cannot return an error");
    };

    if hits.is_empty() {
        println!("{color_bright_yellow}(no matches){color_reset}");
        return false;
    }

    // Compute alignment widths once so the columns line up.  Symbol
    // and kind columns get fixed widths; the snippet wraps at terminal
    // width minus the prefix.
    let max_sym  = hits.iter().map(|h| h.symbol.len()).max().unwrap_or(0);
    let max_kind = hits.iter()
        .map(|h| h.kinds.iter().map(|k| k.as_str().len()).sum::<usize>()
                 + h.kinds.len().saturating_sub(1))
        .max()
        .unwrap_or(0);

    for hit in &hits {
        let kinds_str: String = hit.kinds.iter()
            .map(|k| k.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let src_label = match hit.source {
            SearchSource::TermFormat    => "term  ",
            SearchSource::Documentation => "doc   ",
            SearchSource::Format        => "format",
        };

        // Truncate long snippets and inline the language tag for
        // multilingual scans.  Newlines in the doc text get collapsed
        // so a single hit always occupies one terminal line.
        let snippet = collapse_ws(&hit.text);
        let snippet = truncate(&snippet, 80);

        println!(
            "{color_bright_cyan}{:<sym_w$}{color_reset}  \
             {color_bright_black}{:<kind_w$}{color_reset}  \
             {color_yellow}{}{color_reset} \
             {color_bright_black}[{}]{color_reset}  {}",
            hit.symbol,
            kinds_str,
            src_label,
            hit.language,
            snippet,
            sym_w  = max_sym,
            kind_w = max_kind,
        );
    }

    // Footer summary — useful when --limit truncated the output.
    println!(
        "{color_bright_black}{} hit(s){}{color_reset}",
        hits.len(),
        if limit > 0 && hits.len() == limit { " (limit reached — pass --limit 0 for all)" } else { "" }
    );

    true
}

/// Collapse runs of whitespace (incl. newlines) to single spaces so
/// docstring multilines render on one terminal line.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncate at character boundary with an ellipsis when over `max`.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max { return s.to_string(); }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}
