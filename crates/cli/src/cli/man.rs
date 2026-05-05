// crates/cli/src/cli/man.rs
//
// `sumo man <symbol>` -- manpage-style introspection.  Opens whatever
// KB the shared KbArgs point to (LMDB + any `-f/-d` layers), then
// renders the `ManPage` returned by `KnowledgeBase::manpage` into an
// interactive viewer (alternate-screen, crossterm raw mode) where Tab
// cycles through every link on the page (PARENTS entries plus inline
// `&%Symbol` cross-refs in the documentation / term-format / format
// blocks), Enter follows the focused link, Backspace pops the
// navigation history, and `q` quits.
//
// The viewer is bypassed automatically when stdout is not a TTY
// (piped / redirected), when `--no-pager` is passed, or when the
// `NO_PAGER` environment variable is set -- so scripts and CI pipes
// keep their plain-text output.

use std::io::{self, IsTerminal, Write};

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute, queue,
    terminal::{
        disable_raw_mode, enable_raw_mode, size, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};

use sigmakee_rs_core::{DocEntry, KnowledgeBase, ManPage, SentenceId, SortSig};

use crate::cli::args::KbArgs;
use crate::cli::util::open_or_build_kb;

pub fn run_man(
    symbol:   String,
    lang:     Option<String>,
    no_pager: bool,
    kb_args:  KbArgs,
) -> bool {
    let kb = match open_or_build_kb(&kb_args) {
        Ok(kb) => kb,
        Err(_) => return false,
    };

    let Some(man) = kb.manpage(&symbol) else {
        log::error!("symbol '{}' not found in the knowledge base", symbol);
        return false;
    };

    // Decide whether to enter the interactive viewer.  Honour (in order):
    // explicit --no-pager flag, NO_PAGER env, non-TTY stdout.  Each one
    // by itself forces direct-print mode.
    let tty       = io::stdout().is_terminal();
    let env_off   = std::env::var_os("NO_PAGER").is_some();
    let use_pager = !no_pager && !env_off && tty;

    if use_pager {
        // Clone `man` so the fallback path below still has an owned
        // copy if the viewer returns an error (terminal detach etc.).
        match interactive_view(&kb, man.clone(), lang.as_deref()) {
            Ok(()) => true,
            Err(e) => {
                // Viewer failed (e.g. detached terminal).  Fall back to
                // direct print so the user still sees the content
                // instead of an opaque error.
                log::warn!("manpage viewer failed ({}); falling back to stdout", e);
                let doc = build_document(&kb, &man, lang.as_deref());
                print_document_plain(&doc);
                true
            }
        }
    } else {
        let doc = build_document(&kb, &man, lang.as_deref());
        print_document_plain(&doc);
        true
    }
}

// ---------------------------------------------------------------------------
// Document model
// ---------------------------------------------------------------------------

/// A single styled run on a line.  `style` is a pre-built ANSI prefix
/// (e.g. `"\x1b[33m\x1b[4m"`); the renderer always emits a `\x1b[0m`
/// reset after the text.  `link` is `Some(idx)` if this span is the
/// visible label of a navigable cross-ref, and points into
/// `Document.links`.
///
/// When `style` is empty *and* the text itself already contains ANSI
/// escapes (e.g. `kb.pretty_print_sentence` output for the REFERENCES
/// section), the span is treated as pre-rendered: the renderer prints
/// it verbatim and only injects an inverse-video overlay if it's the
/// focused link.  See [`pre_styled`].
#[derive(Default, Clone)]
struct Span {
    text:  String,
    style: String,
    link:  Option<usize>,
}

#[derive(Default, Clone)]
struct LineRow {
    spans: Vec<Span>,
}

#[derive(Clone)]
struct Link {
    target: String,
    line:   usize,
}

#[derive(Default, Clone)]
struct Document {
    lines: Vec<LineRow>,
    links: Vec<Link>,
}

impl Document {
    fn push_blank(&mut self) {
        self.lines.push(LineRow::default());
    }

    fn push_line(&mut self, spans: Vec<Span>) {
        self.lines.push(LineRow { spans });
    }

    fn push_header(&mut self, title: &str) {
        self.push_blank();
        self.push_line(vec![styled(title, "\x1b[1m")]);
    }
}

fn plain<S: Into<String>>(text: S) -> Span {
    Span { text: text.into(), style: String::new(), link: None }
}
fn styled<S: Into<String>>(text: S, ansi: &str) -> Span {
    Span { text: text.into(), style: ansi.to_string(), link: None }
}
fn yellow<S: Into<String>>(text: S) -> Span { styled(text, "\x1b[33m") }
fn cyan<S: Into<String>>(text: S)   -> Span { styled(text, "\x1b[36m") }
fn blue<S: Into<String>>(text: S)   -> Span { styled(text, "\x1b[94m") }
fn dim<S: Into<String>>(text: S)    -> Span { styled(text, "\x1b[90m") }

/// Span for text that already contains its own ANSI escapes (e.g.
/// `kb.pretty_print_sentence` output).  Forces a trailing reset so
/// styling can't bleed into the next line.
fn pre_styled<S: Into<String>>(text: S) -> Span {
    let mut t = text.into();
    if !t.ends_with("\x1b[0m") {
        t.push_str("\x1b[0m");
    }
    Span { text: t, style: String::new(), link: None }
}

/// A linked, underlined symbol label.  Yellow + underline; the focus
/// overlay (inverse video) is added by the renderer.
fn link_span<S: Into<String>>(text: S, link_idx: usize) -> Span {
    Span {
        text:  text.into(),
        style: "\x1b[33m\x1b[4m".to_string(),
        link:  Some(link_idx),
    }
}

// ---------------------------------------------------------------------------
// Build a Document from a ManPage
// ---------------------------------------------------------------------------

fn build_document(kb: &KnowledgeBase, man: &ManPage, lang_filter: Option<&str>) -> Document {
    let mut doc = Document::default();

    // NAME
    doc.push_header("NAME");
    let kinds = if man.kinds.is_empty() {
        String::from("(uncategorised)")
    } else {
        man.kinds.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ")
    };
    doc.push_line(vec![
        plain("    "),
        yellow(man.name.clone()),
        plain("  "),
        dim(format!("({})", kinds)),
    ]);

    // PARENTS
    if !man.parents.is_empty() {
        doc.push_header("PARENTS");
        let width = man.parents.iter().map(|p| p.relation.len()).max().unwrap_or(0);
        for p in &man.parents {
            let link_idx = doc.links.len();
            doc.links.push(Link { target: p.parent.clone(), line: doc.lines.len() });
            doc.push_line(vec![
                plain("    "),
                cyan(format!("{:<width$}", p.relation, width = width)),
                plain("  "),
                blue("→"),
                plain("  "),
                link_span(p.parent.clone(), link_idx),
            ]);
        }
    }

    // SIGNATURE (arity / domains / range)
    let has_sig = man.arity.is_some() || !man.domains.is_empty() || man.range.is_some();
    if has_sig {
        doc.push_header("SIGNATURE");
        if let Some(a) = man.arity {
            let rendered = if a < 0 { "variable".to_string() } else { a.to_string() };
            doc.push_line(vec![
                plain("    "),
                dim("arity:"),
                plain("  "),
                plain(rendered),
            ]);
        }
        for (pos, sig) in &man.domains {
            doc.push_line(sig_line(&format!("arg{}:", pos), sig));
        }
        if let Some(sig) = &man.range {
            doc.push_line(sig_line("range:", sig));
        }
    }

    // DOCUMENTATION (with cross-ref parsing)
    let docs = filter_lang(&man.documentation, lang_filter);
    if !docs.is_empty() {
        doc.push_header("DOCUMENTATION");
        for d in &docs {
            doc.push_line(vec![dim(format!("    [{}]", d.language))]);
            for line in wrap_text(&d.text, 72) {
                let spans = parse_cross_refs(&line, &mut doc.links, doc.lines.len());
                let mut prefixed = vec![plain("    ")];
                prefixed.extend(spans);
                doc.push_line(prefixed);
            }
            doc.push_blank();
        }
    }

    // TERM FORMAT
    let tfs = filter_lang(&man.term_format, lang_filter);
    if !tfs.is_empty() {
        doc.push_header("TERM FORMAT");
        for t in &tfs {
            let mut spans = vec![
                plain("    "),
                dim(format!("[{}]", t.language)),
                plain("  "),
            ];
            spans.extend(parse_cross_refs(&t.text, &mut doc.links, doc.lines.len()));
            doc.push_line(spans);
        }
    }

    // FORMAT
    let fmts = filter_lang(&man.format, lang_filter);
    if !fmts.is_empty() {
        doc.push_header("FORMAT");
        for f in &fmts {
            let mut spans = vec![
                plain("    "),
                dim(format!("[{}]", f.language)),
                plain("  "),
            ];
            spans.extend(parse_cross_refs(&f.text, &mut doc.links, doc.lines.len()));
            doc.push_line(spans);
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
                doc.push_header(&label);
                for sid in pos.iter() {
                    push_sentence_block(&mut doc, kb, *sid);
                }
            }
        }
        if !man.ref_nested.is_empty() {
            let mut nested = man.ref_nested.clone();
            sort_sids_by_source(&mut nested, kb);
            doc.push_header("Appearance nested inside other axioms");
            for sid in &nested {
                push_sentence_block(&mut doc, kb, *sid);
            }
        }
    }

    // Trailing blank to mirror the legacy man(1) layout.
    doc.push_blank();
    doc
}

fn sig_line(label: &str, sig: &SortSig) -> Vec<Span> {
    let mut spans = vec![
        plain("    "),
        dim(label.to_string()),
        plain("  "),
        yellow(sig.class.clone()),
    ];
    if sig.subclass {
        spans.push(plain(" "));
        spans.push(dim("(subclass-of)"));
    }
    spans
}

/// Render one reference entry: the source `file:line` header (dim
/// grey) followed by the pretty-printed sentence indented four
/// spaces, matching the surrounding man-page layout.
fn push_sentence_block(doc: &mut Document, kb: &KnowledgeBase, sid: SentenceId) {
    let Some(sent) = kb.sentence(sid) else { return };
    let trace = format!("{}:{}", sent.span.file, sent.span.line);
    doc.push_line(vec![dim(format!("    {}", trace))]);
    // `pretty_print_sentence` returns ANSI-coloured multi-line KIF
    // when the sentence is wide enough to break.  Each line goes in
    // verbatim via `pre_styled` so the embedded escapes survive.
    let pretty = kb.pretty_print_sentence(sid, 4);
    for line in pretty.lines() {
        doc.push_line(vec![plain("    "), pre_styled(line.to_string())]);
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

/// Scan `text` for `&%Symbol` tokens.  Each match becomes an
/// underlined link span carrying `Symbol` (the `&%` marker is stripped
/// from the visible text); other characters become plain spans.  New
/// links are appended to `links` with `line = line_idx` (the line this
/// span is about to land on).
fn parse_cross_refs(text: &str, links: &mut Vec<Link>, line_idx: usize) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0usize;
    let mut i     = 0usize;

    while i < bytes.len() {
        if i + 2 < bytes.len() && bytes[i] == b'&' && bytes[i + 1] == b'%' {
            let sym_start = i + 2;
            let mut sym_end = sym_start;
            while sym_end < bytes.len() {
                let c = bytes[sym_end];
                if c.is_ascii_alphanumeric() || c == b'_' { sym_end += 1; } else { break; }
            }
            if sym_end > sym_start {
                if i > start {
                    spans.push(plain(text[start..i].to_string()));
                }
                let target: String = text[sym_start..sym_end].to_string();
                let link_idx = links.len();
                links.push(Link { target: target.clone(), line: line_idx });
                spans.push(link_span(target, link_idx));
                i = sym_end;
                start = sym_end;
                continue;
            }
        }
        i += 1;
    }
    if start < bytes.len() {
        spans.push(plain(text[start..].to_string()));
    }
    spans
}

// ---------------------------------------------------------------------------
// Plain (pipe / --no-pager) rendering
// ---------------------------------------------------------------------------

/// Print the document with all original ANSI styling preserved.  Used
/// when the interactive viewer is bypassed (pipe / `--no-pager` /
/// `NO_PAGER`).  Styling stays so that `sumo man Foo | less -R` still
/// renders colour.
fn print_document_plain(doc: &Document) {
    let mut stdout = io::stdout().lock();
    for row in &doc.lines {
        for span in &row.spans {
            if span.style.is_empty() {
                let _ = write!(stdout, "{}", span.text);
            } else {
                let _ = write!(stdout, "{}{}\x1b[0m", span.style, span.text);
            }
        }
        let _ = writeln!(stdout);
    }
}

// ---------------------------------------------------------------------------
// Interactive viewer
// ---------------------------------------------------------------------------

enum ViewAction {
    Quit,
    Follow(String),
    Back,
}

fn interactive_view(
    kb:          &KnowledgeBase,
    initial:     ManPage,
    lang_filter: Option<&str>,
) -> io::Result<()> {
    let mut history: Vec<String> = Vec::new();
    let mut current             = initial;

    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, cursor::Hide)?;

    let result = (|| -> io::Result<()> {
        loop {
            let doc = build_document(kb, &current, lang_filter);
            let action = view_loop(&mut stdout, &doc, history.len(), &current.name)?;
            match action {
                ViewAction::Quit => return Ok(()),
                ViewAction::Follow(target) => {
                    if let Some(next) = kb.manpage(&target) {
                        history.push(current.name.clone());
                        current = next;
                    }
                    // No-op if the symbol isn't resolvable; the status
                    // bar will redraw the current page on the next pass.
                }
                ViewAction::Back => {
                    if let Some(prev_name) = history.pop() {
                        if let Some(prev) = kb.manpage(&prev_name) {
                            current = prev;
                        }
                    }
                }
            }
        }
    })();

    let _ = execute!(stdout, cursor::Show, LeaveAlternateScreen);
    let _ = disable_raw_mode();
    result
}

fn view_loop<W: Write>(
    stdout:        &mut W,
    doc:           &Document,
    history_depth: usize,
    current_name:  &str,
) -> io::Result<ViewAction> {
    let mut focused: Option<usize> = if doc.links.is_empty() { None } else { Some(0) };
    let mut scroll: usize          = 0;

    loop {
        let (cols, rows) = size()?;
        let body_rows    = (rows as usize).saturating_sub(1).max(1);
        ensure_focused_visible(focused, doc, &mut scroll, body_rows);
        let max_scroll = doc.lines.len().saturating_sub(body_rows);
        if scroll > max_scroll { scroll = max_scroll; }
        render(stdout, doc, focused, scroll, body_rows, cols as usize, history_depth, current_name)?;

        match event::read()? {
            Event::Key(k) => {
                let KeyEvent { code, modifiers, .. } = k;
                match (code, modifiers) {
                    (KeyCode::Char('q'), _)
                    | (KeyCode::Esc,     _) => return Ok(ViewAction::Quit),
                    (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                        return Ok(ViewAction::Quit);
                    }
                    (KeyCode::Tab, _) | (KeyCode::Right, _) => {
                        if !doc.links.is_empty() {
                            let n = doc.links.len();
                            focused = Some(focused.map_or(0, |i| (i + 1) % n));
                        }
                    }
                    (KeyCode::BackTab, _) | (KeyCode::Left, _) => {
                        if !doc.links.is_empty() {
                            let n = doc.links.len();
                            focused = Some(focused.map_or(n - 1, |i| (i + n - 1) % n));
                        }
                    }
                    (KeyCode::Enter, _) | (KeyCode::Char('p'), _) => {
                        if let Some(i) = focused {
                            return Ok(ViewAction::Follow(doc.links[i].target.clone()));
                        }
                    }
                    (KeyCode::Backspace, _) | (KeyCode::Char('b'), _) => {
                        if history_depth > 0 {
                            return Ok(ViewAction::Back);
                        }
                    }
                    (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                        if scroll < max_scroll { scroll += 1; }
                    }
                    (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                        if scroll > 0 { scroll -= 1; }
                    }
                    (KeyCode::PageDown, _) | (KeyCode::Char(' '), _) => {
                        scroll = (scroll + body_rows.saturating_sub(1)).min(max_scroll);
                    }
                    (KeyCode::PageUp, _) => {
                        scroll = scroll.saturating_sub(body_rows.saturating_sub(1));
                    }
                    (KeyCode::Home, _) | (KeyCode::Char('g'), _) => { scroll = 0; }
                    (KeyCode::End,  _) | (KeyCode::Char('G'), _) => { scroll = max_scroll; }
                    _ => {}
                }
            }
            Event::Resize(_, _) => { /* re-render on next loop */ }
            _ => {}
        }
    }
}

fn ensure_focused_visible(
    focused:   Option<usize>,
    doc:       &Document,
    scroll:    &mut usize,
    body_rows: usize,
) {
    let Some(f) = focused else { return };
    let Some(link) = doc.links.get(f) else { return };
    let line = link.line;
    if line < *scroll {
        *scroll = line;
    } else if body_rows > 0 && line >= *scroll + body_rows {
        *scroll = line + 1 - body_rows;
    }
}

fn render<W: Write>(
    stdout:        &mut W,
    doc:           &Document,
    focused:       Option<usize>,
    scroll:        usize,
    body_rows:     usize,
    cols:          usize,
    history_depth: usize,
    current_name:  &str,
) -> io::Result<()> {
    queue!(stdout, Clear(ClearType::All), cursor::MoveTo(0, 0))?;

    for screen_row in 0..body_rows {
        let line_idx = scroll + screen_row;
        queue!(stdout, cursor::MoveTo(0, screen_row as u16))?;
        if line_idx >= doc.lines.len() { continue; }
        for span in &doc.lines[line_idx].spans {
            let is_focused = matches!((focused, span.link), (Some(f), Some(s)) if f == s);
            write_span(stdout, span, is_focused)?;
        }
    }

    // Status bar (reserved bottom row, inverse-video).
    let n_links = doc.links.len();
    let focus_s = focused
        .map(|f| format!("{}/{}", f + 1, n_links))
        .unwrap_or_else(|| "-/-".to_string());
    let raw_status = format!(
        " {}  links {}  hist {}  [tab cycle · enter follow · b back · q quit] ",
        current_name, focus_s, history_depth,
    );
    let status = if raw_status.chars().count() > cols {
        raw_status.chars().take(cols).collect::<String>()
    } else {
        raw_status
    };
    queue!(stdout, cursor::MoveTo(0, body_rows as u16))?;
    write!(stdout, "\x1b[7m{}\x1b[0m", status)?;

    stdout.flush()?;
    Ok(())
}

fn write_span<W: Write>(stdout: &mut W, span: &Span, focused: bool) -> io::Result<()> {
    if focused {
        // Focus gets inverse-video stacked on top of the underlying
        // style so links remain underlined while highlighted.
        write!(stdout, "{}\x1b[7m{}\x1b[0m", span.style, span.text)
    } else if span.style.is_empty() {
        write!(stdout, "{}", span.text)
    } else {
        write!(stdout, "{}{}\x1b[0m", span.style, span.text)
    }
}

// ---------------------------------------------------------------------------
// Helpers (unchanged from the legacy implementation)
// ---------------------------------------------------------------------------

fn filter_lang<'a>(entries: &'a [DocEntry], want: Option<&str>) -> Vec<&'a DocEntry> {
    match want {
        None    => entries.iter().collect(),
        Some(l) => entries.iter().filter(|e| e.language == l).collect(),
    }
}

/// Soft-wrap `text` at `width` columns, splitting on spaces.  The KIF
/// `documentation` convention sometimes inlines `&%Symbol` cross-refs;
/// `parse_cross_refs` is responsible for rewriting those after wrap.
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cross_refs_extracts_marker() {
        let mut links = Vec::new();
        let spans = parse_cross_refs("see &%Animal and &%Plant_Tissue.", &mut links, 0);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].target, "Animal");
        assert_eq!(links[1].target, "Plant_Tissue");
        // 5 spans: "see ", link, " and ", link, "."
        assert_eq!(spans.len(), 5);
        assert_eq!(spans[1].text, "Animal");
        assert_eq!(spans[3].text, "Plant_Tissue");
        assert!(spans[1].link.is_some());
        assert!(spans[3].link.is_some());
        // Underline ANSI must be present in the link style.
        assert!(spans[1].style.contains("\x1b[4m"));
    }

    #[test]
    fn parse_cross_refs_handles_apostrophe_terminator() {
        let mut links = Vec::new();
        let spans = parse_cross_refs("&%dog's tail", &mut links, 0);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "dog");
        // dog | 's tail
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].text, "dog");
        assert_eq!(spans[1].text, "'s tail");
    }

    #[test]
    fn parse_cross_refs_no_match_returns_plain() {
        let mut links = Vec::new();
        let spans = parse_cross_refs("no markers here", &mut links, 0);
        assert!(links.is_empty());
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "no markers here");
        assert!(spans[0].link.is_none());
    }

    #[test]
    fn parse_cross_refs_lone_marker_is_literal() {
        // `&%` not followed by an identifier byte stays as plain text.
        let mut links = Vec::new();
        let spans = parse_cross_refs("&% bare", &mut links, 0);
        assert!(links.is_empty());
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "&% bare");
    }

    #[test]
    fn pre_styled_appends_reset_when_missing() {
        let s = pre_styled("\x1b[33mhello");
        assert!(s.text.ends_with("\x1b[0m"));
        let t = pre_styled("\x1b[33mhello\x1b[0m");
        // No double reset.
        assert!(!t.text.ends_with("\x1b[0m\x1b[0m"));
    }
}
