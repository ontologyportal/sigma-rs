//! `sumo man <symbol>` -- manpage-style introspection.
//!
//! Opens whatever KB the shared KbArgs point to (LMDB + any `-f/-d`
//! layers), then renders the `ManPage` returned by
//! `KnowledgeBase::manpage` into an interactive viewer where Tab cycles
//! through every link on the page, Enter follows the focused link,
//! Backspace pops the navigation history, and `q` quits.
//!
//! The viewer is bypassed when stdout is not a TTY, when `--no-pager` is
//! passed, or when the `NO_PAGER` environment variable is set.

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

use sigmakee_rs_sdk::{DocEntry, KnowledgeBase, ManPage, SentenceId, SortSig, TranslationLayer, TptpLang};
use sigmakee_rs_sdk::Session;
use sigmakee_rs_sdk::manager::KBManager;

/// How reference / antecedent formulas are rendered. Toggled with `t`
/// in the interactive viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormulaMode {
    /// Pretty-printed SUO-KIF (default).
    Kif,
    /// Cached TPTP (TFF). Suppressed sentences show their synthetic
    /// replacement(s) instead, tagged `(synthetic)`.
    Tptp,
}

pub fn run_man(
    mut session: Session<TranslationLayer>,
    _manager:    KBManager,
    symbol:      String,
    lang:        Option<String>,
    no_pager:    bool,
) -> bool {
    session.kb_mut().ensure_introspection();
    let kb = session.kb();

    let Some(man) = kb.manpage(&symbol) else {
        log::error!("symbol '{}' not found in the knowledge base", symbol);
        return false;
    };

    let tty       = io::stdout().is_terminal();
    let env_off   = std::env::var_os("NO_PAGER").is_some();
    let use_pager = !no_pager && !crate::style::is_ugly() && !env_off && tty;

    if use_pager {
        match interactive_view(kb,man.clone(), lang.as_deref()) {
            Ok(()) => true,
            Err(e) => {
                log::warn!("manpage viewer failed ({}); falling back to stdout", e);
                let doc = build_document(kb,&man, lang.as_deref(), FormulaMode::Kif);
                print_document_plain(&doc);
                true
            }
        }
    } else {
        let doc = build_document(kb,&man, lang.as_deref(), FormulaMode::Kif);
        print_document_plain(&doc);
        true
    }
}

// ---------------------------------------------------------------------------
// Document model
// ---------------------------------------------------------------------------

/// A single styled run on a line. `style` is a pre-built ANSI prefix
/// (e.g. `"\x1b[33m\x1b[4m"`); the renderer always emits a `\x1b[0m`
/// reset after the text. `link` is `Some(idx)` if this span is the
/// visible label of a navigable cross-ref, and points into
/// `Document.links`.
///
/// When `style` is empty and the text itself already contains ANSI
/// escapes, the span is treated as pre-rendered: the renderer prints it
/// verbatim and only injects an inverse-video overlay if it's the
/// focused link. See [`pre_styled`].
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

/// Span for text that already contains its own ANSI escapes. Forces a
/// trailing reset so styling can't bleed into the next line.
fn pre_styled<S: Into<String>>(text: S) -> Span {
    let mut t = text.into();
    if !t.ends_with("\x1b[0m") {
        t.push_str("\x1b[0m");
    }
    Span { text: t, style: String::new(), link: None }
}

/// A linked, underlined symbol label. Yellow + underline; the focus
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

fn build_document(
    kb:          &KnowledgeBase,
    man:         &ManPage,
    lang_filter: Option<&str>,
    mode:        FormulaMode,
) -> Document {
    let mut doc = Document::default();
    let src_idx = kb.build_axiom_source_index();

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

    // OCCURRENCES
    doc.push_header("OCCURRENCES");
    doc.push_line(vec![
        plain("    "),
        dim("appears in:"),
        plain("  "),
        yellow(format!("{}", man.appears_in_count)),
        plain(" formula(s)"),
    ]);
    doc.push_line(vec![
        plain("    "),
        dim("antecedent of:"),
        plain("  "),
        yellow(format!("{}", man.antecedent_refs.len())),
        plain("   "),
        dim("consequent of:"),
        plain("  "),
        yellow(format!("{}", man.consequent_count)),
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

    // CHILDREN — inverse taxonomy edges (`(rel Child sym)`). Capped so a
    // class with thousands of subclasses stays tab-navigable; the tail
    // count is noted instead.
    if !man.children.is_empty() {
        const CHILD_CAP: usize = 50;
        let total = man.children.len();
        doc.push_header(&format!("CHILDREN ({})", total));
        let width = man.children.iter().take(CHILD_CAP)
            .map(|c| c.relation.len()).max().unwrap_or(0);
        for c in man.children.iter().take(CHILD_CAP) {
            let link_idx = doc.links.len();
            doc.links.push(Link { target: c.parent.clone(), line: doc.lines.len() });
            doc.push_line(vec![
                plain("    "),
                cyan(format!("{:<width$}", c.relation, width = width)),
                plain("  "),
                blue("←"),
                plain("  "),
                link_span(c.parent.clone(), link_idx),
            ]);
        }
        if total > CHILD_CAP {
            doc.push_line(vec![plain("    "), dim(format!("… and {} more", total - CHILD_CAP))]);
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

    // ANTECEDENT — formulas in which the symbol appears in the
    // antecedent of a (normalized) implication.
    if !man.antecedent_refs.is_empty() {
        doc.push_header(&format!("APPEARS IN ANTECEDENT ({})", man.antecedent_refs.len()));
        let mut refs = man.antecedent_refs.clone();
        sort_sids_by_source(&mut refs, &src_idx);
        for sid in &refs {
            push_sentence_block(&mut doc, kb, &src_idx, *sid, man, mode);
        }
    }

    // REFERENCES
    //
    // Group root-level occurrences by position — position 0 is the head
    // slot, position 1.. are argument slots. Variable-arity relations can
    // land at any position, so the bucket vector is sized from the data.
    if !man.ref_args.is_empty() || !man.ref_nested.is_empty() {
        if !man.ref_args.is_empty() {
            let max_pos = man.ref_args.iter().map(|r| r.0).max().unwrap_or(0);
            let mut buckets: Vec<Vec<SentenceId>> = vec![Vec::new(); max_pos + 1];
            for r in &man.ref_args {
                buckets[r.0].push(r.1);
            }
            for (i, pos) in buckets.iter_mut().enumerate() {
                if pos.is_empty() { continue; }
                sort_sids_by_source(pos, &src_idx);
                let label = if i == 0 {
                    String::from("Appearance as head")
                } else {
                    format!("Appearance as argument number {}", i)
                };
                doc.push_header(&label);
                for sid in pos.iter() {
                    push_sentence_block(&mut doc, kb, &src_idx, *sid, man, mode);
                }
            }
        }
        if !man.ref_nested.is_empty() {
            let mut nested = man.ref_nested.clone();
            sort_sids_by_source(&mut nested, &src_idx);
            doc.push_header("Appearance nested inside other axioms");
            for sid in &nested {
                push_sentence_block(&mut doc, kb, &src_idx, *sid, man, mode);
            }
        }
    }

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

/// Render one reference entry: the source `file:line` header (dim grey)
/// followed by the pretty-printed sentence indented four spaces.
fn push_sentence_block(
    doc:     &mut Document,
    kb:      &KnowledgeBase,
    src_idx: &sigmakee_rs_sdk::AxiomSourceIndex,
    sid:     SentenceId,
    man:     &ManPage,
    mode:    FormulaMode,
) {
    if kb.sentence(sid).is_none() { return; }
    let trace = src_idx
        .lookup_by_sid(sid)
        .map(|s| format!("{}:{}", s.file, s.line))
        .unwrap_or_else(|| format!("sid {:x}", sid));
    // Source trace, with a SInE-ownership marker when this symbol is the
    // formula's SInE trigger.
    let mut header = vec![dim(format!("    {}", trace))];
    if man.owned_sids.contains(&sid) {
        header.push(plain("  "));
        header.push(cyan("⊙ SInE-owned"));
    }
    doc.push_line(header);

    match mode {
        FormulaMode::Kif => {
            let pretty = kb.pretty_print_sentence(sid, 4);
            for line in pretty.lines() {
                doc.push_line(vec![plain("    "), pre_styled(line.to_string())]);
            }
        }
        FormulaMode::Tptp => push_tptp(doc, kb, sid),
    }
}

/// Render a sentence's cached TPTP (TFF). When the sentence was
/// suppressed by the rewrite pass, show its synthetic replacement(s)
/// instead, each tagged `(synthetic)`.
fn push_tptp(doc: &mut Document, kb: &KnowledgeBase, sid: SentenceId) {
    if let Some(tptp) = kb.sentence_tptp(sid, TptpLang::Tff) {
        doc.push_line(vec![plain("    "), yellow(tptp)]);
        return;
    }
    if kb.is_suppressed(sid) {
        let syns = kb.synthetic_replacements_of(sid);
        let mut shown = false;
        for s in syns {
            if let Some(tptp) = kb.sentence_tptp(s, TptpLang::Tff) {
                doc.push_line(vec![
                    plain("    "),
                    yellow(tptp),
                    plain("  "),
                    dim("(synthetic)"),
                ]);
                shown = true;
            }
        }
        if !shown {
            doc.push_line(vec![plain("    "), dim("(suppressed; no emittable synthetic)")]);
        }
        return;
    }
    doc.push_line(vec![plain("    "), dim("(no TPTP — not convertible)")]);
}

/// Sort a list of sentence ids by (file, line) for stable output.
fn sort_sids_by_source(sids: &mut Vec<SentenceId>, src_idx: &sigmakee_rs_sdk::AxiomSourceIndex) {
    sids.sort_by(|a, b| {
        match (src_idx.lookup_by_sid(*a), src_idx.lookup_by_sid(*b)) {
            (Some(x), Some(y)) => (x.file.as_str(), x.line).cmp(&(y.file.as_str(), y.line)),
            _ => a.cmp(b),
        }
    });
}

/// Scan `text` for `&%Symbol` tokens. Each match becomes an underlined
/// link span carrying `Symbol` (the `&%` marker is stripped from the
/// visible text); other characters become plain spans. New links are
/// appended to `links` with `line = line_idx`.
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

/// Print the document with all original ANSI styling preserved. Used
/// when the interactive viewer is bypassed (pipe / `--no-pager` /
/// `NO_PAGER`), so `sumo man Foo | less -R` still renders colour.
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
    ToggleFormula,
}

fn interactive_view(
    kb:          &KnowledgeBase,
    initial:     ManPage,
    lang_filter: Option<&str>,
) -> io::Result<()> {
    let mut history: Vec<String> = Vec::new();
    let mut current             = initial;
    let mut mode                = FormulaMode::Kif;

    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, cursor::Hide)?;

    let result = (|| -> io::Result<()> {
        loop {
            let doc = build_document(kb, &current, lang_filter, mode);
            let action = view_loop(&mut stdout, &doc, history.len(), &current.name, mode)?;
            match action {
                ViewAction::Quit => return Ok(()),
                ViewAction::Follow(target) => {
                    if let Some(next) = kb.manpage(&target) {
                        history.push(current.name.clone());
                        current = next;
                    }
                    // No-op if the symbol isn't resolvable.
                }
                ViewAction::Back => {
                    if let Some(prev_name) = history.pop() {
                        if let Some(prev) = kb.manpage(&prev_name) {
                            current = prev;
                        }
                    }
                }
                ViewAction::ToggleFormula => {
                    mode = match mode {
                        FormulaMode::Kif  => FormulaMode::Tptp,
                        FormulaMode::Tptp => FormulaMode::Kif,
                    };
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
    mode:          FormulaMode,
) -> io::Result<ViewAction> {
    let mut focused: Option<usize> = if doc.links.is_empty() { None } else { Some(0) };
    let mut scroll: usize          = 0;
    // Only snap the viewport to the focused link when focus moves
    // (Tab/BackTab); otherwise manual scrolling would be yanked back
    // every frame, making the bottom of a long page unreachable.
    let mut scroll_to_focus        = true;

    loop {
        let (cols, rows) = size()?;
        let body_rows    = (rows as usize).saturating_sub(1).max(1);
        if scroll_to_focus {
            ensure_focused_visible(focused, doc, &mut scroll, body_rows);
            scroll_to_focus = false;
        }
        let max_scroll = doc.lines.len().saturating_sub(body_rows);
        if scroll > max_scroll { scroll = max_scroll; }
        render(stdout, doc, focused, scroll, body_rows, cols as usize, history_depth, current_name, mode)?;

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
                            scroll_to_focus = true;
                        }
                    }
                    (KeyCode::BackTab, _) | (KeyCode::Left, _) => {
                        if !doc.links.is_empty() {
                            let n = doc.links.len();
                            focused = Some(focused.map_or(n - 1, |i| (i + n - 1) % n));
                            scroll_to_focus = true;
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
                    (KeyCode::Char('t'), _) => {
                        return Ok(ViewAction::ToggleFormula);
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
            Event::Resize(_, _) => {}
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
    mode:          FormulaMode,
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
    let mode_s = match mode { FormulaMode::Kif => "kif", FormulaMode::Tptp => "tptp" };
    let raw_status = format!(
        " {}  links {}  hist {}  fmt {}  [tab cycle · enter follow · b back · t kif/tptp · q quit] ",
        current_name, focus_s, history_depth, mode_s,
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
        // Inverse-video stacked on the underlying style so links stay
        // underlined while highlighted.
        write!(stdout, "{}\x1b[7m{}\x1b[0m", span.style, span.text)
    } else if span.style.is_empty() {
        write!(stdout, "{}", span.text)
    } else {
        write!(stdout, "{}{}\x1b[0m", span.style, span.text)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn filter_lang<'a>(entries: &'a [DocEntry], want: Option<&str>) -> Vec<&'a DocEntry> {
    match want {
        None    => entries.iter().collect(),
        Some(l) => entries.iter().filter(|e| e.language == l).collect(),
    }
}

/// Soft-wrap `text` at `width` columns, splitting on spaces. Inlined
/// `&%Symbol` cross-refs are rewritten separately by `parse_cross_refs`
/// after wrapping.
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
