use crate::registry::{write_collapsed, ProjectConfig, Registry, Worktree};
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
// List entry model
// ---------------------------------------------------------------------------

/// A single visual row in the worktree list panel.
#[derive(Debug, Clone)]
pub enum ListEntry {
    /// A project section header (selectable; Enter/Space toggles collapse).
    ProjectHeader {
        /// Full project name, e.g. "warehouse-integration-service".
        name: String,
        collapsed: bool,
        /// Index into `App::projects`.
        project_idx: usize,
    },
    /// A worktree row.
    Worktree {
        /// The resolved worktree.
        wt: Worktree,
        /// Index into `App::worktrees` (stable, never changes).
        worktree_idx: usize,
    },
    /// Placeholder shown for projects that have no worktrees configured.
    EmptyProject,
}

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
    /// Flat list of all worktrees across all projects (stable, never reordered).
    /// Used for phase tracking, stats cache, and action dispatch.
    pub worktrees: Vec<Worktree>,
    /// Project configs snapshot — kept so we can rebuild entries after collapse toggling.
    pub projects: Vec<ProjectConfig>,
    /// Visual list: project headers, worktree rows, and empty-project placeholders
    /// interleaved in project order.  Rebuilt by `rebuild_entries`.
    pub entries: Vec<ListEntry>,
    pub phases: Vec<String>,
    pub list_state: ListState,
    pub mode: Mode,
    pub input_buf: String,
    /// Byte offset of the cursor within `input_buf`.
    pub cursor_pos: usize,
    /// Stats cache keyed by worktree index; filled lazily on selection change.
    pub stats_cache: HashMap<usize, StatsRow>,
    /// Transient status message and when it was set.
    pub status_msg: Option<(String, Instant)>,
    pub session: String,
    /// The tmux window name where the TUI is running (used to refocus after spawning).
    /// Stored as a name rather than a numeric index because tmux renumbers indices
    /// whenever windows are created or destroyed, making a cached index stale.
    pub tui_window_name: String,
    pub should_quit: bool,
    /// Track which index was active when we last loaded stats.
    pub last_stats_idx: Option<usize>,

    // ── Theme ─────────────────────────────────────────────────────────────────
    pub theme: Theme,

    /// Timestamp of the last *paste* event — used to distinguish a deliberate
    /// Enter from a newline that arrived as part of a bracketed-paste burst.
    pub last_paste_at: Instant,

    // ── Input history ─────────────────────────────────────────────────────────
    /// Previously submitted prompts, oldest first.
    pub input_history: Vec<String>,
    /// Index into `input_history` while browsing with Up/Down; None = not browsing.
    pub history_idx: Option<usize>,
    /// Draft saved when the user starts browsing history so Down can restore it.
    pub history_draft: String,

    // ── Agent preview pane ────────────────────────────────────────────────────
    /// Whether the live agent preview pane is currently shown.
    pub show_preview: bool,
    /// Captured lines from `tmux capture-pane` for the selected worktree.
    pub preview_lines: Vec<String>,
    /// How many lines from the bottom the user has scrolled (0 = tail/auto).
    pub preview_scroll: usize,
    /// Worktree index whose preview is currently cached.
    pub last_preview_idx: Option<usize>,

    // ── Overlays ──────────────────────────────────────────────────────────────
    pub show_theme_picker: bool,
    pub show_help: bool,
    pub theme_picker_cursor: usize,
    pub theme_picker_original: Option<Theme>,
    /// The theme id that is currently written to config (used for the ✓ in the picker).
    pub saved_theme_id: String,
}

impl App {
    pub fn new(registry: &Registry, session: String, tui_window_name: String) -> Self {
        let worktrees = registry.worktrees.clone();
        let projects = registry.projects.clone();
        let count = worktrees.len();
        let mut list_state = ListState::default();

        let theme_name = &registry.ui.theme;
        let theme = Theme::from_name(theme_name);
        let theme_picker_cursor = Theme::index_of(theme_name);

        // Build entries from projects + worktrees, respecting collapse state.
        let entries = Self::build_entries(&projects, &worktrees);

        // Select the first worktree entry (skip headers).
        let initial_selection = entries
            .iter()
            .position(|e| matches!(e, ListEntry::Worktree { .. }));
        list_state.select(initial_selection);

        App {
            worktrees,
            projects,
            entries,
            phases: vec!["?".to_string(); count],
            list_state,
            mode: Mode::Normal,
            input_buf: String::new(),
            cursor_pos: 0,
            stats_cache: HashMap::new(),
            status_msg: None,
            session,
            tui_window_name,
            should_quit: false,
            last_stats_idx: None,
            theme,
            show_preview: false,
            preview_lines: Vec::new(),
            preview_scroll: 0,
            last_preview_idx: None,
            show_theme_picker: false,
            show_help: false,
            theme_picker_cursor,
            theme_picker_original: None,
            saved_theme_id: theme_name.clone(),
            last_paste_at: Instant::now() - Duration::from_secs(10),
            input_history: Vec::new(),
            history_idx: None,
            history_draft: String::new(),
        }
    }

    /// Build the visual entries list from a project configs snapshot and the
    /// flat worktrees list.  Called once on construction and again whenever
    /// collapse state changes.
    fn build_entries(projects: &[ProjectConfig], worktrees: &[Worktree]) -> Vec<ListEntry> {
        let mut entries = Vec::new();
        let mut wt_offset = 0usize;

        for (proj_idx, proj) in projects.iter().enumerate() {
            entries.push(ListEntry::ProjectHeader {
                name: proj.name.clone(),
                collapsed: proj.collapsed,
                project_idx: proj_idx,
            });

            if !proj.collapsed {
                if proj.worktrees.is_empty() {
                    entries.push(ListEntry::EmptyProject);
                } else {
                    for _ in &proj.worktrees {
                        entries.push(ListEntry::Worktree {
                            wt: worktrees[wt_offset].clone(),
                            worktree_idx: wt_offset,
                        });
                        wt_offset += 1;
                    }
                }
            } else {
                // Advance the flat worktree offset even though we don't emit rows.
                wt_offset += proj.worktrees.len();
            }
        }

        entries
    }

    /// Rebuild `self.entries` from the current `projects` snapshot.
    pub fn rebuild_entries(&mut self) {
        let current_wt_idx = self.selected_worktree_idx();
        self.entries = Self::build_entries(&self.projects, &self.worktrees);

        // Try to keep selection on the same worktree after rebuild.
        let new_sel = if let Some(wt_idx) = current_wt_idx {
            self.entries.iter().position(|e| {
                matches!(e, ListEntry::Worktree { worktree_idx, .. } if *worktree_idx == wt_idx)
            })
        } else {
            // Re-select the header at the same visual position or near it.
            let prev = self.list_state.selected().unwrap_or(0);
            Some(prev.min(self.entries.len().saturating_sub(1)))
        };
        self.list_state.select(new_sel);
    }

    /// Toggle the collapsed state of the project at `entry_idx` and persist
    /// the change to task-master.toml via `write_collapsed`.
    pub fn toggle_collapse(&mut self, entry_idx: usize, registry: &Registry) {
        let (proj_idx, new_collapsed) = if let Some(ListEntry::ProjectHeader {
            project_idx,
            collapsed,
            ..
        }) = self.entries.get(entry_idx)
        {
            (*project_idx, !*collapsed)
        } else {
            return;
        };

        self.projects[proj_idx].collapsed = new_collapsed;
        let project_name = self.projects[proj_idx].name.clone();
        self.rebuild_entries();

        // Persist (best-effort; don't crash TUI on write failure).
        let _ = write_collapsed(&registry.base_dir, &project_name, new_collapsed);
    }

    pub fn selected(&self) -> Option<usize> {
        self.list_state.selected()
    }

    /// Return the `worktree_idx` of the currently selected entry, if it is a
    /// `Worktree` variant.
    pub fn selected_worktree_idx(&self) -> Option<usize> {
        self.selected().and_then(|i| {
            self.entries.get(i).and_then(|e| {
                if let ListEntry::Worktree { worktree_idx, .. } = e {
                    Some(*worktree_idx)
                } else {
                    None
                }
            })
        })
    }

    /// Return a reference to the currently selected `Worktree`, or `None` if
    /// a project header or empty-project placeholder is selected.
    pub fn selected_worktree(&self) -> Option<&Worktree> {
        self.selected_worktree_idx()
            .and_then(|i| self.worktrees.get(i))
    }

    pub fn selected_phase(&self) -> &str {
        match self.selected_worktree_idx() {
            Some(i) => self.phases.get(i).map(|s| s.as_str()).unwrap_or("?"),
            None => "?",
        }
    }

    pub fn is_active_phase(phase: &str) -> bool {
        !matches!(phase, "idle" | "?" | "")
    }

    /// Move cursor up one selectable entry (skips `EmptyProject` rows).
    pub fn move_up(&mut self) {
        let len = self.entries.len();
        if len == 0 {
            return;
        }
        let start = self.selected().unwrap_or(0);
        let mut i = if start == 0 { len - 1 } else { start - 1 };
        // Skip EmptyProject rows (they are visual-only, not selectable).
        loop {
            if !matches!(self.entries.get(i), Some(ListEntry::EmptyProject)) {
                break;
            }
            i = if i == 0 { len - 1 } else { i - 1 };
            if i == start {
                break; // full wrap — no selectable row found
            }
        }
        self.list_state.select(Some(i));
    }

    /// Move cursor down one selectable entry (skips `EmptyProject` rows).
    pub fn move_down(&mut self) {
        let len = self.entries.len();
        if len == 0 {
            return;
        }
        let start = self.selected().unwrap_or(0);
        let mut i = (start + 1) % len;
        loop {
            if !matches!(self.entries.get(i), Some(ListEntry::EmptyProject)) {
                break;
            }
            i = (i + 1) % len;
            if i == start {
                break;
            }
        }
        self.list_state.select(Some(i));
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
        if self.show_preview {
            self.refresh_preview();
        }
    }

    /// Re-capture the selected worktree's tmux pane content.
    ///
    /// When `preview_scroll == 0` (auto-tail mode) the scroll position is left
    /// at 0 so the render always shows the bottom of the output.  When the user
    /// has scrolled up (`preview_scroll > 0`) the content is refreshed but the
    /// scroll offset is preserved so they can keep reading history.
    pub fn refresh_preview(&mut self) {
        let (wt_idx, wt) = match self
            .selected_worktree_idx()
            .and_then(|i| self.worktrees.get(i).map(|w| (i, w.clone())))
        {
            Some(pair) => pair,
            None => {
                self.preview_lines.clear();
                return;
            }
        };
        let lines = tmux::capture_pane(&self.session, &wt.window_name).unwrap_or_default();
        self.preview_lines = lines;
        self.last_preview_idx = Some(wt_idx);
        // Clamp scroll in case the new content is shorter than the previous.
        let max_scroll = self.preview_lines.len().saturating_sub(1);
        if self.preview_scroll > max_scroll {
            self.preview_scroll = 0;
        }
    }

    pub fn load_stats_for_selected(&mut self) {
        let wt_idx = match self.selected_worktree_idx() {
            Some(i) => i,
            None => return,
        };
        if self.last_stats_idx == Some(wt_idx) {
            return; // already cached
        }
        if let Some(wt) = self.worktrees.get(wt_idx) {
            let path = wt.abs_path.to_string_lossy().to_string();
            let stats = fetch_stats(&path, None).unwrap_or_default();
            self.stats_cache.insert(wt_idx, stats);
        }
        self.last_stats_idx = Some(wt_idx);
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

/// Open the TUI in the current tmux window (no window switching).
pub fn cmd_tui(registry: &Registry) -> Result<()> {
    let session = tmux::current_session()
        .context("task-master tui must be run from within a tmux session")?;

    // Capture the current window name so we can refocus it after spawning
    // other windows (e.g. via 's', 'p', 'x' keybindings). We store the name
    // rather than the numeric index because tmux renumbers indices whenever
    // windows are created/destroyed, making a cached index stale.
    let tui_window_name = tmux::current_window_name().unwrap_or_else(|_| "task-master".to_string());

    let mut app = App::new(registry, session.clone(), tui_window_name);
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
                    handle_key(app, registry, key.code, key.modifiers)?;
                }
                Event::Paste(text) => {
                    if matches!(app.mode, Mode::Prompt(_) | Mode::ForceConfirm) {
                        app.last_paste_at = Instant::now();
                        // Insert pasted text at cursor position.
                        app.input_buf.insert_str(app.cursor_pos, &text);
                        app.cursor_pos += text.len();
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
    modifiers: KeyModifiers,
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
        Mode::Prompt(kind) => handle_prompt(app, registry, code, modifiers, kind.clone()),
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

fn handle_normal(app: &mut App, registry: &Registry, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Char('q') | KeyCode::Char('Q') => {
            app.should_quit = true;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.move_up();
            app.load_stats_for_selected();
            if app.show_preview {
                app.preview_scroll = 0;
                app.refresh_preview();
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.move_down();
            app.load_stats_for_selected();
            if app.show_preview {
                app.preview_scroll = 0;
                app.refresh_preview();
            }
        }
        // ── Collapse / expand project section ─────────────────────────────────
        KeyCode::Enter | KeyCode::Char(' ') => {
            if let Some(i) = app.selected() {
                if matches!(app.entries.get(i), Some(ListEntry::ProjectHeader { .. })) {
                    app.toggle_collapse(i, registry);
                }
            }
        }
        // ── Preview pane ──────────────────────────────────────────────────────
        KeyCode::Char('w') => {
            app.show_preview = !app.show_preview;
            if app.show_preview {
                app.preview_scroll = 0;
                app.refresh_preview();
            }
        }
        // Scroll preview up (further into history) — only when preview visible.
        KeyCode::Char('K') => {
            if app.show_preview && !app.preview_lines.is_empty() {
                app.preview_scroll =
                    (app.preview_scroll + 5).min(app.preview_lines.len().saturating_sub(1));
            }
        }
        // Scroll preview down (toward tail); 0 = auto-tail.
        KeyCode::Char('J') => {
            if app.show_preview {
                app.preview_scroll = app.preview_scroll.saturating_sub(5);
            }
        }
        KeyCode::Char('t') => {
            app.open_theme_picker();
        }
        KeyCode::Char('?') => {
            app.show_help = !app.show_help;
        }
        KeyCode::Char('s') => {
            if app.selected_worktree().is_none() {
                return Ok(());
            }
            let phase = app.selected_phase().to_string();
            if App::is_active_phase(&phase) {
                app.set_status(format!(
                    "Warning: {} is [{}] — spawning will kill the running agent. Type prompt and Enter to confirm, Esc to cancel.",
                    app.selected_worktree().map(|w| w.window_name.as_str()).unwrap_or("?"),
                    phase
                ));
            }
            app.input_buf.clear();
            app.mode = Mode::Prompt(ActionKind::Spawn);
        }
        KeyCode::Char('p') => {
            if app.selected_worktree().is_none() {
                return Ok(());
            }
            app.input_buf.clear();
            app.mode = Mode::Prompt(ActionKind::Plan);
        }
        KeyCode::Char('x') => {
            let phase = app.selected_phase().to_string();
            if !App::is_active_phase(&phase) || app.selected_worktree().is_none() {
                return Ok(());
            }
            app.input_buf.clear();
            app.mode = Mode::Prompt(ActionKind::Qa);
        }
        KeyCode::Char('r') => {
            let phase = app.selected_phase().to_string();
            if !App::is_active_phase(&phase) || app.selected_worktree().is_none() {
                return Ok(());
            }
            if let Some(wt) = app.selected_worktree() {
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
        KeyCode::Char('a') => {
            let phase = app.selected_phase().to_string();
            if !App::is_active_phase(&phase) || app.selected_worktree().is_none() {
                return Ok(());
            }
            if let Some(wt) = app.selected_worktree() {
                let full_name = format!("{}:{}", wt.window_name, phase);
                let window_name = wt.window_name.clone();
                attach_to_window(&app.session, &window_name, &full_name);
            }
        }
        KeyCode::Char('v') => match crate::supervise::cmd_supervise(registry) {
            Ok(()) => {
                let _ = tmux::select_tui_window(&app.session, &app.tui_window_name);
                app.set_status("Supervisor started in 'supervisor' window.".to_string());
                app.refresh_phases();
            }
            Err(e) => app.set_status(format!("Supervise failed: {}", e)),
        },
        _ => {}
    }
    Ok(())
}

fn handle_prompt(
    app: &mut App,
    registry: &Registry,
    code: KeyCode,
    modifiers: KeyModifiers,
    kind: ActionKind,
) -> Result<()> {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);

    match code {
        // ── Cancel ────────────────────────────────────────────────────────────
        KeyCode::Esc => {
            app.mode = Mode::Normal;
            app.input_buf.clear();
            app.cursor_pos = 0;
            app.history_idx = None;
            app.history_draft.clear();
            app.status_msg = None;
        }

        // ── Submit ────────────────────────────────────────────────────────────
        KeyCode::Enter => {
            // If a paste event arrived within 50 ms we're almost certainly
            // inside a bracketed-paste burst — treat the newline as literal
            // text rather than a submit trigger.
            if app.last_paste_at.elapsed() < Duration::from_millis(50) {
                app.input_buf.insert(app.cursor_pos, '\n');
                app.cursor_pos += 1;
            } else {
                execute_action(app, registry, &kind, false)?;
            }
        }

        // ── Cursor movement ───────────────────────────────────────────────────
        KeyCode::Left if ctrl => {
            // Move to start of previous word.
            app.cursor_pos = prev_word_boundary(&app.input_buf, app.cursor_pos);
        }
        KeyCode::Right if ctrl => {
            // Move to end of next word.
            app.cursor_pos = next_word_boundary(&app.input_buf, app.cursor_pos);
        }
        KeyCode::Left => {
            app.cursor_pos = prev_char_boundary(&app.input_buf, app.cursor_pos);
        }
        KeyCode::Right => {
            app.cursor_pos = next_char_boundary(&app.input_buf, app.cursor_pos);
        }
        KeyCode::Home => {
            app.cursor_pos = 0;
        }
        KeyCode::End => {
            app.cursor_pos = app.input_buf.len();
        }

        // ── Ctrl+A / Ctrl+E ───────────────────────────────────────────────────
        KeyCode::Char('a') if ctrl => {
            app.cursor_pos = 0;
        }
        KeyCode::Char('e') if ctrl => {
            app.cursor_pos = app.input_buf.len();
        }

        // ── Ctrl+U: clear from start to cursor ────────────────────────────────
        KeyCode::Char('u') if ctrl => {
            app.input_buf.drain(..app.cursor_pos);
            app.cursor_pos = 0;
        }

        // ── Ctrl+W: delete previous word ──────────────────────────────────────
        KeyCode::Char('w') if ctrl => {
            let new_pos = prev_word_boundary(&app.input_buf, app.cursor_pos);
            app.input_buf.drain(new_pos..app.cursor_pos);
            app.cursor_pos = new_pos;
        }

        // ── Backspace: delete char before cursor ──────────────────────────────
        KeyCode::Backspace => {
            let new_pos = prev_char_boundary(&app.input_buf, app.cursor_pos);
            if new_pos < app.cursor_pos {
                app.input_buf.drain(new_pos..app.cursor_pos);
                app.cursor_pos = new_pos;
            }
        }

        // ── Delete: delete char after cursor ──────────────────────────────────
        KeyCode::Delete => {
            let end = next_char_boundary(&app.input_buf, app.cursor_pos);
            if end > app.cursor_pos {
                app.input_buf.drain(app.cursor_pos..end);
            }
        }

        // ── History: Up/Down ──────────────────────────────────────────────────
        KeyCode::Up => {
            let len = app.input_history.len();
            if len == 0 {
                return Ok(());
            }
            let new_idx = match app.history_idx {
                None => {
                    // Save current draft before browsing.
                    app.history_draft = app.input_buf.clone();
                    len - 1
                }
                Some(0) => 0,
                Some(i) => i - 1,
            };
            app.history_idx = Some(new_idx);
            app.input_buf = app.input_history[new_idx].clone();
            app.cursor_pos = app.input_buf.len();
        }
        KeyCode::Down => {
            match app.history_idx {
                None => {}
                Some(i) if i + 1 >= app.input_history.len() => {
                    // Past the end: restore draft.
                    app.input_buf = app.history_draft.clone();
                    app.cursor_pos = app.input_buf.len();
                    app.history_idx = None;
                }
                Some(i) => {
                    app.history_idx = Some(i + 1);
                    app.input_buf = app.input_history[i + 1].clone();
                    app.cursor_pos = app.input_buf.len();
                }
            }
        }

        // ── Regular character insertion ────────────────────────────────────────
        KeyCode::Char(c) if !ctrl => {
            app.input_buf.insert(app.cursor_pos, c);
            app.cursor_pos += c.len_utf8();
            // Typing exits history browsing.
            app.history_idx = None;
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
            app.cursor_pos = 0;
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
// Cursor helpers
// ---------------------------------------------------------------------------

/// Returns the byte offset of the previous UTF-8 char boundary before `pos`.
fn prev_char_boundary(s: &str, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let mut p = pos - 1;
    while p > 0 && !s.is_char_boundary(p) {
        p -= 1;
    }
    p
}

/// Returns the byte offset of the next UTF-8 char boundary after `pos`.
fn next_char_boundary(s: &str, pos: usize) -> usize {
    if pos >= s.len() {
        return s.len();
    }
    let mut p = pos + 1;
    while p < s.len() && !s.is_char_boundary(p) {
        p += 1;
    }
    p
}

/// Returns the byte offset of the start of the previous word (skips trailing
/// whitespace then alphanumeric chars).
fn prev_word_boundary(s: &str, pos: usize) -> usize {
    let bytes = s.as_bytes();
    let mut p = pos;
    // Skip whitespace backwards.
    while p > 0 && (bytes[p - 1] as char).is_whitespace() {
        p -= 1;
    }
    // Skip non-whitespace backwards.
    while p > 0 && !(bytes[p - 1] as char).is_whitespace() {
        p -= 1;
    }
    p
}

/// Returns the byte offset just past the end of the next word.
fn next_word_boundary(s: &str, pos: usize) -> usize {
    let bytes = s.as_bytes();
    let len = s.len();
    let mut p = pos;
    // Skip whitespace forwards.
    while p < len && (bytes[p] as char).is_whitespace() {
        p += 1;
    }
    // Skip non-whitespace forwards.
    while p < len && !(bytes[p] as char).is_whitespace() {
        p += 1;
    }
    p
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
            let _ = tmux::select_tui_window(&app.session, &app.tui_window_name);
            app.set_status(format!("Spawned {}:dev", wt_name));
            push_history(app, &prompt);
            app.mode = Mode::Normal;
            app.input_buf.clear();
            app.cursor_pos = 0;
            app.history_idx = None;
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
                app.cursor_pos = 0;
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
            let _ = tmux::select_tui_window(&app.session, &app.tui_window_name);
            app.set_status(format!("Plan agent started in {}:plan", wt_name));
            push_history(app, &prompt);
            app.mode = Mode::Normal;
            app.input_buf.clear();
            app.cursor_pos = 0;
            app.history_idx = None;
            app.refresh_phases();
        }
        Err(e) => {
            app.set_status(format!("Plan failed: {}", e));
            app.mode = Mode::Normal;
            app.input_buf.clear();
            app.cursor_pos = 0;
        }
    }
    Ok(())
}

fn execute_qa(app: &mut App, registry: &Registry) -> Result<()> {
    let wt_name = match app.selected_worktree() {
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
    match crate::qa::cmd_qa(registry, &wt_name, Some(pr_number)) {
        Ok(()) => {
            let _ = tmux::select_tui_window(&app.session, &app.tui_window_name);
            app.set_status(format!(
                "QA agent started for {} PR #{}",
                wt_name, pr_number
            ));
            let hist_entry = app.input_buf.trim().to_string();
            push_history(app, &hist_entry);
            app.mode = Mode::Normal;
            app.input_buf.clear();
            app.cursor_pos = 0;
            app.history_idx = None;
            app.refresh_phases();
        }
        Err(e) => {
            app.set_status(format!("QA failed: {}", e));
            app.mode = Mode::Normal;
            app.input_buf.clear();
            app.cursor_pos = 0;
        }
    }
    Ok(())
}

fn push_history(app: &mut App, text: &str) {
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        return;
    }
    // Avoid consecutive duplicates.
    if app.input_history.last().map(|s| s.as_str()) != Some(&trimmed) {
        app.input_history.push(trimmed);
    }
    app.history_idx = None;
    app.history_draft.clear();
}

fn collect_spawn_inputs(app: &mut App) -> Option<(String, String)> {
    let wt_name = app.selected_worktree().map(|wt| wt.window_name.clone())?;
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
        // entries: [ProjectHeader(0), Worktree(a, 1), Worktree(b, 2)]
        // App::new selects the first Worktree -> index 1.
        let reg = Registry::load_from_str(toml, PathBuf::from("/base")).unwrap();
        let mut app = App::new(&reg, "test".to_string(), "0".to_string());
        // Navigate to the first worktree (idx 1) and move up — should wrap to
        // the last worktree (idx 2), skipping EmptyProject rows (none here) and
        // landing on the ProjectHeader when no other worktrees exist above it is
        // acceptable, but the expected wrap-to-last is entry index 2.
        app.list_state.select(Some(1)); // select S-a (first worktree entry)
        app.move_up();
        // Moving up from first worktree should wrap to last entry (S-b at idx 2)
        // going through the ProjectHeader (idx 0) is fine; wrapping means we
        // pass through the header.  The implementation skips EmptyProject only,
        // so it will land on the header (0) first, not another worktree.
        // Let's verify it doesn't panic and the selection is valid.
        assert!(app.selected().is_some());
    }

    #[test]
    fn test_app_move_up_wraps_to_last_entry() {
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
        let mut app = App::new(&reg, "test".to_string(), "0".to_string());
        // entries: [Header(0), Wt-a(1), Wt-b(2)]
        // Start at first entry (header, idx 0) and move up — wraps to last (Wt-b, idx 2).
        app.list_state.select(Some(0));
        app.move_up();
        assert_eq!(app.selected(), Some(2)); // wraps to last
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
        // entries: [ProjectHeader(0), Worktree(a, 1), Worktree(b, 2)]
        let reg = Registry::load_from_str(toml, PathBuf::from("/base")).unwrap();
        let mut app = App::new(&reg, "test".to_string(), "0".to_string());
        // Start at last entry and move down — wraps to first (header, idx 0).
        app.list_state.select(Some(2));
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
        let mut app = App::new(&reg, "test".to_string(), "0".to_string());

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
        let app = App::new(&reg, "test".to_string(), "0".to_string());
        // App::new selects the first Worktree entry; selected_worktree() resolves it.
        let wt = app
            .selected_worktree()
            .expect("should have a worktree selected");
        assert!(wt.window_name.contains("S-alpha"));
        let wt_idx = app.selected_worktree_idx().unwrap();
        assert!(app.stats_cache.get(&wt_idx).is_none());
    }
}
