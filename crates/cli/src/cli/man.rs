// crates/native/src/cli/man.rs
//
// `sumo man <symbol>` -- manpage-style introspection.  Opens whatever
// KB the shared KbArgs point to (LMDB + any `-f/-d` layers), then
// renders the `ManPage` returned by `KnowledgeBase::manpage` into a
// terminal pager (via `minus`) for an interactive less-like reading
// experience.
//
// The pager is bypassed automatically when stdout is not a TTY
// (piped / redirected), when `--no-pager` is passed, or when the
// `NO_PAGER` environment variable is set -- so scripts and CI pipes
// keep their plain-text output.

use std::io::IsTerminal;

use inline_colorization::*;

use sumo_kb::{DocEntry, KnowledgeBase, ManPage, ParentEdge, SentenceId, SortSig};

use crate::cli::args::KbArgs;
use crate::cli::util::open_or_build_kb;

pub fn run_man(
    symbol: String,
    lang: Option<String>,
    no_pager: bool,
    kb_args: KbArgs,
) -> bool {
    let kb = match open_or_build_kb(&kb_args) {
        Ok(kb) => kb,
        Err(_) => return false,
    };

    let Some(man) = kb.manpage(&symbol) else {
        log::error!("symbol '{}' not found in the knowledge base", symbol);
        return false;
    };

    // Build the full rendered man page into a string buffer.  Writes
    // to `String` via `fmt::Write` are infallible, so every `writeln!`
    // here uses `.unwrap()` -- the only error path is OOM, which the
    // allocator panics on anyway.
    let mut buf = String::new();
    write_manpage(&mut buf, &kb, &man, lang.as_deref());

    // Decide whether to page.  Honour (in order): explicit --no-pager
    // flag, NO_PAGER env, non-TTY stdout.  Each one by itself forces
    // direct-print mode.
    let tty     = std::io::stdout().is_terminal();
    let env_off = std::env::var_os("NO_PAGER").is_some();
    let use_pager = !no_pager && !env_off && tty;

    if use_pager {
        match show_in_pager(&buf, &symbol) {
            Ok(()) => true,
            Err(e) => {
                // Pager failed (e.g. no TTY on an unusual terminal).
                // Fall back to direct print so the user still sees
                // the content instead of an opaque error.
                log::warn!("pager failed ({}); falling back to stdout", e);
                print!("{}", buf);
                true
            }
        }
    } else {
        print!("{}", buf);
        true
    }
}

/// Feed the rendered buffer into a `minus::Pager` and block until
/// the user quits.  ANSI colour escapes in `buf` are passed through
/// verbatim, so the `inline_colorization` output in `write_manpage`
/// renders in colour inside the pager.
fn show_in_pager(buf: &str, symbol: &str) -> Result<(), minus::error::MinusError> {
    let pager = minus::Pager::new();
    pager.set_prompt(format!("sumo man {}  (q to quit, / to search)", symbol))?;
    pager.push_str(buf)?;
    minus::page_all(pager)
}

fn write_manpage<W: std::fmt::Write>(
    out: &mut W,
    kb: &KnowledgeBase,
    man: &ManPage,
    lang_filter: Option<&str>,
) {
    // NAME
    write_header(out, "NAME");
    let kinds = if man.kinds.is_empty() {
        String::from("(uncategorised)")
    } else {
        man.kinds.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ")
    };
    writeln!(out, "    {color_yellow}{}{color_reset}  {color_bright_black}({}){color_reset}",
        man.name, kinds).unwrap();

    // PARENTS
    if !man.parents.is_empty() {
        write_header(out, "PARENTS");
        let width = man.parents.iter()
            .map(|p: &ParentEdge| p.relation.len())
            .max().unwrap_or(0);
        for p in &man.parents {
            writeln!(
                out,
                "    {color_cyan}{:<width$}{color_reset}  {color_bright_blue}→{color_reset}  {color_yellow}{}{color_reset}",
                p.relation, p.parent, width = width,
            ).unwrap();
        }
    }

    // SIGNATURE (arity / domains / range)
    let has_sig = man.arity.is_some() || !man.domains.is_empty() || man.range.is_some();
    if has_sig {
        write_header(out, "SIGNATURE");
        if let Some(a) = man.arity {
            let rendered = if a < 0 { "variable".to_string() } else { a.to_string() };
            writeln!(out, "    {color_bright_black}arity:{color_reset}  {}", rendered).unwrap();
        }
        for (pos, sig) in &man.domains {
            writeln!(out, "    {color_bright_black}arg{}:{color_reset}   {}",
                pos, format_sort(sig)).unwrap();
        }
        if let Some(sig) = &man.range {
            writeln!(out, "    {color_bright_black}range:{color_reset}  {}", format_sort(sig)).unwrap();
        }
    }

    // DOCUMENTATION
    let docs = filter_lang(&man.documentation, lang_filter);
    if !docs.is_empty() {
        write_header(out, "DOCUMENTATION");
        for d in &docs {
            writeln!(out, "    {color_bright_black}[{}]{color_reset}", d.language).unwrap();
            for line in wrap_text(&d.text, 72) {
                writeln!(out, "    {}", line).unwrap();
            }
            writeln!(out).unwrap();
        }
    }

    // TERM FORMAT
    let tfs = filter_lang(&man.term_format, lang_filter);
    if !tfs.is_empty() {
        write_header(out, "TERM FORMAT");
        for t in &tfs {
            writeln!(out, "    {color_bright_black}[{}]{color_reset}  {}", t.language, t.text).unwrap();
        }
    }

    // FORMAT
    let fmts = filter_lang(&man.format, lang_filter);
    if !fmts.is_empty() {
        write_header(out, "FORMAT");
        for f in &fmts {
            writeln!(out, "    {color_bright_black}[{}]{color_reset}  {}", f.language, f.text).unwrap();
        }
    }

    // REFERENCES
    //
    // Group root-level occurrences by position — position 0 is the
    // head slot, position 1.. are argument slots.  Variable-arity
    // relations can land at any position, so the bucket vector is
    // sized from the data (previously hard-coded to 5, which
    // panicked on `(contraryAttribute a b c d e)`-style lists).
    //
    // Within each bucket, sort by (file, line) so the output is
    // stable across runs and close-together axioms stay adjacent.
    if !man.ref_args.is_empty() || !man.ref_nested.is_empty() {
        if !man.ref_args.is_empty() {
            let max_pos = man.ref_args.iter().map(|r| r.0).max().unwrap_or(0);
            let mut buckets: Vec<Vec<SentenceId>> = vec![Vec::new(); max_pos + 1];
            for r in &man.ref_args {
                buckets[r.0].push(r.1);
            }
            for (i, pos) in buckets.iter_mut().enumerate() {
                if pos.is_empty() { continue; }
                sort_sids_by_source(pos, kb);
                let label = if i == 0 {
                    String::from("Appearance as head")
                } else {
                    format!("Appearance as argument number {}", i)
                };
                write_header(out, &label);
                for sid in pos.iter() {
                    write_sentence_block(out, kb, *sid);
                }
            }
        }
        if !man.ref_nested.is_empty() {
            let mut nested = man.ref_nested.clone();
            sort_sids_by_source(&mut nested, kb);
            write_header(out, "Appearance nested inside other axioms");
            for sid in &nested {
                write_sentence_block(out, kb, *sid);
            }
        }
    }

    // Blank line to separate from the next prompt, mirroring man(1).
    writeln!(out).unwrap();
}

/// Render one reference entry: the source `file:line` header (dim
/// grey) followed by the pretty-printed sentence indented four
/// spaces, matching the surrounding man-page layout.
fn write_sentence_block<W: std::fmt::Write>(
    out: &mut W,
    kb:  &KnowledgeBase,
    sid: SentenceId,
) {
    let Some(sent) = kb.sentence(sid) else { return };
    let trace = format!("{}:{}", sent.span.file, sent.span.line);
    writeln!(out, "    {color_bright_black}{}{color_reset}", trace).unwrap();
    // `pretty_print_sentence` returns ANSI-coloured multi-line KIF
    // when the sentence is wide enough to break.  We prepend each
    // line with four spaces so the whole block stays aligned under
    // the source trace.
    let pretty = kb.pretty_print_sentence(sid, 4);
    for line in pretty.lines() {
        writeln!(out, "    {}", line).unwrap();
    }
}

/// Sort a list of sentence ids by (file, line) for stable output.
fn sort_sids_by_source(sids: &mut Vec<SentenceId>, kb: &KnowledgeBase) {
    sids.sort_by(|a, b| {
        let sa = kb.sentence(*a);
        let sb = kb.sentence(*b);
        match (sa, sb) {
            (Some(a), Some(b)) => (a.file.as_str(), a.span.line)
                .cmp(&(b.file.as_str(), b.span.line)),
            _ => a.cmp(b),
        }
    });
}

fn write_header<W: std::fmt::Write>(out: &mut W, title: &str) {
    writeln!(out).unwrap();
    writeln!(out, "{style_bold}{}{style_reset}", title).unwrap();
}

fn format_sort(sig: &SortSig) -> String {
    if sig.subclass {
        format!("{color_yellow}{}{color_reset} {color_bright_black}(subclass-of){color_reset}", sig.class)
    } else {
        format!("{color_yellow}{}{color_reset}", sig.class)
    }
}

/// Keep only entries whose language matches `want`.  `None` returns
/// the list unchanged (all languages shown).
fn filter_lang<'a>(entries: &'a [DocEntry], want: Option<&str>) -> Vec<&'a DocEntry> {
    match want {
        None    => entries.iter().collect(),
        Some(l) => entries.iter().filter(|e| e.language == l).collect(),
    }
}
/// Soft-wrap `text` at `width` columns, splitting on spaces.  The KIF
/// `documentation` convention sometimes inlines `&%Symbol` cross-refs;
/// we preserve them verbatim.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur   = String::new();
    for word in text.split_whitespace() {
        if !cur.is_empty() && cur.len() + 1 + word.len() > width {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() { cur.push(' '); }
        cur.push_str(word);
    }
    if !cur.is_empty() { lines.push(cur); }
    if lines.is_empty() { lines.push(String::new()); }
    lines
}
