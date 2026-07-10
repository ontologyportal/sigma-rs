// crates/cli/src/cli/ask_tui.rs
//
// `sumo ask -i`: an interactive, vim-lite editor for building up a set of
// hypotheses (the `-t`/`--tell` equivalent) and a conjecture, then asking
// the KB without leaving the terminal. Unlike a plain REPL, previously
// entered formulas stay on screen as editable cells — move the cursor up
// to an earlier formula and change it, the same way `sumo config`'s
// interactive editor keeps every row visible and navigable rather than
// prompting one field at a time.
//
// Terminal setup/teardown follows the same raw-mode/alternate-screen idiom
// as `config_tui.rs`: enable, guaranteed cleanup (even on error/panic
// unwind), gated on a real tty + `!is_ugly()`, checked by the caller
// (`dispatch`) before this runs.
//
// Errors (tell/ask/parse failures surfaced by the SDK) are shown as a
// dismissable overlay for now. The SDK errors already carry `Diagnostic`s
// (`SdkError::Kb(Diagnostic)`) — a future pass can render those with spans
// against the offending formula cell instead of flattening them to text.

use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Terminal;

use sigmakee_rs_sdk::{AstKif, ProverStatus, ProverResult, ProvingLayer, Session};
use sigmakee_rs_sdk::manager::{KBManager, ProverOptsFor};

/// Entry point for `sumo ask -i`. Returns whether the session left the
/// editor in a state that should be treated as CLI success (mirrors
/// `run_ask`: `true` iff the *last* ask verdict was `Proved`, `false` if
/// the editor was quit without ever asking or the terminal couldn't be
/// set up).
pub fn run_ask_tui<L>(mut session: Session<L>, manager: &KBManager) -> bool
where
    L: ProvingLayer,
    L::Opts: ProverOptsFor,
{
    // The indicatif-backed phase spinner (installed by `dispatch` for
    // constituent ingestion) draws by tracking cursor position on whatever
    // screen buffer is currently active, via a background thread that
    // redraws on its own timer independent of whatever else is happening.
    // Ratatui's alternate screen is a *different* buffer it knows nothing
    // about, so if the spinner is still ticking (or its bar just hasn't
    // been cleared yet) when we switch buffers, its next redraw stomps on
    // our frame instead of the terminal state it was tracking. Force it
    // closed before touching the terminal, then swap in a silent sink so
    // no further events (e.g. from `ask()` calls made inside the editor)
    // spin one back up mid-session.
    if let Some(sink) = crate::progress::global() {
        sink.finish();
    }
    session.set_progress_sink(std::sync::Arc::new(|_: &sigmakee_rs_sdk::ProgressEvent| {}));

    match enter_and_run(&mut session, manager) {
        Ok(proved) => proved,
        Err(e) => { log::error!("ask -i: interactive editor failed: {e}"); false }
    }
}

fn enter_and_run<L>(session: &mut Session<L>, manager: &KBManager) -> io::Result<bool>
where
    L: ProvingLayer,
    L::Opts: ProverOptsFor,
{
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    let setup = execute!(stdout, EnterAlternateScreen);
    let result = match setup {
        Ok(()) => {
            let backend = CrosstermBackend::new(io::stdout());
            let mut terminal = Terminal::new(backend)?;
            run_app(&mut terminal, session, manager)
        }
        Err(e) => Err(e),
    };
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
    let _ = disable_raw_mode();
    result
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode { Normal, Insert }

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus { Formula(usize), Query }

enum Overlay { Error(String), Result(String) }

/// A single-line editable text cell. Cursor is a char index (not byte
/// offset), so movement/insert/delete stay correct on multibyte input.
#[derive(Default)]
struct Buffer {
    chars:  Vec<char>,
    cursor: usize,
}

impl Buffer {
    fn text(&self) -> String { self.chars.iter().collect() }
    fn insert(&mut self, c: char) { self.chars.insert(self.cursor, c); self.cursor += 1; }
    fn backspace(&mut self) { if self.cursor > 0 { self.cursor -= 1; self.chars.remove(self.cursor); } }
    fn left(&mut self)  { self.cursor = self.cursor.saturating_sub(1); }
    fn right(&mut self) { self.cursor = (self.cursor + 1).min(self.chars.len()); }
    fn end(&mut self)   { self.cursor = self.chars.len(); }
}

struct App {
    formulas: Vec<Buffer>,
    query:    Buffer,
    focus:    Focus,
    mode:     Mode,
    overlay:  Option<Overlay>,
}

impl App {
    fn new() -> Self {
        Self {
            formulas: vec![Buffer::default()],
            query:    Buffer::default(),
            focus:    Focus::Formula(0),
            mode:     Mode::Normal,
            overlay:  None,
        }
    }

    fn current_buf_mut(&mut self) -> &mut Buffer {
        match self.focus {
            Focus::Formula(i) => &mut self.formulas[i],
            Focus::Query      => &mut self.query,
        }
    }

    fn move_focus(&mut self, delta: isize) {
        let last = self.formulas.len();
        let cur = match self.focus {
            Focus::Formula(i) => i as isize,
            Focus::Query      => last as isize,
        };
        let next = (cur + delta).clamp(0, last as isize);
        self.focus = if next as usize == last { Focus::Query } else { Focus::Formula(next as usize) };
    }

    /// Insert a fresh formula cell right after the focused one (or at the
    /// end, from the query box) and move focus onto it.
    fn new_formula_after_focus(&mut self) {
        let at = match self.focus {
            Focus::Formula(i) => i + 1,
            Focus::Query      => self.formulas.len(),
        };
        self.formulas.insert(at, Buffer::default());
        self.focus = Focus::Formula(at);
    }

    fn delete_focused_formula(&mut self) {
        let Focus::Formula(i) = self.focus else { return };
        self.formulas.remove(i);
        if self.formulas.is_empty() {
            self.formulas.push(Buffer::default());
        }
        self.focus = Focus::Formula(i.min(self.formulas.len() - 1));
    }

    fn run_ask<L>(&mut self, session: &mut Session<L>, manager: &KBManager)
    where
        L: ProvingLayer,
        L::Opts: ProverOptsFor,
    {
        let conjecture = self.query.text();
        if conjecture.trim().is_empty() {
            self.overlay = Some(Overlay::Error("query box is empty — nothing to ask".to_string()));
            return;
        }
        let tells: Vec<String> = self.formulas.iter()
            .map(Buffer::text)
            .filter(|s| !s.trim().is_empty())
            .collect();

        let opts = <L::Opts as ProverOptsFor>::from_manager(manager);
        let open = tells.iter().try_fold(session.open_session(), |s, t| s.tell(t));
        let open = match open {
            Ok(o) => o,
            Err(errs) => {
                self.overlay = Some(Overlay::Error(join_errors("tell error", &errs)));
                return;
            }
        };
        match open.ask(&conjecture, Some(opts)) {
            Ok(result) => self.overlay = Some(Overlay::Result(render_result(&conjecture, &result))),
            Err(errs)  => self.overlay = Some(Overlay::Error(join_errors("ask error", &errs))),
        }
    }
}

fn join_errors<E: std::fmt::Display>(label: &str, errs: &[E]) -> String {
    let body = errs.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n");
    format!("{label}:\n{body}")
}

fn render_result(conjecture: &str, result: &ProverResult) -> String {
    let verdict = match result.status {
        ProverStatus::Proved       => "Proved",
        ProverStatus::Disproved    => "Disproved",
        ProverStatus::Consistent   => "Consistent",
        ProverStatus::Inconsistent => "Inconsistent",
        ProverStatus::Timeout      => "Timeout",
        ProverStatus::InputError   => "Input Error",
        ProverStatus::Unknown      => "Unknown",
    };
    let mut out = format!("Conjecture: {}\n\nResult: {verdict}", conjecture.trim());
    for b in &result.bindings {
        out.push_str(&format!("\n  {b}"));
    }
    if !result.proof_kif.is_empty() {
        out.push_str("\n\nProof:");
        for step in &result.proof_kif {
            out.push_str(&format!("\n  {:>3}. [{:<14}] {}", step.index, step.rule, step.formula.flat()));
        }
    } else {
        // Same per-verdict explanation `run_ask` prints for an empty
        // `proof_kif` — say WHY there's nothing to show rather than just
        // omitting the section, since Proved/Inconsistent with no proof
        // reads very differently from Disproved/Consistent with no proof.
        let note = match result.status {
            ProverStatus::Proved | ProverStatus::Inconsistent =>
                "(proof found, but the prover returned no renderable transcript)",
            ProverStatus::Disproved | ProverStatus::Consistent =>
                "(no proof exists: the prover saturated without finding a refutation)",
            ProverStatus::Timeout =>
                "(no proof: the prover timed out before finding a refutation)",
            ProverStatus::InputError =>
                "(no proof: the prover rejected the input before running)",
            ProverStatus::Unknown =>
                "(no proof: the prover found no refutation)",
        };
        out.push_str(&format!("\n\n{note}"));
    }
    out
}

/// Returns whether the last ask verdict was `Proved` (mirrors `run_ask`'s
/// return convention). `false` if the editor was quit before ever asking.
fn run_app<B: ratatui::backend::Backend, L>(
    terminal: &mut Terminal<B>,
    session:  &mut Session<L>,
    manager:  &KBManager,
) -> io::Result<bool>
where
    L: ProvingLayer,
    L::Opts: ProverOptsFor,
{
    let mut app = App::new();
    let mut proved = false;

    loop {
        terminal.draw(|f| draw(f, &app))?;

        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if let Some(overlay) = &app.overlay {
            if let Overlay::Result(text) = overlay {
                proved = text.contains("Result: Proved");
            }
            match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char(' ') => app.overlay = None,
                _ => {}
            }
            continue;
        }

        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(proved);
        }

        match app.mode {
            Mode::Normal => match key.code {
                KeyCode::Up   | KeyCode::Char('k') => app.move_focus(-1),
                KeyCode::Down | KeyCode::Char('j')  => app.move_focus(1),
                KeyCode::Left  | KeyCode::Char('h') => app.current_buf_mut().left(),
                KeyCode::Right | KeyCode::Char('l') => app.current_buf_mut().right(),
                KeyCode::Home  | KeyCode::Char('0') => app.current_buf_mut().cursor = 0,
                KeyCode::End   | KeyCode::Char('$') => app.current_buf_mut().end(),
                KeyCode::Tab   => app.focus = Focus::Query,
                KeyCode::Char('i') => app.mode = Mode::Insert,
                KeyCode::Char('o') => { app.new_formula_after_focus(); app.mode = Mode::Insert; }
                KeyCode::Char('D') => app.delete_focused_formula(),
                KeyCode::Char('a') => app.run_ask(session, manager),
                KeyCode::Char('q') => return Ok(proved),
                _ => {}
            },
            Mode::Insert => match key.code {
                KeyCode::Esc => app.mode = Mode::Normal,
                KeyCode::Left  => app.current_buf_mut().left(),
                KeyCode::Right => app.current_buf_mut().right(),
                KeyCode::Home  => app.current_buf_mut().cursor = 0,
                KeyCode::End   => app.current_buf_mut().end(),
                KeyCode::Backspace => app.current_buf_mut().backspace(),
                KeyCode::Char(c) => app.current_buf_mut().insert(c),
                KeyCode::Enter => match app.focus {
                    Focus::Formula(_) => { app.new_formula_after_focus(); }
                    Focus::Query => app.run_ask(session, manager),
                },
                _ => {}
            },
        }
    }
}

fn draw(f: &mut ratatui::Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::new(
        Direction::Vertical,
        [Constraint::Length(1), Constraint::Min(3), Constraint::Length(3), Constraint::Length(1)],
    ).split(area);

    draw_title(f, chunks[0], app);
    draw_formulas(f, chunks[1], app);
    draw_query(f, chunks[2], app);
    draw_footer(f, chunks[3], app);

    match &app.overlay {
        Some(Overlay::Error(msg))  => draw_overlay(f, area, " error ", msg, Color::Red),
        Some(Overlay::Result(msg)) => draw_overlay(f, area, " result (Esc/Enter to dismiss) ", msg, Color::Green),
        None => {}
    }
}

fn draw_title(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let mode = match app.mode { Mode::Normal => "NORMAL", Mode::Insert => "INSERT" };
    let line = Line::from(Span::styled(
        format!(" sumo ask -i — [{mode}]"),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    f.render_widget(Paragraph::new(line), area);
}

fn draw_formulas(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let n = app.formulas.len().max(1) as u16;
    let rows = Layout::new(
        Direction::Vertical,
        vec![Constraint::Length(3); n as usize],
    ).split(area);

    for (i, buf) in app.formulas.iter().enumerate() {
        let focused = app.focus == Focus::Formula(i);
        let line = cursor_line(buf, focused);
        let border_style = if focused { Style::default().fg(Color::Cyan) } else { Style::default() };
        let p = Paragraph::new(line)
            .block(Block::default().borders(Borders::ALL).title(format!(" [{}] ", i + 1)).border_style(border_style))
            .wrap(Wrap { trim: false });
        if let Some(row) = rows.get(i) {
            f.render_widget(p, *row);
        }
    }
}

fn draw_query(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let focused = app.focus == Focus::Query;
    let border_style = if focused { Style::default().fg(Color::Yellow) } else { Style::default() };
    let line = cursor_line(&app.query, focused);
    let p = Paragraph::new(line)
        .block(Block::default().borders(Borders::ALL).title(" query (conjecture) ").border_style(border_style))
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn draw_footer(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let hint = match app.mode {
        Mode::Normal =>
            "↑/k ↓/j move  ←/h →/l cursor  0/$ line start/end  Tab query  i insert  o new formula  D delete formula  a ask  ^C quit",
        Mode::Insert =>
            "Esc normal  ←/→/Home/End cursor  Backspace delete  Enter new formula / ask (in query)",
    };
    f.render_widget(Paragraph::new(hint).style(Style::default().fg(Color::DarkGray)), area);
}

fn draw_overlay(f: &mut ratatui::Frame, area: Rect, title: &str, text: &str, color: Color) {
    let popup = centered_rect(80, 70, area);
    f.render_widget(Clear, popup);
    let p = Paragraph::new(text)
        .style(Style::default().fg(color))
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false });
    f.render_widget(p, popup);
}

fn centered_rect(pct_x: u16, pct_y: u16, area: Rect) -> Rect {
    let width  = area.width.saturating_mul(pct_x) / 100;
    let height = area.height.saturating_mul(pct_y) / 100;
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect { x, y, width: width.min(area.width), height: height.min(area.height) }
}

/// Render a buffer with a reversed-style block cursor at `buf.cursor` when
/// `focused` — a lightweight stand-in for a real terminal cursor that works
/// uniformly across every cell without fighting ratatui for a single
/// terminal-wide cursor position.
fn cursor_line(buf: &Buffer, focused: bool) -> Line<'static> {
    if !focused {
        return Line::from(buf.text());
    }
    let before: String = buf.chars[..buf.cursor].iter().collect();
    let at = buf.chars.get(buf.cursor).copied().unwrap_or(' ');
    let after: String = buf.chars.get(buf.cursor + 1..).map(|s| s.iter().collect()).unwrap_or_default();
    Line::from(vec![
        Span::raw(before),
        Span::styled(at.to_string(), Style::default().add_modifier(Modifier::REVERSED)),
        Span::raw(after),
    ])
}
