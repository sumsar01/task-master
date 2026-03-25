use crate::registry::{Registry, Worktree};
use crate::stats::{fetch_stats, format_tokens, StatsRow};
use crate::status::find_live_phase;
use crate::tmux;
use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};
use std::{
    collections::HashMap,
    io,
    time::{Duration, Instant},
};

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum ActionKind {
    Spawn,
    Plan,
    Qa,
}

#[derive(Debug, Clone, PartialEq)]
enum Mode {
    Normal,
    Prompt(ActionKind),
    /// Spawn on an active window: user confirmed once, needs Enter again to force.
    ForceConfirm,
}

struct App {
    worktrees: Vec<Worktree>,
    phases: Vec<String>,
    list_state: ListState,
    mode: Mode,
    input_buf: String,
    /// Stats cache keyed by worktree index; filled lazily on selection change.
    stats_cache: HashMap<usize, StatsRow>,
    /// Transient status message and when it was set.
    status_msg: Option<(String, Instant)>,
    session: String,
    should_quit: bool,
    /// Track which index was active when we last loaded stats.
    last_stats_idx: Option<usize>,
}

impl App {
    fn new(registry: &Registry, session: String) -> Self {
        let worktrees = registry.worktrees.clone();
        let count = worktrees.len();
        let mut list_state = ListState::default();
        if count > 0 {
            list_state.select(Some(0));
        }
        App {
            worktrees,
            phases: vec!["?".to_string(); count],
            list_state,
            mode: Mode::Normal,
            input_buf: String::new(),
            stats_cache: HashMap::new(),
            status_msg: None,
            session,
            should_quit: false,
            last_stats_idx: None,
        }
    }

    fn selected(&self) -> Option<usize> {
        self.list_state.selected()
    }

    fn selected_phase(&self) -> &str {
        match self.selected() {
            Some(i) => self.phases.get(i).map(|s| s.as_str()).unwrap_or("?"),
            None => "?",
        }
    }

    fn is_active_phase(phase: &str) -> bool {
        !matches!(phase, "idle" | "?" | "")
    }

    fn move_up(&mut self) {
        let len = self.worktrees.len();
        if len == 0 {
            return;
        }
        let i = self.selected().unwrap_or(0);
        self.list_state
            .select(Some(if i == 0 { len - 1 } else { i - 1 }));
    }

    fn move_down(&mut self) {
        let len = self.worktrees.len();
        if len == 0 {
            return;
        }
        let i = self.selected().unwrap_or(0);
        self.list_state.select(Some((i + 1) % len));
    }

    fn set_status(&mut self, msg: impl Into<String>) {
        self.status_msg = Some((msg.into(), Instant::now()));
    }

    /// Returns the current status message if it's still within the display window.
    fn current_status(&self) -> Option<&str> {
        self.status_msg.as_ref().and_then(|(msg, at)| {
            if at.elapsed() < Duration::from_secs(4) {
                Some(msg.as_str())
            } else {
                None
            }
        })
    }

    fn refresh_phases(&mut self) {
        for (i, wt) in self.worktrees.iter().enumerate() {
            let phase = find_live_phase(&self.session, &wt.window_name)
                .unwrap_or_else(|| "idle".to_string());
            self.phases[i] = phase;
        }
    }

    fn load_stats_for_selected(&mut self) {
        let idx = match self.selected() {
            Some(i) => i,
            None => return,
        };
        if self.last_stats_idx == Some(idx) {
            return; // already cached
        }
        if let Some(wt) = self.worktrees.get(idx) {
            let path = wt.abs_path.to_string_lossy().to_string();
            let stats = fetch_stats(&path, None).unwrap_or_default();
            self.stats_cache.insert(idx, stats);
        }
        self.last_stats_idx = Some(idx);
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Open (or focus) a dedicated `task-master` tmux window and run the TUI there.
///
/// If the window doesn't exist yet, it is created in the current session and
/// focused. If it already exists, we just focus it (the TUI is already running
/// there, driven by the other process).
pub fn cmd_tui(registry: &Registry) -> Result<()> {
    let session = tmux::current_session()
        .context("task-master tui must be run from within a tmux session")?;

    // Ensure the dedicated task-master window exists and is focused.
    let base_dir = registry.base_dir.to_string_lossy().to_string();
    ensure_tui_window(&session, &base_dir)?;

    // Initialise the App state.
    let mut app = App::new(registry, session.clone());
    app.refresh_phases();
    app.load_stats_for_selected();

    // Set up the terminal.
    enable_raw_mode().context("Failed to enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("Failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("Failed to create terminal")?;

    let res = run_loop(&mut terminal, &mut app, registry);

    // Always restore the terminal, even on error.
    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    res
}

/// Create the `task-master` tmux window if it doesn't exist, then focus it.
fn ensure_tui_window(session: &str, working_dir: &str) -> Result<()> {
    use std::process::Command;

    let window_name = "task-master";

    if tmux::find_window_index(session, window_name).is_none() {
        // Create a new window (don't switch yet — we do that next).
        let end_target = format!("{}:", session);
        let status = Command::new("tmux")
            .args([
                "new-window",
                "-d",
                "-t",
                &end_target,
                "-n",
                window_name,
                "-c",
                working_dir,
            ])
            .status()
            .context("Failed to create task-master tmux window")?;
        if !status.success() {
            anyhow::bail!("tmux new-window failed");
        }
    }

    // Focus the window.
    if let Some(idx) = tmux::find_window_index(session, window_name) {
        let target = format!("{}:{}", session, idx);
        Command::new("tmux")
            .args(["select-window", "-t", &target])
            .status()
            .context("Failed to focus task-master window")?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Main event loop
// ---------------------------------------------------------------------------

fn run_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    registry: &Registry,
) -> Result<()> {
    loop {
        terminal.draw(|f| render(f, app))?;

        // Poll with a 2-second timeout for the refresh tick.
        if event::poll(Duration::from_millis(2000))? {
            if let Event::Key(key) = event::read()? {
                // Only react to key-press events (not release/repeat on some platforms).
                if key.kind == KeyEventKind::Press {
                    handle_key(app, registry, key.code, key.modifiers)?;
                }
            }
        } else {
            // Timeout: refresh phases.
            app.refresh_phases();
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Key handling
// ---------------------------------------------------------------------------

fn handle_key(
    app: &mut App,
    registry: &Registry,
    code: KeyCode,
    _modifiers: KeyModifiers,
) -> Result<()> {
    match &app.mode.clone() {
        Mode::Normal => handle_normal(app, registry, code),
        Mode::Prompt(kind) => handle_prompt(app, registry, code, kind.clone()),
        Mode::ForceConfirm => handle_force_confirm(app, registry, code),
    }
}

fn handle_normal(app: &mut App, _registry: &Registry, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Char('q') | KeyCode::Char('Q') => {
            app.should_quit = true;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.move_up();
            app.load_stats_for_selected();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.move_down();
            app.load_stats_for_selected();
        }
        KeyCode::Char('s') => {
            if app.worktrees.is_empty() {
                return Ok(());
            }
            let phase = app.selected_phase().to_string();
            if App::is_active_phase(&phase) {
                // Active window: warn first, let user type prompt and confirm.
                app.set_status(format!(
                    "Warning: {} is [{}] — spawning will kill the running agent. Type prompt and Enter to confirm, Esc to cancel.",
                    app.selected().and_then(|i| app.worktrees.get(i)).map(|w| w.window_name.as_str()).unwrap_or("?"),
                    phase
                ));
            }
            app.input_buf.clear();
            app.mode = Mode::Prompt(ActionKind::Spawn);
        }
        KeyCode::Char('p') => {
            if app.worktrees.is_empty() {
                return Ok(());
            }
            app.input_buf.clear();
            app.mode = Mode::Prompt(ActionKind::Plan);
        }
        KeyCode::Char('x') => {
            // QA — only enabled when window is active.
            let phase = app.selected_phase().to_string();
            if !App::is_active_phase(&phase) || app.worktrees.is_empty() {
                return Ok(());
            }
            app.input_buf.clear();
            app.mode = Mode::Prompt(ActionKind::Qa);
        }
        KeyCode::Char('r') => {
            let phase = app.selected_phase().to_string();
            if !App::is_active_phase(&phase) || app.worktrees.is_empty() {
                return Ok(());
            }
            if let Some(i) = app.selected() {
                if let Some(wt) = app.worktrees.get(i) {
                    let name = wt.window_name.clone();
                    match crate::cmd_reset(&name) {
                        Ok(()) => {
                            app.set_status(format!("Reset {} to idle.", name));
                            app.refresh_phases();
                        }
                        Err(e) => app.set_status(format!("Reset failed: {}", e)),
                    }
                }
            }
        }
        KeyCode::Char('a') => {
            let phase = app.selected_phase().to_string();
            if !App::is_active_phase(&phase) || app.worktrees.is_empty() {
                return Ok(());
            }
            if let Some(i) = app.selected() {
                if let Some(wt) = app.worktrees.get(i) {
                    // Find the actual current window name (with phase suffix).
                    let full_name = format!("{}:{}", wt.window_name, phase);
                    attach_to_window(&app.session, &wt.window_name, &full_name);
                }
            }
            app.should_quit = true;
        }
        _ => {}
    }
    Ok(())
}

fn handle_prompt(
    app: &mut App,
    registry: &Registry,
    code: KeyCode,
    kind: ActionKind,
) -> Result<()> {
    match code {
        KeyCode::Esc => {
            app.mode = Mode::Normal;
            app.input_buf.clear();
            app.status_msg = None;
        }
        KeyCode::Enter => {
            execute_action(app, registry, &kind, false)?;
        }
        KeyCode::Backspace => {
            app.input_buf.pop();
        }
        KeyCode::Char(c) => {
            app.input_buf.push(c);
        }
        _ => {}
    }
    Ok(())
}

fn handle_force_confirm(app: &mut App, registry: &Registry, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Esc => {
            app.mode = Mode::Normal;
            app.input_buf.clear();
            app.status_msg = None;
        }
        KeyCode::Enter => {
            execute_spawn(app, registry, true)?;
        }
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Action execution
// ---------------------------------------------------------------------------

fn execute_action(
    app: &mut App,
    registry: &Registry,
    kind: &ActionKind,
    force: bool,
) -> Result<()> {
    match kind {
        ActionKind::Spawn => execute_spawn(app, registry, force),
        ActionKind::Plan => execute_plan(app, registry),
        ActionKind::Qa => execute_qa(app, registry),
    }
}

fn execute_spawn(app: &mut App, registry: &Registry, force: bool) -> Result<()> {
    let (wt_name, prompt) = match collect_spawn_inputs(app) {
        Some(x) => x,
        None => return Ok(()),
    };
    match crate::cmd_spawn(registry, &wt_name, &prompt, force) {
        Ok(()) => {
            app.set_status(format!("Spawned {}:dev", wt_name));
            app.mode = Mode::Normal;
            app.input_buf.clear();
            app.refresh_phases();
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("uncommitted changes") && !force {
                // Ask for explicit force confirmation.
                app.set_status(format!(
                    "{} has uncommitted changes. Press Enter to force-reset and spawn, Esc to cancel.",
                    wt_name
                ));
                app.mode = Mode::ForceConfirm;
            } else {
                app.set_status(format!("Spawn failed: {}", msg));
                app.mode = Mode::Normal;
                app.input_buf.clear();
            }
        }
    }
    Ok(())
}

fn execute_plan(app: &mut App, registry: &Registry) -> Result<()> {
    let (wt_name, prompt) = match collect_spawn_inputs(app) {
        Some(x) => x,
        None => return Ok(()),
    };
    match crate::plan::cmd_plan(registry, &wt_name, &prompt) {
        Ok(()) => {
            app.set_status(format!("Plan agent started in {}:plan", wt_name));
            app.mode = Mode::Normal;
            app.input_buf.clear();
            app.refresh_phases();
        }
        Err(e) => {
            app.set_status(format!("Plan failed: {}", e));
            app.mode = Mode::Normal;
            app.input_buf.clear();
        }
    }
    Ok(())
}

fn execute_qa(app: &mut App, registry: &Registry) -> Result<()> {
    let wt_name = match app.selected().and_then(|i| app.worktrees.get(i)) {
        Some(wt) => wt.window_name.clone(),
        None => return Ok(()),
    };
    let pr_number: u64 = match app.input_buf.trim().parse() {
        Ok(n) => n,
        Err(_) => {
            app.set_status("Invalid PR number — enter a number (e.g. 42)");
            return Ok(());
        }
    };
    match crate::qa::cmd_qa(registry, &wt_name, pr_number) {
        Ok(()) => {
            app.set_status(format!(
                "QA agent started for {} PR #{}",
                wt_name, pr_number
            ));
            app.mode = Mode::Normal;
            app.input_buf.clear();
            app.refresh_phases();
        }
        Err(e) => {
            app.set_status(format!("QA failed: {}", e));
            app.mode = Mode::Normal;
            app.input_buf.clear();
        }
    }
    Ok(())
}

/// Returns (worktree_name, prompt_text) from the current app state, or None if
/// inputs are missing.
fn collect_spawn_inputs(app: &mut App) -> Option<(String, String)> {
    let wt_name = app
        .selected()
        .and_then(|i| app.worktrees.get(i))
        .map(|wt| wt.window_name.clone())?;
    let prompt = app.input_buf.trim().to_string();
    if prompt.is_empty() {
        app.set_status("Prompt cannot be empty");
        return None;
    }
    Some((wt_name, prompt))
}

fn attach_to_window(session: &str, base_name: &str, full_name: &str) {
    use std::process::Command;
    // Try the full name first (e.g. WIS-olive:dev), fall back to base name.
    let target_full = format!("{}:{}", session, full_name);
    let status = Command::new("tmux")
        .args(["select-window", "-t", &target_full])
        .status();
    if status.map(|s| s.success()).unwrap_or(false) {
        return;
    }
    // Fallback: look up by index.
    if let Some(idx) = tmux::find_window_index(session, base_name) {
        let target = format!("{}:{}", session, idx);
        Command::new("tmux")
            .args(["select-window", "-t", &target])
            .status()
            .ok();
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();

    // Outer layout: title (1), main (fill), bottom bar (1).
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    render_title(f, outer[0]);
    render_main(f, outer[1], app);
    render_bottom_bar(f, outer[2], app);
}

fn render_title(f: &mut Frame, area: Rect) {
    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            " task-master",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "   q quit  s spawn  p plan  x qa  r reset  a attach",
            Style::default().fg(Color::DarkGray),
        ),
    ]))
    .style(Style::default().bg(Color::Black));
    f.render_widget(title, area);
}

fn render_main(f: &mut Frame, area: Rect, app: &mut App) {
    // Split into left (worktree list) and right (actions).
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    render_worktree_list(f, cols[0], app);
    render_actions(f, cols[1], app);
}

fn render_worktree_list(f: &mut Frame, area: Rect, app: &mut App) {
    let items: Vec<ListItem> = app
        .worktrees
        .iter()
        .enumerate()
        .map(|(i, wt)| {
            let phase = app.phases.get(i).map(|s| s.as_str()).unwrap_or("?");
            let phase_color = phase_color(phase);
            let line = Line::from(vec![
                Span::raw(format!("{:<20}", wt.window_name)),
                Span::styled(format!("[{}]", phase), Style::default().fg(phase_color)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Worktrees ")
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    f.render_stateful_widget(list, area, &mut app.list_state);
}

fn render_actions(f: &mut Frame, area: Rect, app: &App) {
    let phase = app.selected_phase().to_string();
    let active = App::is_active_phase(&phase);
    let has_wt = !app.worktrees.is_empty();

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(""));

    let spawn_warn = if active { "  ⚠ resets window" } else { "" };
    let spawn_label = format!("spawn agent{}", spawn_warn);
    let plan_label = format!("plan{}", spawn_warn);

    lines.push(action_line(
        's',
        &spawn_label,
        has_wt,
        active,
        false, // spawn is always enabled when worktrees exist
        true,
    ));

    // plan
    lines.push(action_line('p', &plan_label, has_wt, active, false, true));

    lines.push(Line::from(""));

    // qa — only active windows
    lines.push(action_line(
        'x',
        "qa  (enter PR #)",
        has_wt,
        active,
        !active,
        true,
    ));

    lines.push(Line::from(""));

    // reset — only active windows
    lines.push(action_line(
        'r',
        "reset window",
        has_wt,
        active,
        !active,
        true,
    ));

    // attach — only active windows
    lines.push(action_line(
        'a',
        "attach  (leaves TUI)",
        has_wt,
        active,
        !active,
        true,
    ));

    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        "  q  quit",
        Style::default().fg(Color::DarkGray),
    )]));

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Actions ")
        .border_style(Style::default().fg(Color::DarkGray));

    let para = Paragraph::new(lines).block(block);
    f.render_widget(para, area);
}

/// Build a single action line.
///
/// `disabled` overrides everything and renders the line grayed out.
/// `enabled` is the general enabled flag (e.g. has worktrees).
fn action_line<'a>(
    key: char,
    label: &'a str,
    _enabled: bool,
    _active: bool,
    disabled: bool,
    _show_active_warn: bool,
) -> Line<'a> {
    let label = label.to_string();
    if disabled {
        Line::from(vec![
            Span::styled("  -  ", Style::default().fg(Color::DarkGray)),
            Span::styled(label, Style::default().fg(Color::DarkGray)),
        ])
    } else {
        Line::from(vec![
            Span::styled(
                format!("  {}  ", key),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(label, Style::default().fg(Color::White)),
        ])
    }
}

fn render_bottom_bar(f: &mut Frame, area: Rect, app: &App) {
    let content = match &app.mode {
        Mode::Prompt(ActionKind::Spawn) => {
            format!(" Prompt: {}_", app.input_buf)
        }
        Mode::Prompt(ActionKind::Plan) => {
            format!(" Task: {}_", app.input_buf)
        }
        Mode::Prompt(ActionKind::Qa) => {
            format!(" PR number: {}_", app.input_buf)
        }
        Mode::ForceConfirm => {
            " Press Enter to force-spawn (discards uncommitted changes), Esc to cancel".to_string()
        }
        Mode::Normal => {
            // Show status message if recent, else stats.
            if let Some(msg) = app.current_status() {
                format!(" {}", msg)
            } else {
                stats_bar_text(app)
            }
        }
    };

    let style = match &app.mode {
        Mode::Normal => Style::default().fg(Color::DarkGray),
        Mode::ForceConfirm => Style::default().fg(Color::Red),
        _ => Style::default().fg(Color::Green),
    };

    let para = Paragraph::new(content).style(style);
    f.render_widget(para, area);
}

fn stats_bar_text(app: &App) -> String {
    let idx = match app.selected() {
        Some(i) => i,
        None => return " No worktrees".to_string(),
    };
    let wt = match app.worktrees.get(idx) {
        Some(w) => w,
        None => return String::new(),
    };
    match app.stats_cache.get(&idx) {
        Some(stats) if stats.input > 0 || stats.sessions > 0 => {
            format!(
                " {}  ·  {} in / {} out  ·  {} sessions",
                wt.window_name,
                format_tokens(stats.input),
                format_tokens(stats.output),
                stats.sessions,
            )
        }
        _ => format!(" {}  ·  no usage data", wt.window_name),
    }
}

fn phase_color(phase: &str) -> Color {
    match phase {
        "dev" => Color::Yellow,
        "plan" => Color::Cyan,
        "qa" => Color::Cyan,
        "review" | "ready" => Color::Green,
        "idle" | "?" | "" => Color::DarkGray,
        p if p.ends_with("stalled") => Color::Red,
        p if p == "blocked" => Color::Red,
        _ => Color::White,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_phase_color_known_phases() {
        assert_eq!(phase_color("dev"), Color::Yellow);
        assert_eq!(phase_color("qa"), Color::Cyan);
        assert_eq!(phase_color("plan"), Color::Cyan);
        assert_eq!(phase_color("review"), Color::Green);
        assert_eq!(phase_color("ready"), Color::Green);
        assert_eq!(phase_color("blocked"), Color::Red);
        assert_eq!(phase_color("idle"), Color::DarkGray);
        assert_eq!(phase_color("?"), Color::DarkGray);
    }

    #[test]
    fn test_phase_color_stalled_variants() {
        assert_eq!(phase_color("dev-stalled"), Color::Red);
        assert_eq!(phase_color("qa-stalled"), Color::Red);
        assert_eq!(phase_color("plan-stalled"), Color::Red);
    }

    #[test]
    fn test_is_active_phase() {
        assert!(!App::is_active_phase("idle"));
        assert!(!App::is_active_phase("?"));
        assert!(!App::is_active_phase(""));
        assert!(App::is_active_phase("dev"));
        assert!(App::is_active_phase("qa"));
        assert!(App::is_active_phase("review"));
        assert!(App::is_active_phase("blocked"));
        assert!(App::is_active_phase("dev-stalled"));
    }

    #[test]
    fn test_app_move_up_wraps() {
        use crate::registry::Registry;
        use std::path::PathBuf;

        let toml = r#"
[[projects]]
name = "svc"
short = "S"
repo = "projects/svc"
[[projects.worktrees]]
name = "a"
[[projects.worktrees]]
name = "b"
"#;
        let reg = Registry::load_from_str(toml, PathBuf::from("/base")).unwrap();
        let mut app = App::new(&reg, "test".to_string());
        app.list_state.select(Some(0));
        app.move_up();
        assert_eq!(app.selected(), Some(1)); // wraps to last
    }

    #[test]
    fn test_app_move_down_wraps() {
        use crate::registry::Registry;
        use std::path::PathBuf;

        let toml = r#"
[[projects]]
name = "svc"
short = "S"
repo = "projects/svc"
[[projects.worktrees]]
name = "a"
[[projects.worktrees]]
name = "b"
"#;
        let reg = Registry::load_from_str(toml, PathBuf::from("/base")).unwrap();
        let mut app = App::new(&reg, "test".to_string());
        app.list_state.select(Some(1));
        app.move_down();
        assert_eq!(app.selected(), Some(0)); // wraps to first
    }

    #[test]
    fn test_status_message_expires() {
        use crate::registry::Registry;
        use std::path::PathBuf;

        let toml = r#"
[[projects]]
name = "svc"
short = "S"
repo = "projects/svc"
[[projects.worktrees]]
name = "a"
"#;
        let reg = Registry::load_from_str(toml, PathBuf::from("/base")).unwrap();
        let mut app = App::new(&reg, "test".to_string());

        // Fresh message is visible.
        app.set_status("hello");
        assert_eq!(app.current_status(), Some("hello"));

        // Artificially age it past the display window.
        if let Some((_, ref mut at)) = app.status_msg {
            *at = Instant::now() - Duration::from_secs(10);
        }
        assert_eq!(app.current_status(), None);
    }

    #[test]
    fn test_stats_bar_text_no_data() {
        use crate::registry::Registry;
        use std::path::PathBuf;

        let toml = r#"
[[projects]]
name = "svc"
short = "S"
repo = "projects/svc"
[[projects.worktrees]]
name = "alpha"
"#;
        let reg = Registry::load_from_str(toml, PathBuf::from("/base")).unwrap();
        let app = App::new(&reg, "test".to_string());
        let text = stats_bar_text(&app);
        assert!(text.contains("S-alpha"));
        assert!(text.contains("no usage data"));
    }
}
