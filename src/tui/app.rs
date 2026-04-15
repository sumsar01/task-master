use crate::registry::{write_collapsed, write_group_collapsed, ProjectConfig, Registry, Worktree};
use crate::stats::{fetch_stats, StatsRow};
use crate::status::find_live_phase;
use crate::tmux;
use crate::ui::theme::{Theme, ALL_THEMES};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

// ---------------------------------------------------------------------------
// List entry model
// ---------------------------------------------------------------------------

/// A single visual row in the worktree list panel.
#[derive(Debug, Clone)]
pub enum ListEntry {
    /// A super-group header row (selectable; Enter/Space toggles collapse).
    /// When collapsed, all projects and their worktrees in this group are hidden.
    GroupHeader {
        /// Group name, e.g. "Work" or "Personal".
        name: String,
        collapsed: bool,
    },
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
    Send,
    /// User typed a new (named) worktree name; Enter calls execute_add_worktree.
    AddWorktree,
    /// Multi-step prompt for adding a new project (name → short → url).
    AddProject,
    /// User typed an agent task prompt; Enter calls execute_spawn_ephemeral
    /// (worktree name is auto-generated).
    SpawnEphemeral,
}

/// Tracks which input step the add-project flow is on.
#[derive(Debug, Clone, PartialEq)]
pub enum AddProjectStep {
    /// Step 1: collecting the full project name (e.g. "warehouse-integration-service").
    Name,
    /// Step 2: collecting the short prefix (e.g. "WIS").
    Short,
    /// Step 3: collecting the git repo URL to clone.
    Url,
    /// Step 4: selecting the GitHub account to use for cloning.
    Account,
    /// Step 5: optional super-group label (e.g. "Whiteaway", "Personal").
    Group,
    /// Step 6: optional bounded-context tag (e.g. "fulfillment", "delivery-and-logistics").
    Context,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Normal,
    Prompt(ActionKind),
    /// Spawn on an active window: user confirmed once, needs Enter again to force.
    ForceConfirm,
    /// Confirm-close modal: user pressed 'c', waiting for 'y' or any other key.
    ConfirmClose,
    /// Confirm-remove-worktree modal: user pressed 'D', waiting for 'y' or any other key.
    ConfirmRemoveWorktree,
    /// Remove worktree has modified files: user confirmed once, Enter force-removes, Esc cancels.
    ForceConfirmRemoveWorktree,
    /// A long-running clone is running in a background thread.
    /// The TUI shows an animated spinner and ignores all key input until done.
    Cloning,
    /// Confirm-cleanup modal: user pressed 'X', waiting for 'y' or any other key.
    ConfirmCleanup,
}

pub struct App {
    /// Owned registry — kept live so all action dispatch (spawn, remove, etc.)
    /// always uses an up-to-date view of task-master.toml.
    /// Updated by `reload_from_registry` whenever the config changes.
    pub registry: Registry,
    /// Flat list of all worktrees across all projects (stable, never reordered).
    /// Used for phase tracking, stats cache, and action dispatch.
    pub worktrees: Vec<Worktree>,
    /// Project configs snapshot — kept so we can rebuild entries after collapse toggling.
    pub projects: Vec<ProjectConfig>,
    /// Persisted super-group collapse state, keyed by group name.
    /// A missing entry means the group is expanded (default).
    pub group_collapsed: HashMap<String, bool>,
    /// Visual list: group headers, project headers, worktree rows, and
    /// empty-project placeholders interleaved in order.  Rebuilt by `rebuild_entries`.
    pub entries: Vec<ListEntry>,
    pub phases: Vec<String>,
    pub list_state: ratatui::widgets::ListState,
    pub mode: Mode,
    pub input_buf: String,
    /// Byte offset of the cursor within `input_buf`.
    pub cursor_pos: usize,
    /// Stats cache keyed by worktree index; filled lazily on selection change.
    pub stats_cache: HashMap<usize, StatsRow>,
    /// Transient status message and when it was set.
    pub status_msg: Option<(String, Instant)>,
    pub session: String,
    /// The stable tmux window ID (e.g. `@3`) for the window running the TUI.
    ///
    /// This ID is assigned once at window creation and never changes, even when
    /// the window is renamed or other windows shift its numeric index. It is used
    /// by `select_window_by_id` to reliably re-focus the TUI after spawning or
    /// resetting worktree windows, avoiding the name-collision bug where a
    /// worktree whose base name equals the TUI window name would cause
    /// `find_window_index` to return the wrong window.
    pub tui_window_id: String,
    pub should_quit: bool,
    /// Track which index was active when we last loaded stats.
    pub last_stats_idx: Option<usize>,

    // ── Theme ─────────────────────────────────────────────────────────────────
    pub theme: Theme,

    /// Timestamp of the last *paste* event — used to distinguish a deliberate
    /// Enter from a newline that arrived as part of a bracketed-paste burst.
    pub last_paste_at: Instant,

    /// Timestamp of the last *key* event — used to detect rapid-fire key bursts
    /// that arrive when the terminal doesn't honor bracketed paste mode.
    pub last_key_at: Instant,

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

    // ── Worktree detail pane ──────────────────────────────────────────────────
    /// Whether the git-detail pane is currently shown.
    pub show_detail: bool,
    /// Rendered lines for the detail pane (branch, dirty count, recent commits).
    pub detail_lines: Vec<String>,
    /// Worktree index whose detail is currently cached.
    pub last_detail_idx: Option<usize>,

    // ── Overlays ──────────────────────────────────────────────────────────────
    pub show_theme_picker: bool,
    pub show_help: bool,
    pub theme_picker_cursor: usize,
    pub theme_picker_original: Option<Theme>,
    /// The theme id that is currently written to config (used for the ✓ in the picker).
    pub saved_theme_id: String,

    /// Vertical scroll offset for the prompt input area (in visual/wrapped lines).
    /// Adjusted automatically after every keystroke so the cursor line stays
    /// within the visible 8-row window.  Reset to 0 when the prompt is dismissed.
    pub prompt_scroll: usize,

    /// Last known terminal width, updated every render frame.  Used by
    /// `update_prompt_scroll` to compute word-wrap line counts without needing
    /// access to the Frame at event-handling time.
    pub terminal_width: u16,

    /// When `true`, the next frame will call `terminal.clear()` before drawing.
    ///
    /// Set after any action that dismisses the prompt overlay (spawn, plan, qa,
    /// send) so ratatui repaints every cell from scratch.  This prevents
    /// leftover prompt-box cells from appearing as artifacts after the overlay
    /// disappears — ratatui's incremental diff renderer only repaints changed
    /// cells, and a tmux window-switch round-trip can leave the terminal state
    /// stale enough that the diff misses cells that need clearing.
    pub needs_full_redraw: bool,

    // ── Add-project multi-step flow ───────────────────────────────────────────
    /// Which step of the add-project flow we are currently collecting input for.
    /// `None` when no add-project flow is active.
    pub add_project_step: Option<AddProjectStep>,
    /// Partial result: project full name collected in step 1.
    pub pending_project_name: String,
    /// Partial result: project short name collected in step 2.
    pub pending_project_short: String,
    /// Partial result: git repo URL collected in step 3.
    pub pending_project_url: String,
    /// Partial result: gh account collected in step 4.
    pub pending_project_account: String,
    /// Partial result: group label collected in step 5 (None = no group).
    pub pending_project_group: Option<String>,
    /// Partial result: bounded-context tag collected in step 6 (None = no context).
    pub pending_project_context: Option<String>,
    /// Cycle options for the Group step (distinct existing groups + empty).
    pub group_cycle_options: Vec<String>,
    /// Cycle options for the Context step (distinct existing contexts + empty).
    pub context_cycle_options: Vec<String>,

    // ── Background clone state ────────────────────────────────────────────────
    /// Receives the result of a background git-clone spawned during add-project.
    /// `Some` while `Mode::Cloning` is active; `None` otherwise.
    pub clone_rx: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    /// Human-readable label shown next to the spinner, e.g. "Cloning my-service…".
    pub cloning_label: String,
    /// Current frame index for the braille spinner (0–7, wraps).
    pub spinner_frame: u8,
}

impl App {
    pub fn new(registry: Registry, session: String, tui_window_id: String) -> Self {
        let worktrees = registry.worktrees.clone();
        let projects = registry.projects.clone();
        let count = worktrees.len();
        let mut list_state = ratatui::widgets::ListState::default();

        let theme_name = &registry.ui.theme;
        let theme = Theme::from_name(theme_name);
        let theme_picker_cursor = Theme::index_of(theme_name);

        // Load persisted group collapse state from registry.
        let group_collapsed = registry.group_states.clone();

        // Build entries from projects + worktrees, respecting collapse state.
        let entries = Self::build_entries(&projects, &worktrees, &group_collapsed);

        // Select the first worktree entry (skip headers).
        let initial_selection = entries
            .iter()
            .position(|e| matches!(e, ListEntry::Worktree { .. }));
        list_state.select(initial_selection);

        let saved_theme_id = registry.ui.theme.clone();

        App {
            registry,
            worktrees,
            projects,
            group_collapsed,
            entries,
            phases: vec!["?".to_string(); count],
            list_state,
            mode: Mode::Normal,
            input_buf: String::new(),
            cursor_pos: 0,
            stats_cache: HashMap::new(),
            status_msg: None,
            session,
            tui_window_id,
            should_quit: false,
            last_stats_idx: None,
            theme,
            show_preview: false,
            preview_lines: Vec::new(),
            preview_scroll: 0,
            last_preview_idx: None,
            show_detail: false,
            detail_lines: Vec::new(),
            last_detail_idx: None,
            show_theme_picker: false,
            show_help: false,
            theme_picker_cursor,
            theme_picker_original: None,
            saved_theme_id,
            last_paste_at: Instant::now() - Duration::from_secs(10),
            last_key_at: Instant::now() - Duration::from_secs(10),
            prompt_scroll: 0,
            terminal_width: 80,
            input_history: Vec::new(),
            history_idx: None,
            history_draft: String::new(),
            needs_full_redraw: false,
            add_project_step: None,
            pending_project_name: String::new(),
            pending_project_short: String::new(),
            pending_project_url: String::new(),
            pending_project_account: String::new(),
            pending_project_group: None,
            pending_project_context: None,
            group_cycle_options: Vec::new(),
            context_cycle_options: Vec::new(),
            clone_rx: None,
            cloning_label: String::new(),
            spinner_frame: 0,
        }
    }

    /// Build the visual entries list from a project configs snapshot and the
    /// flat worktrees list.  Called once on construction and again whenever
    /// collapse state changes.
    ///
    /// The list is structured as a two-level hierarchy:
    ///   GroupHeader (if any groups are defined)
    ///     ProjectHeader
    ///       Worktree rows / EmptyProject
    ///
    /// Projects with `group = None` are rendered under an implicit "Ungrouped"
    /// GroupHeader at the bottom — but only if there is at least one named group,
    /// so configs without any group assignments remain unchanged (backwards compat).
    fn build_entries(
        projects: &[ProjectConfig],
        worktrees: &[Worktree],
        group_collapsed: &HashMap<String, bool>,
    ) -> Vec<ListEntry> {
        let mut entries = Vec::new();
        let mut wt_offset = 0usize;

        // Collect distinct group names in first-seen order.
        // None → will be placed in "Ungrouped" if any named groups exist.
        let mut group_order: Vec<Option<String>> = Vec::new();
        for proj in projects {
            let key = proj.group.clone();
            if !group_order.contains(&key) {
                group_order.push(key);
            }
        }

        // Determine whether any named groups exist so we know if we need an
        // "Ungrouped" header.
        let has_named_groups = group_order.iter().any(|g| g.is_some());

        // If there are no named groups at all, fall back to the original flat
        // rendering (no GroupHeader rows emitted).
        if !has_named_groups {
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
                    wt_offset += proj.worktrees.len();
                }
            }
            return entries;
        }

        // Named groups exist: emit GroupHeader rows and nest everything beneath them.
        // Named groups first, then ungrouped projects (if any) under "Ungrouped".
        let mut named_groups: Vec<Option<String>> = group_order
            .iter()
            .filter(|g| g.is_some())
            .cloned()
            .collect();
        let has_ungrouped = group_order.iter().any(|g| g.is_none());
        if has_ungrouped {
            named_groups.push(None); // process "Ungrouped" last
        }

        // We need a fresh wt_offset pass because projects are filtered per group.
        // Rebuild a mapping: project_idx → worktree_start_offset in flat list.
        let mut proj_wt_start = vec![0usize; projects.len()];
        let mut offset = 0usize;
        for (i, proj) in projects.iter().enumerate() {
            proj_wt_start[i] = offset;
            offset += proj.worktrees.len();
        }

        for group_key in &named_groups {
            let group_display_name = group_key.as_deref().unwrap_or("Ungrouped").to_string();
            let group_is_collapsed = *group_collapsed.get(&group_display_name).unwrap_or(&false);

            entries.push(ListEntry::GroupHeader {
                name: group_display_name.clone(),
                collapsed: group_is_collapsed,
            });

            if !group_is_collapsed {
                // Emit projects belonging to this group, in their original order.
                for (proj_idx, proj) in projects.iter().enumerate() {
                    if proj.group.as_deref() != group_key.as_deref() {
                        continue;
                    }
                    entries.push(ListEntry::ProjectHeader {
                        name: proj.name.clone(),
                        collapsed: proj.collapsed,
                        project_idx: proj_idx,
                    });
                    let wt_start = proj_wt_start[proj_idx];
                    if !proj.collapsed {
                        if proj.worktrees.is_empty() {
                            entries.push(ListEntry::EmptyProject);
                        } else {
                            for i in 0..proj.worktrees.len() {
                                entries.push(ListEntry::Worktree {
                                    wt: worktrees[wt_start + i].clone(),
                                    worktree_idx: wt_start + i,
                                });
                            }
                        }
                    }
                }
            }
        }

        entries
    }

    /// Rebuild `self.entries` from the current `projects` snapshot.
    pub fn rebuild_entries(&mut self) {
        let current_wt_idx = self.selected_worktree_idx();
        self.entries = Self::build_entries(&self.projects, &self.worktrees, &self.group_collapsed);

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
    pub fn toggle_collapse(&mut self, entry_idx: usize) {
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
        let _ = write_collapsed(&self.registry.base_dir, &project_name, new_collapsed);
    }

    /// Toggle the collapsed state of the super-group at `entry_idx` and persist
    /// the change to task-master.toml via `write_group_collapsed`.
    pub fn toggle_group_collapse(&mut self, entry_idx: usize) {
        let (group_name, new_collapsed) =
            if let Some(ListEntry::GroupHeader {
                name, collapsed, ..
            }) = self.entries.get(entry_idx)
            {
                (name.clone(), !*collapsed)
            } else {
                return;
            };

        self.group_collapsed
            .insert(group_name.clone(), new_collapsed);
        self.rebuild_entries();

        // Try to keep the cursor on the GroupHeader row after rebuild.
        // rebuild_entries already handles selection preservation, but if the
        // cursor was on a worktree that just got hidden we want to land on the
        // group header itself instead.
        if self.list_state.selected().is_none() {
            let header_pos = self.entries.iter().position(
                |e| matches!(e, ListEntry::GroupHeader { name, .. } if name == &group_name),
            );
            self.list_state.select(header_pos.or(Some(0)));
        }

        // Persist (best-effort; don't crash TUI on write failure).
        let _ = write_group_collapsed(&self.registry.base_dir, &group_name, new_collapsed);
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
        if self.show_detail {
            self.refresh_detail();
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

    /// Populate `detail_lines` for the currently selected worktree by running
    /// git subcommands in the worktree directory.  Called when the detail pane
    /// is toggled on or when the selection changes while the pane is visible.
    ///
    /// Gathers:
    ///   - Current branch name (`git rev-parse --abbrev-ref HEAD`)
    ///   - Count of uncommitted changes (`git status --porcelain`)
    ///   - Last 5 commit subjects (`git log --oneline -5`)
    pub fn refresh_detail(&mut self) {
        let wt = match self.selected_worktree() {
            Some(w) => w.clone(),
            None => {
                self.detail_lines.clear();
                return;
            }
        };
        let wt_idx = self.selected_worktree_idx().unwrap();
        let path = wt.abs_path.to_string_lossy().to_string();
        let mut lines: Vec<String> = Vec::new();

        // Branch
        let branch = run_git(&path, &["rev-parse", "--abbrev-ref", "HEAD"])
            .unwrap_or_else(|| "unknown".to_string());
        lines.push(format!("Branch: {}", branch.trim()));

        // Dirty file count
        let dirty = run_git(&path, &["status", "--porcelain"]).unwrap_or_default();
        let dirty_count = dirty.lines().filter(|l| !l.is_empty()).count();
        if dirty_count == 0 {
            lines.push("Status: clean".to_string());
        } else {
            lines.push(format!("Status: {} uncommitted file(s)", dirty_count));
        }

        lines.push(String::new()); // blank separator
        lines.push("Recent commits:".to_string());

        // Last 5 commits
        let log = run_git(&path, &["log", "--oneline", "-5"]).unwrap_or_default();
        if log.trim().is_empty() {
            lines.push("  (no commits)".to_string());
        } else {
            for commit_line in log.lines() {
                lines.push(format!("  {}", commit_line));
            }
        }

        self.detail_lines = lines;
        self.last_detail_idx = Some(wt_idx);
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

    pub fn theme_picker_commit(&mut self) {
        let id = ALL_THEMES[self.theme_picker_cursor].0;
        // Persist to config (best effort — don't crash TUI on write failure).
        let _ = crate::registry::write_theme(&self.registry.base_dir, id);
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

    /// Clear the prompt input and return to Normal mode.
    ///
    /// Call this after any action (success or failure) that should dismiss the
    /// input bar.  Centralising the reset here ensures every field is cleared
    /// consistently and makes it impossible to accidentally leave `cursor_pos`
    /// or `history_idx` in a stale state.
    pub fn reset_input(&mut self) {
        self.mode = Mode::Normal;
        self.input_buf.clear();
        self.cursor_pos = 0;
        self.prompt_scroll = 0;
        self.history_idx = None;
        self.history_draft.clear();
        // Clear any in-progress add-project flow so stale state can never leak.
        // NOTE: clone_rx / cloning_label / spinner_frame are intentionally NOT
        // cleared here — they persist across reset_input while Mode::Cloning is
        // active and are only cleared when the background thread delivers its result.
        self.add_project_step = None;
        self.pending_project_name.clear();
        self.pending_project_short.clear();
        self.pending_project_url.clear();
        self.pending_project_account.clear();
        self.pending_project_group = None;
        self.pending_project_context = None;
        self.group_cycle_options.clear();
        self.context_cycle_options.clear();
        // Force a full repaint on the next frame so the prompt overlay cells
        // are cleared even if ratatui's diff renderer would otherwise skip them
        // (e.g. after a tmux window-switch leaves the terminal state stale).
        self.needs_full_redraw = true;
    }

    /// Reload in-memory worktree/project state from a freshly-loaded registry.
    ///
    /// Called after `cmd_add_worktree` or `cmd_remove_worktree` mutates
    /// `task-master.toml` on disk.  Rebuilds `entries` and resizes/resets
    /// derivative caches while preserving UI state (theme, mode, history, etc.).
    ///
    /// After calling this, `refresh_phases()` should be called so the phase
    /// column reflects any windows that may have appeared or disappeared.
    pub fn reload_from_registry(&mut self, new_registry: Registry) {
        self.worktrees = new_registry.worktrees.clone();
        self.projects = new_registry.projects.clone();

        // Resize phases vec to match new worktree count; fill new slots with '?'.
        self.phases.resize(self.worktrees.len(), "?".to_string());

        // Invalidate derived caches — indices may have shifted.
        self.stats_cache.clear();
        self.last_stats_idx = None;
        self.last_preview_idx = None;
        self.last_detail_idx = None;
        self.preview_lines.clear();
        self.detail_lines.clear();

        // Replace the owned registry so all subsequent actions use the new state.
        self.registry = new_registry;

        // Rebuild visual entries.  rebuild_entries tries to keep the cursor on
        // the same worktree_idx; if that index no longer exists (e.g. after a
        // remove), it falls back to the nearest visible row.
        self.rebuild_entries();
    }

    /// Guard for keyboard actions that require a worktree to be selected.
    ///
    /// If no worktree row is currently selected, sets a status message and
    /// returns `false`.  Returns `true` when a worktree is selected and the
    /// caller can proceed.
    ///
    /// Usage:
    /// ```ignore
    /// if !app.require_worktree_selected() { return Ok(()); }
    /// ```
    pub fn require_worktree_selected(&mut self) -> bool {
        if self.selected_worktree().is_none() {
            self.set_status("Select a worktree first (use j/k to navigate to a worktree row).");
            false
        } else {
            true
        }
    }

    /// Adjust `prompt_scroll` so the cursor line is always within the visible
    /// window of the prompt overlay (capped at `PROMPT_MAX_ROWS` content rows).
    ///
    /// Call this after every change to `input_buf` or `cursor_pos` while the
    /// prompt is open.  Uses `self.terminal_width` (refreshed each render frame)
    /// to compute word-wrap boundaries.
    pub fn update_prompt_scroll(&mut self) {
        const PROMPT_MAX_ROWS: usize = 8;

        // Inner width = terminal_width - 2 borders, minimum 1.
        let inner_width = (self.terminal_width as usize).saturating_sub(2).max(1);

        // Compute the visual line index of the cursor, matching the word-wrap
        // logic used in prompt.rs's content_rows calculation.
        let cursor_byte = self.cursor_pos.min(self.input_buf.len());
        let text_before_cursor = &self.input_buf[..cursor_byte];

        let mut cursor_visual_line: usize = 0;
        for hard_line in text_before_cursor.split('\n') {
            let chars = hard_line.chars().count();
            // Each hard line contributes ceil(chars / inner_width) wrapped rows,
            // minimum 1.  The cursor sits at the end of the last such row.
            let rows = (chars / inner_width) + 1;
            cursor_visual_line += rows;
        }
        // cursor_visual_line is now 1-based (the row the cursor is on).
        // Convert to 0-based for easier arithmetic.
        let cursor_row = cursor_visual_line.saturating_sub(1);

        // Scroll up if cursor is above the visible window.
        if cursor_row < self.prompt_scroll {
            self.prompt_scroll = cursor_row;
        }
        // Scroll down if cursor is below the visible window.
        if cursor_row >= self.prompt_scroll + PROMPT_MAX_ROWS {
            self.prompt_scroll = cursor_row + 1 - PROMPT_MAX_ROWS;
        }
    }

    /// Build the list of visual lines for the current `input_buf`, accounting
    /// for both hard newlines and soft word-wrap at `inner_width`.
    ///
    /// Returns `Vec<(byte_start, byte_end)>` — the byte range in `input_buf`
    /// for each visual line (excluding the newline character itself).
    fn visual_lines(input: &str, inner_width: usize) -> Vec<(usize, usize)> {
        let mut result = Vec::new();
        let mut byte_offset = 0usize;

        for hard_line in input.split('\n') {
            let hard_len = hard_line.len();
            let chars: Vec<(usize, char)> = hard_line.char_indices().collect();

            if chars.is_empty() {
                // Empty hard line → one empty visual line.
                result.push((byte_offset, byte_offset));
            } else {
                let mut visual_start_char = 0usize;
                while visual_start_char < chars.len() {
                    let visual_end_char = (visual_start_char + inner_width).min(chars.len());
                    let vl_byte_start = byte_offset + chars[visual_start_char].0;
                    let vl_byte_end = if visual_end_char == chars.len() {
                        byte_offset + hard_len
                    } else {
                        byte_offset + chars[visual_end_char].0
                    };
                    result.push((vl_byte_start, vl_byte_end));
                    visual_start_char = visual_end_char;
                }
            }

            // +1 for the '\n' separator (not present for the last segment).
            byte_offset += hard_len + 1;
        }

        result
    }

    /// Try to move the cursor up one visual line, preserving the column.
    ///
    /// Returns `Some(new_cursor_pos)` on success, or `None` if the cursor is
    /// already on the first visual line (caller should fall through to history).
    pub fn move_cursor_up(&self) -> Option<usize> {
        let inner_width = (self.terminal_width as usize).saturating_sub(2).max(1);
        let lines = Self::visual_lines(&self.input_buf, inner_width);
        let cursor = self.cursor_pos.min(self.input_buf.len());

        // Find which visual line the cursor is on.
        let cur_line_idx = lines
            .iter()
            .rposition(|(start, _)| *start <= cursor)
            .unwrap_or(0);

        if cur_line_idx == 0 {
            return None; // already on first line — fall through to history
        }

        // Column = chars from the start of the current visual line to the cursor.
        let (cur_start, _) = lines[cur_line_idx];
        let col = self.input_buf[cur_start..cursor].chars().count();

        // Target: same column on the previous visual line (clamped to its length).
        let (prev_start, prev_end) = lines[cur_line_idx - 1];
        let prev_line_chars: Vec<(usize, char)> = self.input_buf[prev_start..prev_end]
            .char_indices()
            .collect();
        let new_pos = if col >= prev_line_chars.len() {
            prev_end
        } else {
            prev_start + prev_line_chars[col].0
        };
        Some(new_pos)
    }

    /// Try to move the cursor down one visual line, preserving the column.
    ///
    /// Returns `Some(new_cursor_pos)` on success, or `None` if the cursor is
    /// already on the last visual line (caller should fall through to history).
    pub fn move_cursor_down(&self) -> Option<usize> {
        let inner_width = (self.terminal_width as usize).saturating_sub(2).max(1);
        let lines = Self::visual_lines(&self.input_buf, inner_width);
        let cursor = self.cursor_pos.min(self.input_buf.len());

        // Find which visual line the cursor is on.
        let cur_line_idx = lines
            .iter()
            .rposition(|(start, _)| *start <= cursor)
            .unwrap_or(0);

        if cur_line_idx + 1 >= lines.len() {
            return None; // already on last line — fall through to history
        }

        // Column = chars from the start of the current visual line to the cursor.
        let (cur_start, _) = lines[cur_line_idx];
        let col = self.input_buf[cur_start..cursor].chars().count();

        // Target: same column on the next visual line (clamped to its length).
        let (next_start, next_end) = lines[cur_line_idx + 1];
        let next_line_chars: Vec<(usize, char)> = self.input_buf[next_start..next_end]
            .char_indices()
            .collect();
        let new_pos = if col >= next_line_chars.len() {
            next_end
        } else {
            next_start + next_line_chars[col].0
        };
        Some(new_pos)
    }

    /// Resolve the project short name from the currently selected entry.    ///
    /// Returns `Some(project_short)` when:
    /// - A `Worktree` row is selected → use that worktree's `project_short`.
    /// - A `ProjectHeader` row is selected → use that project's `short` name.
    ///
    /// Returns `None` when a `GroupHeader`, `EmptyProject`, or nothing is selected,
    /// i.e. when there is no unambiguous project context.
    pub fn selected_project_short(&self) -> Option<String> {
        let idx = self.selected()?;
        match self.entries.get(idx)? {
            ListEntry::Worktree { wt, .. } => Some(wt.project_short.clone()),
            ListEntry::ProjectHeader { project_idx, .. } => {
                self.projects.get(*project_idx).map(|p| p.short.clone())
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Run a git command in `working_dir` and return its trimmed stdout, or `None`
/// if the command fails or produces no output.
fn run_git(working_dir: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(working_dir)
        .args(args)
        .output()
        .ok()?;
    if output.status.success() {
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    } else {
        None
    }
}
