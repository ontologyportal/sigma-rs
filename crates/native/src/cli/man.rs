// crates/native/src/cli/man.rs
//
// `sumo man <symbol>` -- manpage-style introspection.  Opens whatever
// KB the shared KbArgs point to (LMDB + any `-f/-d` layers), then
// renders the `ManPage` returned by `KnowledgeBase::manpage`.

use inline_colorization::*;

use sumo_kb::{DocEntry, KnowledgeBase, ManPage, ParentEdge, SortSig};

use crate::cli::args::KbArgs;
use crate::cli::util::open_or_build_kb;

pub fn run_man(symbol: String, lang: Option<String>, kb_args: KbArgs) -> bool {
    let kb = match open_or_build_kb(&kb_args) {
        Ok(kb) => kb,
        Err(_) => return false,
    };

    let Some(man) = kb.manpage(&symbol) else {
        log::error!("symbol '{}' not found in the knowledge base", symbol);
        return false;
    };

    print_manpage(&kb, &man, lang.as_deref());
    true
}

fn print_manpage(_kb: &KnowledgeBase, man: &ManPage, lang_filter: Option<&str>) {
    // NAME
    print_header("NAME");
    let kinds = if man.kinds.is_empty() {
        String::from("(uncategorised)")
    } else {
        man.kinds.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ")
    };
    println!("    {color_yellow}{}{color_reset}  {color_bright_black}({}){color_reset}",
        man.name, kinds);

    // PARENTS
    if !man.parents.is_empty() {
        print_header("PARENTS");
        let width = man.parents.iter()
            .map(|p: &ParentEdge| p.relation.len())
            .max().unwrap_or(0);
        for p in &man.parents {
            println!(
                "    {color_cyan}{:<width$}{color_reset}  {color_bright_blue}→{color_reset}  {color_yellow}{}{color_reset}",
                p.relation, p.parent, width = width,
            );
        }
    }

    // SIGNATURE (arity / domains / range)
    let has_sig = man.arity.is_some() || !man.domains.is_empty() || man.range.is_some();
    if has_sig {
        print_header("SIGNATURE");
        if let Some(a) = man.arity {
            let rendered = if a < 0 { "variable".to_string() } else { a.to_string() };
            println!("    {color_bright_black}arity:{color_reset}  {}", rendered);
        }
        for (pos, sig) in &man.domains {
            println!("    {color_bright_black}arg{}:{color_reset}   {}",
                pos, format_sort(sig));
        }
        if let Some(sig) = &man.range {
            println!("    {color_bright_black}range:{color_reset}  {}", format_sort(sig));
        }
    }

    // DOCUMENTATION
    let docs = filter_lang(&man.documentation, lang_filter);
    if !docs.is_empty() {
        print_header("DOCUMENTATION");
        for d in &docs {
            println!("    {color_bright_black}[{}]{color_reset}", d.language);
            for line in wrap_text(&d.text, 72) {
                println!("    {}", line);
            }
            println!();
        }
    }

    // TERM FORMAT
    let tfs = filter_lang(&man.term_format, lang_filter);
    if !tfs.is_empty() {
        print_header("TERM FORMAT");
        for t in &tfs {
            println!("    {color_bright_black}[{}]{color_reset}  {}", t.language, t.text);
        }
    }

    // FORMAT
    let fmts = filter_lang(&man.format, lang_filter);
    if !fmts.is_empty() {
        print_header("FORMAT");
        for f in &fmts {
            println!("    {color_bright_black}[{}]{color_reset}  {}", f.language, f.text);
        }
    }

    // Blank line to separate from the next prompt, mirroring man(1).
    println!();
}

fn print_header(title: &str) {
    println!();
    println!("{style_bold}{}{style_reset}", title);
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
