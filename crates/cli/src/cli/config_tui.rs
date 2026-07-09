// crates/cli/src/cli/config_tui.rs
//
// `sumo config` with no flags, in a real terminal: an interactive editor
// over every `KBManager::options()` entry (grouped the same way
// `config_cmd.rs`'s read-only dump groups them), saving to config.xml on
// exit. Also shows the configured `<kb>`s and their constituents (matching
// `config_cmd.rs::print_kbs`), and lets you add/remove constituents or
// create a new KB directly — the same mutations `sumo config --kb NAME
// -f/-d/--exclude` exposes on the command line (`KBManager::create_kb` /
// `add_constituents_to_kb` / `remove_constituents_from_kb`), just reachable
// from the viewer that already shows every other config.xml-derived value.
//
// Terminal setup/teardown follows the same raw-mode/alternate-screen idiom
// already established in `man.rs`/`audit.rs`: enable, guaranteed cleanup
// (even on error/panic unwind), gated on a real tty + `!is_ugly()` (checked
// by the caller before this runs).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Terminal;

use sigmakee_rs_sdk::Source;
use sigmakee_rs_sdk::manager::{Constituent, KBManager};
use sigmakee_rs_sdk::manager::meta::{Kind, OptionMeta, Scope};

/// One row in the scrollable list: a non-selectable section header, an
/// editable option, a KB summary line, or one of that KB's constituents.
enum Row {
    Header(&'static str),
    Item(&'static OptionMeta),
    Kb { name: String, active: bool, count: usize },
    Constituent { kb: String, path: PathBuf, label: String },
}

/// Which text-edit flow `App::edit_buf` is currently feeding, so `Enter`
/// dispatches to the right confirm handler.
#[derive(Clone)]
enum EditKind {
    Option,
    AddConstituent(String),
    NewKb,
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
/// (Global / native prover / external prover / other subsystem / config-only
/// / knowledge bases), skipping `Kind::List` entries — no table option of
/// that kind can round-trip through a typed single value (see
/// `args_project::extract`), so there's nothing sane to edit inline;
/// `--warning` (the only one) stays runtime-only, unchanged from how every
/// other subcommand already treats it.
fn build_rows(manager: &KBManager) -> Vec<Row> {
    let opts = KBManager::options();
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
    section("Native prover options (NativeProverConfig)",
        &|o| super::config_cmd::prover_category(o) == Some(super::config_cmd::ProverCategory::Native));
    section("External prover options (ExternalProverConfig)",
        &|o| super::config_cmd::prover_category(o) == Some(super::config_cmd::ProverCategory::External));
    section("Other subcommand flags",
        &|o| matches!(o.scope, Scope::Subsystems(_)) && super::config_cmd::prover_category(o).is_none());
    section("Config-file only", &|o| matches!(o.scope, Scope::ConfigOnly));

    if !manager.kbs.is_empty() {
        rows.push(Row::Header("Knowledge bases (a: add constituent, d: delete, n: new KB)"));
        for kb in &manager.kbs {
            rows.push(Row::Kb {
                name: kb.name().to_string(),
                active: kb.name() == manager.sumokbname,
                count: kb.constituents().len(),
            });
            for c in kb.constituents() {
                rows.push(Row::Constituent {
                    kb: kb.name().to_string(),
                    path: constituent_path(c),
                    label: render_constituent(c),
                });
            }
        }
    }
    rows
}

/// The single filesystem path a constituent is stored under — every
/// constituent this crate ever produces (via config.xml parsing or
/// `add_constituents_to_kb`) is single-path, so this always has a real
/// answer in practice.
fn constituent_path(c: &Constituent) -> PathBuf {
    match c {
        Constituent::Named(p) => p.clone(),
        Constituent::Source(Source::Local(paths)) => paths.first().cloned().unwrap_or_default(),
        Constituent::Source(_) => PathBuf::new(),
    }
}

/// One-line rendering of a constituent — mirrors `config_cmd.rs::render_source`.
fn render_constituent(c: &Constituent) -> String {
    match c {
        Constituent::Named(p) => format!("{} [relative → kbDir]", p.display()),
        Constituent::Source(Source::Local(paths)) =>
            format!("{} [pinned]", paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")),
        Constituent::Source(_) => "<non-local source>".to_string(),
    }
}

struct App {
    rows: Vec<Row>,
    values: HashMap<&'static str, serde_json::Value>,
    list_state: ListState,
    edit_buf: Option<String>,
    edit_kind: EditKind,
    dirty: bool,
    confirm_quit: bool,
    status: Option<String>,
}

impl App {
    fn new(manager: &KBManager) -> Self {
        let doc = serde_json::to_value(manager).unwrap_or_default();
        let rows = build_rows(manager);
        let values: HashMap<&'static str, serde_json::Value> = KBManager::options()
            .iter()
            .filter_map(|o| o.json_paths.first().and_then(|p| resolve(&doc, p)).map(|v| (o.field, v.clone())))
            .collect();
        let mut list_state = ListState::default();
        let first_stop = rows.iter().position(|r| !matches!(r, Row::Header(_)));
        list_state.select(first_stop);
        Self {
            rows, values, list_state,
            edit_buf: None, edit_kind: EditKind::Option,
            dirty: false, confirm_quit: false, status: None,
        }
    }

    fn selected_option(&self) -> Option<&'static OptionMeta> {
        match self.list_state.selected().and_then(|i| self.rows.get(i)) {
            Some(Row::Item(o)) => Some(o),
            _ => None,
        }
    }

    /// The KB the current selection belongs to — a `Kb` row names itself, a
    /// `Constituent` row names its owner. `None` off the knowledge-bases
    /// section entirely.
    fn selected_kb(&self) -> Option<&str> {
        match self.list_state.selected().and_then(|i| self.rows.get(i)) {
            Some(Row::Kb { name, .. }) => Some(name.as_str()),
            Some(Row::Constituent { kb, .. }) => Some(kb.as_str()),
            _ => None,
        }
    }

    /// Navigate to the next/previous non-header row — `Row::Kb`/`Row::Constituent`
    /// (e.g. a KB constituent line) is a valid stop too, even though it's not
    /// editable the same way an option is (`activate`/`selected_option`
    /// no-op on it): the alternative — only ever landing on a `Row::Item` —
    /// means navigation can never advance past the *last editable option*,
    /// so ratatui's "keep the selection visible" auto-scroll has no reason
    /// to ever reveal whatever's rendered below it (the "Knowledge bases"
    /// section, entirely unreachable no matter how far down you scroll).
    fn move_selection(&mut self, delta: isize) {
        let Some(cur) = self.list_state.selected() else { return };
        let n = self.rows.len() as isize;
        let mut i = cur as isize;
        loop {
            i = (i + delta).rem_euclid(n);
            if !matches!(self.rows[i as usize], Row::Header(_)) {
                self.list_state.select(Some(i as usize));
                return;
            }
            if i as usize == cur { return; } // no other stoppable row
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
        self.edit_kind = EditKind::Option;
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

    /// Open the "add constituent" prompt for the KB the selection belongs
    /// to. No-op off the knowledge-bases section.
    fn begin_add_constituent(&mut self) {
        let Some(kb) = self.selected_kb() else { return };
        self.edit_kind = EditKind::AddConstituent(kb.to_string());
        self.edit_buf = Some(String::new());
    }

    /// Add the typed path (file or directory — `add_constituents_to_kb`
    /// treats them identically) as a constituent of `kb`, existence-checked
    /// the same way `sumo config --kb NAME -f ...` is on the command line.
    fn confirm_add_constituent(&mut self, manager: &mut KBManager, kb: &str) {
        let buf = self.edit_buf.take().unwrap_or_default();
        let path = buf.trim();
        if path.is_empty() {
            self.status = Some("path cannot be empty".to_string());
            return;
        }
        match manager.add_constituents_to_kb(kb, vec![PathBuf::from(path)], vec![], true) {
            Ok(()) => {
                self.dirty = true;
                self.status = None;
                self.refresh_rows(manager, Some(kb));
            }
            Err(e) => self.status = Some(e.to_string()),
        }
    }

    /// Open the "new KB name" prompt. Available regardless of the current
    /// selection.
    fn begin_new_kb(&mut self) {
        self.edit_kind = EditKind::NewKb;
        self.edit_buf = Some(String::new());
    }

    fn confirm_new_kb(&mut self, manager: &mut KBManager) {
        let name = self.edit_buf.take().unwrap_or_default().trim().to_string();
        if name.is_empty() {
            self.status = Some("KB name cannot be empty".to_string());
            return;
        }
        if manager.kbs.iter().any(|k| k.name() == name) {
            self.status = Some(format!("KB `{name}` already exists"));
            return;
        }
        manager.create_kb(&name);
        self.dirty = true;
        self.status = None;
        self.refresh_rows(manager, Some(&name));
    }

    /// Remove the selected constituent immediately — no confirmation
    /// prompt, matching this editor's existing "toggle now, `s` persists to
    /// disk" pattern (a Bool option flips the same way). No-op unless the
    /// selection is on a `Constituent` row.
    fn delete_selected_constituent(&mut self, manager: &mut KBManager) {
        let Some(Row::Constituent { kb, path, .. }) = self.list_state.selected().and_then(|i| self.rows.get(i))
        else { return };
        let (kb, path) = (kb.clone(), path.clone());
        match manager.remove_constituents_from_kb(&kb, vec![path]) {
            Ok(n) if n > 0 => {
                self.dirty = true;
                self.status = None;
                self.refresh_rows(manager, Some(&kb));
            }
            Ok(_) => {}
            Err(e) => self.status = Some(e.to_string()),
        }
    }

    /// Rebuild `rows` after a KB mutation, keeping the selection on
    /// `prefer_kb`'s row when it's still present.
    fn refresh_rows(&mut self, manager: &KBManager, prefer_kb: Option<&str>) {
        self.rows = build_rows(manager);
        let idx = prefer_kb.and_then(|want| self.rows.iter().position(|r|
            matches!(r, Row::Kb { name, .. } if name == want)))
            .or_else(|| self.rows.iter().position(|r| !matches!(r, Row::Header(_))));
        self.list_state.select(idx);
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
        terminal.draw(|f| draw(f, &mut app))?;

        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if app.edit_buf.is_some() {
            match key.code {
                KeyCode::Enter => match app.edit_kind.clone() {
                    EditKind::Option => app.confirm_edit(),
                    EditKind::AddConstituent(kb) => app.confirm_add_constituent(&mut manager, &kb),
                    EditKind::NewKb => app.confirm_new_kb(&mut manager),
                },
                KeyCode::Esc => { app.edit_buf = None; }
                KeyCode::Backspace => { if let Some(buf) = app.edit_buf.as_mut() { buf.pop(); } }
                KeyCode::Char(c) => { if let Some(buf) = app.edit_buf.as_mut() { buf.push(c); } }
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
            KeyCode::Char('a') => app.begin_add_constituent(),
            KeyCode::Char('n') => app.begin_new_kb(),
            KeyCode::Char('d') | KeyCode::Delete => app.delete_selected_constituent(&mut manager),
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

fn draw(f: &mut ratatui::Frame, app: &mut App) {
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

/// Takes `&mut App`, not `&App`: `render_stateful_widget` writes the
/// scrolled-into-view offset back into whatever `ListState` it's given, and
/// that write needs to land in `app.list_state` itself (not a throwaway
/// clone) — otherwise every frame recomputes the viewport from a stale
/// offset instead of the real one, so a section past the first screenful
/// (e.g. "Prover options", after 8 "Global flags" rows) becomes unreachable
/// or erratic to scroll to.
fn draw_list(f: &mut ratatui::Frame, area: Rect, app: &mut App) {
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
        Row::Kb { name, active, count } => {
            let marker = if *active { "●" } else { "○" };
            ListItem::new(Line::from(Span::styled(
                format!("{marker} {name}  ({count} constituent(s))"),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )))
        }
        Row::Constituent { label, .. } => ListItem::new(Line::from(Span::styled(
            format!("    {label}"), Style::default().fg(Color::Cyan),
        ))),
    }).collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" sumo config "))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    f.render_stateful_widget(list, area, &mut app.list_state);
}

fn draw_detail(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let text = if let Some(msg) = &app.status {
        Line::from(Span::styled(msg.as_str(), Style::default().fg(Color::Red)))
    } else if let Some(o) = app.selected_option() {
        Line::from(o.help)
    } else {
        match app.list_state.selected().and_then(|i| app.rows.get(i)) {
            Some(Row::Kb { .. }) => Line::from(Span::styled(
                "a: add constituent   n: new KB", Style::default().fg(Color::DarkGray),
            )),
            Some(Row::Constituent { .. }) => Line::from(Span::styled(
                "a: add another constituent   d: delete this one", Style::default().fg(Color::DarkGray),
            )),
            _ => Line::from(""),
        }
    };
    let p = Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL).title(" help "))
        .wrap(Wrap { trim: true });
    f.render_widget(p, area);
}

fn draw_footer(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let hint = if app.dirty {
        "↑/↓ navigate  Enter/Space edit  a add  d delete  n new KB  s save & quit  q quit (unsaved changes)"
    } else {
        "↑/↓ navigate  Enter/Space edit  a add  d delete  n new KB  s save & quit  q quit"
    };
    f.render_widget(Paragraph::new(hint).style(Style::default().fg(Color::DarkGray)), area);
}

fn draw_edit_popup(f: &mut ratatui::Frame, area: Rect, app: &App, buf: &str) {
    let title = match &app.edit_kind {
        EditKind::Option => {
            let Some(o) = app.selected_option() else { return };
            format!(" --{}  (Enter=confirm, Esc=cancel) ", o.long)
        }
        EditKind::AddConstituent(kb) => format!(" add constituent to `{kb}`  (Enter=confirm, Esc=cancel) "),
        EditKind::NewKb => " new KB name  (Enter=confirm, Esc=cancel) ".to_string(),
    };
    let popup = centered_rect(60, 5, area);
    f.render_widget(Clear, popup);
    let p = Paragraph::new(format!("{buf}_"))
        .block(Block::default().borders(Borders::ALL).title(title));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_manager() -> KBManager {
        KBManager::parse_config_xml_lenient(
            r#"<configuration>
              <preference name="sumokbname" value="SUMO"/>
              <kb name="SUMO">
                <constituent filename="Merge.kif"/>
              </kb>
            </configuration>"#,
        ).unwrap()
    }

    /// Drift guard: every section `run_config`'s read-only dump shows should
    /// also be non-empty here — a regression on this would silently drop an
    /// entire category of settings from the interactive editor (this caught
    /// exactly that for a scroll-offset bug that made "Prover options" look
    /// missing, even though it was present in `build_rows()` all along).
    #[test]
    fn every_dump_section_has_rows() {
        let manager = fixture_manager();
        let rows = build_rows(&manager);
        let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        let mut cur = "";
        for r in &rows {
            match r {
                Row::Header(h) => cur = h,
                Row::Item(_) | Row::Kb { .. } | Row::Constituent { .. } => *counts.entry(cur).or_insert(0) += 1,
            }
        }
        for section in [
            "Global flags",
            "Native prover options (NativeProverConfig)",
            "External prover options (ExternalProverConfig)",
            "Other subcommand flags",
            "Config-file only",
        ] {
            assert!(
                counts.get(section).copied().unwrap_or(0) > 0,
                "section `{section}` has no rows in the sumo config TUI",
            );
        }
        assert!(
            counts.keys().any(|k| k.starts_with("Knowledge bases")),
            "no Knowledge bases section rendered for a manager with a configured KB",
        );
    }

    /// Regression: `move_selection` used to stop only at `Row::Item`, so
    /// navigation could never advance past the last *editable* option —
    /// the "Knowledge bases" section below it was in `app.rows` (proven by
    /// `every_dump_section_has_rows`) but unreachable, since ratatui only
    /// auto-scrolls to keep the *selected* row visible. Confirms `App` can
    /// navigate all the way down to a trailing `Row::Constituent` row via
    /// repeated `move_selection(1)`.
    #[test]
    fn navigation_reaches_trailing_constituent_rows() {
        let manager = fixture_manager();
        let mut app = App::new(&manager);
        let last_constituent_index = app.rows.iter().rposition(|r| matches!(r, Row::Constituent { .. }))
            .expect("fixture has at least one constituent row");

        // Walk `move_selection(1)` forward, collecting every stop, until it
        // revisits one (a full lap — `move_selection` wraps deterministically,
        // so this always terminates without needing a fixed step budget).
        let mut visited = std::collections::HashSet::new();
        visited.insert(app.list_state.selected().expect("App::new selects a starting row"));
        loop {
            app.move_selection(1);
            let cur = app.list_state.selected().unwrap();
            if !visited.insert(cur) { break; }
        }
        assert!(
            visited.contains(&last_constituent_index),
            "navigation never reached the last constituent row (index {last_constituent_index}); visited {visited:?}",
        );
    }

    #[test]
    fn new_kb_then_add_constituent_round_trips_into_the_manager() {
        let mut manager = KBManager::default();
        let mut app = App::new(&manager);

        app.begin_new_kb();
        app.edit_buf = Some("SUMO".to_string());
        app.confirm_new_kb(&mut manager);
        assert_eq!(manager.kbs.len(), 1);
        assert_eq!(manager.kbs[0].name(), "SUMO");
        assert!(app.dirty);
        assert_eq!(app.selected_kb(), Some("SUMO"), "selection follows the newly created KB");

        let dir = std::env::temp_dir().join("sdk_tui_add_constituent");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("Merge.kif");
        std::fs::write(&f, "").unwrap();

        app.begin_add_constituent();
        app.edit_buf = Some(f.display().to_string());
        app.confirm_add_constituent(&mut manager, "SUMO");
        assert_eq!(manager.kbs[0].constituents().len(), 1);
    }

    #[test]
    fn delete_selected_constituent_removes_it_from_the_manager() {
        let dir = std::env::temp_dir().join("sdk_tui_delete_constituent");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("Merge.kif");
        std::fs::write(&f, "").unwrap();

        let mut manager = KBManager::default();
        manager.add_constituents_to_kb("SUMO", vec![f], vec![], true).unwrap();
        let mut app = App::new(&manager);

        let idx = app.rows.iter().position(|r| matches!(r, Row::Constituent { .. })).unwrap();
        app.list_state.select(Some(idx));
        app.delete_selected_constituent(&mut manager);

        assert!(manager.kbs[0].constituents().is_empty());
        assert!(app.dirty);
    }
}
