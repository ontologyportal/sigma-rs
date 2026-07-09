// crates/cli/src/cli/config_tui.rs
//
// `sumo config` with no flags, in a real terminal: an interactive editor
// over every `KBManager::options()` entry (grouped the same way
// `config_cmd.rs`'s read-only dump groups them), saving to config.xml on
// exit. `<kb>` constituent lists and any field without an `OptionMeta` row
// are out of scope — preserved as loaded/defaulted, not editable here.
//
// Terminal setup/teardown follows the same raw-mode/alternate-screen idiom
// already established in `man.rs`/`audit.rs`: enable, guaranteed cleanup
// (even on error/panic unwind), gated on a real tty + `!is_ugly()` (checked
// by the caller before this runs).

use std::collections::HashMap;
use std::io;
use std::path::Path;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Terminal;

use sigmakee_rs_sdk::manager::KBManager;
use sigmakee_rs_sdk::manager::meta::{Kind, OptionMeta, Scope};

/// One row in the scrollable list: either a non-selectable section header, or
/// an editable option.
enum Row {
    Header(&'static str),
    Item(&'static OptionMeta),
}

/// Entry point for `sumo config` with no flags, in a real terminal.
pub fn run_config_tui(target: &Path) -> bool {
    let manager = if target.exists() {
        match KBManager::from_config_xml_path_lenient(target) {
            Ok(m) => m,
            Err(e) => { log::error!("config: cannot parse {}: {e}", target.display()); return false; }
        }
    } else {
        KBManager::default()
    };

    match enter_and_run(manager, target) {
        Ok(saved) => saved,
        Err(e) => { log::error!("config: interactive editor failed: {e}"); false }
    }
}

fn enter_and_run(manager: KBManager, target: &Path) -> io::Result<bool> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    let setup = execute!(stdout, EnterAlternateScreen);
    let result = match setup {
        Ok(()) => {
            let backend = CrosstermBackend::new(io::stdout());
            let mut terminal = Terminal::new(backend)?;
            run_app(&mut terminal, manager)
        }
        Err(e) => Err(e),
    };
    // Guaranteed cleanup regardless of how the loop ended.
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
    let _ = disable_raw_mode();

    let outcome = result?;
    if let Some(xml) = outcome {
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(target, xml)?;
        println!("Wrote config: {}", target.display());
        Ok(true)
    } else {
        Ok(true) // quit without saving is not a failure
    }
}

/// Build the row list in the same section order as `config_cmd.rs::run_config`
/// (Global / prover / other subsystem / config-only), skipping `Kind::List`
/// entries — no table option of that kind can round-trip through a typed
/// single value (see `args_project::extract`), so there's nothing sane to
/// edit inline; `--warning` (the only one) stays runtime-only, unchanged
/// from how every other subcommand already treats it.
fn build_rows() -> Vec<Row> {
    let opts = KBManager::options();
    let is_prover = |o: &OptionMeta| {
        o.json_paths.iter().any(|p| p.starts_with("native_prover") || p.starts_with("external_prover"))
            || matches!(o.field, "vampire" | "vampire_hol" | "eprover" | "backend")
    };
    let mut rows = Vec::new();
    let mut section = |title: &'static str, pred: &dyn Fn(&OptionMeta) -> bool| {
        let items: Vec<&'static OptionMeta> = opts.iter()
            .filter(|o| !matches!(o.kind, Kind::List) && pred(o))
            .collect();
        if items.is_empty() { return; }
        rows.push(Row::Header(title));
        rows.extend(items.into_iter().map(Row::Item));
    };
    section("Global flags", &|o| matches!(o.scope, Scope::Global));
    section("Prover options", &|o| matches!(o.scope, Scope::Subsystems(_)) && is_prover(o));
    section("Other subcommand flags", &|o| matches!(o.scope, Scope::Subsystems(_)) && !is_prover(o));
    section("Config-file only", &|o| matches!(o.scope, Scope::ConfigOnly));
    rows
}

struct App {
    rows: Vec<Row>,
    values: HashMap<&'static str, serde_json::Value>,
    list_state: ListState,
    edit_buf: Option<String>,
    dirty: bool,
    confirm_quit: bool,
    status: Option<String>,
}

impl App {
    fn new(manager: &KBManager) -> Self {
        let doc = serde_json::to_value(manager).unwrap_or_default();
        let rows = build_rows();
        let values: HashMap<&'static str, serde_json::Value> = KBManager::options()
            .iter()
            .filter_map(|o| o.json_paths.first().and_then(|p| resolve(&doc, p)).map(|v| (o.field, v.clone())))
            .collect();
        let mut list_state = ListState::default();
        let first_item = rows.iter().position(|r| matches!(r, Row::Item(_)));
        list_state.select(first_item);
        Self { rows, values, list_state, edit_buf: None, dirty: false, confirm_quit: false, status: None }
    }

    fn selected_option(&self) -> Option<&'static OptionMeta> {
        match self.list_state.selected().and_then(|i| self.rows.get(i)) {
            Some(Row::Item(o)) => Some(o),
            _ => None,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let Some(cur) = self.list_state.selected() else { return };
        let n = self.rows.len() as isize;
        let mut i = cur as isize;
        loop {
            i = (i + delta).rem_euclid(n);
            if matches!(self.rows[i as usize], Row::Item(_)) {
                self.list_state.select(Some(i as usize));
                return;
            }
            if i as usize == cur { return; } // no other selectable row
        }
    }

    /// Begin editing the selected option: `Bool` toggles immediately (no
    /// buffer needed); everything else opens a single-line text edit seeded
    /// with the current value.
    fn activate(&mut self) {
        let Some(o) = self.selected_option() else { return };
        if matches!(o.kind, Kind::Bool) {
            let cur = self.values.get(o.field).and_then(|v| v.as_bool()).unwrap_or(false);
            self.values.insert(o.field, serde_json::json!(!cur));
            self.dirty = true;
            return;
        }
        let seed = self.values.get(o.field).map(display_value).unwrap_or_default();
        self.edit_buf = Some(seed);
    }

    fn confirm_edit(&mut self) {
        let (Some(o), Some(buf)) = (self.selected_option(), self.edit_buf.take()) else { return };
        let parsed = match o.kind {
            Kind::Int   => buf.trim().parse::<u64>().ok().map(|n| serde_json::json!(n)),
            Kind::Float => buf.trim().parse::<f64>().ok().map(|n| serde_json::json!(n)),
            Kind::Str | Kind::Path => Some(serde_json::json!(buf)),
            Kind::Bool | Kind::List => None,
        };
        match parsed {
            Some(v) => { self.values.insert(o.field, v); self.dirty = true; self.status = None; }
            None => self.status = Some(format!("'{buf}' is not a valid {kind}", kind = kind_name(o.kind))),
        }
    }
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Int => "integer",
        Kind::Float => "number",
        _ => "value",
    }
}

fn display_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn resolve<'a>(doc: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut cur = doc;
    for seg in path.split('.') {
        cur = cur.get(seg)?;
    }
    Some(cur)
}

/// Returns `Ok(Some(xml))` to save-and-exit, `Ok(None)` to quit without
/// saving.
fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    mut manager: KBManager,
) -> io::Result<Option<String>> {
    let mut app = App::new(&manager);

    loop {
        terminal.draw(|f| draw(f, &app))?;

        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if let Some(buf) = app.edit_buf.as_mut() {
            match key.code {
                KeyCode::Enter  => app.confirm_edit(),
                KeyCode::Esc    => { app.edit_buf = None; }
                KeyCode::Backspace => { buf.pop(); }
                KeyCode::Char(c) => buf.push(c),
                _ => {}
            }
            continue;
        }

        if app.confirm_quit {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => return Ok(None),
                _ => app.confirm_quit = false,
            }
            continue;
        }

        match key.code {
            KeyCode::Up | KeyCode::Char('k')   => app.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
            KeyCode::Enter | KeyCode::Char(' ') => app.activate(),
            KeyCode::Char('s') => {
                let overrides: Vec<(&'static OptionMeta, serde_json::Value)> = KBManager::options()
                    .iter()
                    .filter_map(|o| app.values.get(o.field).map(|v| (o, v.clone())))
                    .collect();
                if let Err(e) = manager.apply_overrides(overrides) {
                    app.status = Some(format!("save failed: {e}"));
                    continue;
                }
                return Ok(Some(manager.to_config_xml()));
            }
            KeyCode::Char('q') | KeyCode::Esc => {
                if app.dirty {
                    app.confirm_quit = true;
                } else {
                    return Ok(None);
                }
            }
            _ => {}
        }
    }
}

fn draw(f: &mut ratatui::Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::new(
        Direction::Vertical,
        [Constraint::Min(5), Constraint::Length(4), Constraint::Length(1)],
    ).split(area);

    draw_list(f, chunks[0], app);
    draw_detail(f, chunks[1], app);
    draw_footer(f, chunks[2], app);

    if let Some(buf) = &app.edit_buf {
        draw_edit_popup(f, area, app, buf);
    } else if app.confirm_quit {
        draw_confirm_popup(f, area);
    }
}

fn draw_list(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app.rows.iter().map(|r| match r {
        Row::Header(title) => ListItem::new(Line::from(Span::styled(
            format!("── {title} ──"),
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
        ))),
        Row::Item(o) => {
            let value = app.values.get(o.field).map(display_value).unwrap_or_default();
            ListItem::new(Line::from(vec![
                Span::raw(format!("  --{:<26}", o.long)),
                Span::styled(format!("= {value}"), Style::default().fg(Color::Green)),
            ]))
        }
    }).collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" sumo config "))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    let mut state = app.list_state.clone();
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_detail(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let text = if let Some(msg) = &app.status {
        Line::from(Span::styled(msg.as_str(), Style::default().fg(Color::Red)))
    } else if let Some(o) = app.selected_option() {
        Line::from(o.help)
    } else {
        Line::from("")
    };
    let p = Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL).title(" help "))
        .wrap(Wrap { trim: true });
    f.render_widget(p, area);
}

fn draw_footer(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let hint = if app.dirty {
        "↑/↓ navigate  Enter/Space edit  s save & quit  q quit (unsaved changes)"
    } else {
        "↑/↓ navigate  Enter/Space edit  s save & quit  q quit"
    };
    f.render_widget(Paragraph::new(hint).style(Style::default().fg(Color::DarkGray)), area);
}

fn draw_edit_popup(f: &mut ratatui::Frame, area: Rect, app: &App, buf: &str) {
    let Some(o) = app.selected_option() else { return };
    let popup = centered_rect(60, 5, area);
    f.render_widget(Clear, popup);
    let p = Paragraph::new(format!("{buf}_"))
        .block(Block::default().borders(Borders::ALL)
            .title(format!(" --{}  (Enter=confirm, Esc=cancel) ", o.long)));
    f.render_widget(p, popup);
}

fn draw_confirm_popup(f: &mut ratatui::Frame, area: Rect) {
    let popup = centered_rect(50, 5, area);
    f.render_widget(Clear, popup);
    let p = Paragraph::new("Quit without saving? (y/n)")
        .block(Block::default().borders(Borders::ALL).title(" unsaved changes "));
    f.render_widget(p, popup);
}

fn centered_rect(pct_x: u16, height: u16, area: Rect) -> Rect {
    let width = area.width.saturating_mul(pct_x) / 100;
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect { x, y, width: width.min(area.width), height: height.min(area.height) }
}
