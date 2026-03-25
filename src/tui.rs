use crate::registry::Registry;
use crate::registry::Worktree;
use crate::stats::{fetch_stats, StatsRow};
use crate::status::find_live_phase;
use crate::tmux;
use crate::ui::theme::{Theme, ALL_THEMES};
use anyhow::{Context, Result};
use crossterm::{
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, widgets::ListState, Frame, Terminal};
use std::{
    collections::HashMap,
    io,
    time::{Duration, Instant},
};

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum ActionKind {
    Spawn,
    Plan,
    Qa,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Normal,
    Prompt(ActionKind),
    /// Spawn on an active window: user confirmed once, needs Enter again to force.
    ForceConfirm,
}

pub struct App {
    pub worktrees: Vec<Worktree>,
    pub phases: Vec<String>,
    pub list_state: ListState,
    pub mode: Mode,
    pub input_buf: String,
    /// Stats cache keyed by worktree index; filled lazily on selection change.
    pub stats_cache: HashMap<usize, StatsRow>,
    /// Transient status message and when it was set.
    pub status_msg: Option<(String, Instant)>,
    pub session: String,
    pub should_quit: bool,
    /// Track which index was active when we last loaded stats.
    pub last_stats_idx: Option<usize>,

    // ── Theme ─────────────────────────────────────────────────────────────────
    pub theme: Theme,

    /// Timestamp of the last key event — used to distinguish a deliberate Enter
    /// from a newline that arrived as part of a paste burst.
    pub last_key_at: Instant,

    // ── Overlays ──────────────────────────────────────────────────────────────
    pub show_theme_picker: bool,
    pub show_help: bool,
    pub theme_picker_cursor: usize,
    pub theme_picker_original: Option<Theme>,
    /// The theme id that is currently written to config (used for the ✓ in the picker).
    pub saved_theme_id: String,
}

impl App {
    pub fn new(registry: &Registry, session: String) -> Self {
        let worktrees = registry.worktrees.clone();
        let count = worktrees.len();
        let mut list_state = ListState::default();
        if count > 0 {
            list_state.select(Some(0));
        }
        let theme_name = &registry.ui.theme;
        let theme = Theme::from_name(theme_name);
        let theme_picker_cursor = Theme::index_of(theme_name);

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
            theme,
            show_theme_picker: false,
            show_help: false,
            theme_picker_cursor,
            theme_picker_original: None,
            saved_theme_id: theme_name.clone(),
            last_key_at: Instant::now(),
        }
    }

    pub fn selected(&self) -> Option<usize> {
        self.list_state.selected()
    }

    pub fn selected_phase(&self) -> &str {
        match self.selected() {
            Some(i) => self.phases.get(i).map(|s| s.as_str()).unwrap_or("?"),
            None => "?",
        }
    }

    pub fn is_active_phase(phase: &str) -> bool {
        !matches!(phase, "idle" | "?" | "")
    }

    pub fn move_up(&mut self) {
        let len = self.worktrees.len();
        if len == 0 {
            return;
        }
        let i = self.selected().unwrap_or(0);
        self.list_state
            .select(Some(if i == 0 { len - 1 } else { i - 1 }));
    }

    pub fn move_down(&mut self) {
        let len = self.worktrees.len();
        if len == 0 {
            return;
        }
        let i = self.selected().unwrap_or(0);
        self.list_state.select(Some((i + 1) % len));
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_msg = Some((msg.into(), Instant::now()));
    }

    /// Returns the current status message if it's still within the display window.
    pub fn current_status(&self) -> Option<&str> {
        self.status_msg.as_ref().and_then(|(msg, at)| {
            if at.elapsed() < Duration::from_secs(4) {
                Some(msg.as_str())
            } else {
                None
            }
        })
    }

    pub fn refresh_phases(&mut self) {
        for (i, wt) in self.worktrees.iter().enumerate() {
            let phase = find_live_phase(&self.session, &wt.window_name)
                .unwrap_or_else(|| "idle".to_string());
            self.phases[i] = phase;
        }
    }

    pub fn load_stats_for_selected(&mut self) {
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

    // ── Theme picker ──────────────────────────────────────────────────────────

    pub fn open_theme_picker(&mut self) {
        self.theme_picker_original = Some(self.theme.clone());
        self.theme_picker_cursor = Theme::index_of(
            ALL_THEMES
                .iter()
                .find(|(_, name)| Theme::from_name(name).border == self.theme.border)
                .map(|(id, _)| *id)
                .unwrap_or("tokyonight"),
        );
        // Sync cursor to current theme by id comparison.
        let current_name = ALL_THEMES
            .get(self.theme_picker_cursor)
            .map(|(id, _)| *id)
            .unwrap_or("tokyonight");
        let _ = current_name;
        self.show_theme_picker = true;
    }

    pub fn theme_picker_move(&mut self, delta: i32) {
        let len = ALL_THEMES.len();
        if len == 0 {
            return;
        }
        self.theme_picker_cursor =
            ((self.theme_picker_cursor as i32 + delta).rem_euclid(len as i32)) as usize;
        // Live preview: apply theme immediately.
        self.theme = Theme::from_name(ALL_THEMES[self.theme_picker_cursor].0);
    }

    pub fn theme_picker_commit(&mut self, registry: &Registry) {
        let id = ALL_THEMES[self.theme_picker_cursor].0;
        // Persist to config (best effort — don't crash TUI on write failure).
        let _ = crate::registry::write_theme(&registry.base_dir, id);
        self.saved_theme_id = id.to_string();
        self.theme_picker_original = None;
        self.show_theme_picker = false;
    }

    pub fn theme_picker_revert(&mut self) {
        if let Some(original) = self.theme_picker_original.take() {
            self.theme = original;
        }
        self.show_theme_picker = false;
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Open (or focus) a dedicated `task-master` tmux window and run the TUI there.
pub fn cmd_tui(registry: &Registry) -> Result<()> {
    let session = tmux::current_session()
        .context("task-master tui must be run from within a tmux session")?;

    let base_dir = registry.base_dir.to_string_lossy().to_string();
    ensure_tui_window(&session, &base_dir)?;

    let mut app = App::new(registry, session.clone());
    app.refresh_phases();
    app.load_stats_for_selected();

    enable_raw_mode().context("Failed to enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("Failed to enter alternate screen")?;
    execute!(stdout, EnableBracketedPaste).context("Failed to enable bracketed paste")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("Failed to create terminal")?;

    let res = run_loop(&mut terminal, &mut app, registry);

    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), DisableBracketedPaste).ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    res
}

fn ensure_tui_window(session: &str, working_dir: &str) -> Result<()> {
    use std::process::Command;

    let window_name = "task-master";

    if tmux::find_window_index(session, window_name).is_none() {
        // Create the window at the end, then move it to slot 1.
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
        // Move it to slot 1, shifting any existing windows up.
        let src_target = format!("{}:{}", session, window_name);
        let dst_target = format!("{}:1", session);
        Command::new("tmux")
            .args(["move-window", "-r", "-s", &src_target, "-t", &dst_target])
            .status()
            .ok(); // best-effort; don't fail the whole TUI if renumbering fails
    }

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

        if event::poll(Duration::from_millis(2000))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    app.last_key_at = Instant::now();
                    handle_key(app, registry, key.code, key.modifiers)?;
                }
                Event::Paste(text) => {
                    if matches!(app.mode, Mode::Prompt(_) | Mode::ForceConfirm) {
                        app.input_buf.push_str(&text);
                    }
                }
                _ => {}
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

fn render(f: &mut Frame, app: &mut App) {
    crate::ui::render(f, app);
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
    // Theme picker consumes all keys when open.
    if app.show_theme_picker {
        return handle_theme_picker(app, registry, code);
    }
    // Help overlay: any key closes it.
    if app.show_help {
        app.show_help = false;
        return Ok(());
    }

    match &app.mode.clone() {
        Mode::Normal => handle_normal(app, registry, code),
        Mode::Prompt(kind) => handle_prompt(app, registry, code, kind.clone()),
        Mode::ForceConfirm => handle_force_confirm(app, registry, code),
    }
}

fn handle_theme_picker(app: &mut App, registry: &Registry, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Char('j') | KeyCode::Down => {
            app.theme_picker_move(1);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.theme_picker_move(-1);
        }
        KeyCode::Enter => {
            app.theme_picker_commit(registry);
        }
        KeyCode::Esc => {
            app.theme_picker_revert();
        }
        _ => {}
    }
    Ok(())
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
        KeyCode::Char('t') => {
            app.open_theme_picker();
        }
        KeyCode::Char('?') => {
            app.show_help = !app.show_help;
        }
        KeyCode::Char('s') => {
            if app.worktrees.is_empty() {
                return Ok(());
            }
            let phase = app.selected_phase().to_string();
            if App::is_active_phase(&phase) {
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
                    let full_name = format!("{}:{}", wt.window_name, phase);
                    attach_to_window(&app.session, &wt.window_name, &full_name);
                }
            }
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
            // If a key arrived within 20 ms of the previous one we're almost
            // certainly inside a paste burst — treat the newline as literal text
            // rather than a submit trigger.
            if app.last_key_at.elapsed() < Duration::from_millis(20) {
                app.input_buf.push('\n');
            } else {
                execute_action(app, registry, &kind, false)?;
            }
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
            let _ = tmux::select_tui_window(&app.session);
            app.set_status(format!("Spawned {}:dev", wt_name));
            app.mode = Mode::Normal;
            app.input_buf.clear();
            app.refresh_phases();
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("uncommitted changes") && !force {
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
            let _ = tmux::select_tui_window(&app.session);
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
            let _ = tmux::select_tui_window(&app.session);
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
    let target_full = format!("{}:{}", session, full_name);
    let status = Command::new("tmux")
        .args(["select-window", "-t", &target_full])
        .status();
    if status.map(|s| s.success()).unwrap_or(false) {
        return;
    }
    if let Some(idx) = tmux::find_window_index(session, base_name) {
        let target = format!("{}:{}", session, idx);
        Command::new("tmux")
            .args(["select-window", "-t", &target])
            .status()
            .ok();
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
        let t = Theme::tokyonight();
        assert_eq!(t.phase_color("dev"), t.phase_dev);
        assert_eq!(t.phase_color("qa"), t.phase_plan_qa);
        assert_eq!(t.phase_color("plan"), t.phase_plan_qa);
        assert_eq!(t.phase_color("review"), t.phase_done);
        assert_eq!(t.phase_color("ready"), t.phase_done);
        assert_eq!(t.phase_color("blocked"), t.phase_error);
        assert_eq!(t.phase_color("idle"), t.phase_idle);
        assert_eq!(t.phase_color("?"), t.phase_idle);
    }

    #[test]
    fn test_phase_color_stalled_variants() {
        let t = Theme::tokyonight();
        assert_eq!(t.phase_color("dev-stalled"), t.phase_error);
        assert_eq!(t.phase_color("qa-stalled"), t.phase_error);
        assert_eq!(t.phase_color("plan-stalled"), t.phase_error);
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

        app.set_status("hello");
        assert_eq!(app.current_status(), Some("hello"));

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
        // Verify the worktree name and empty stats cache.
        let idx = app.selected().unwrap_or(0);
        let wt = &app.worktrees[idx];
        assert!(wt.window_name.contains("S-alpha"));
        assert!(app.stats_cache.get(&idx).is_none());
    }
}
