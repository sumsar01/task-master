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
}

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Normal,
    Prompt(ActionKind),
    /// Spawn on an active window: user confirmed once, needs Enter again to force.
    ForceConfirm,
    /// Confirm-close modal: user pressed 'c', waiting for 'y' or any other key.
    ConfirmClose,
}

pub struct App {
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

    // ── Overlays ──────────────────────────────────────────────────────────────
    pub show_theme_picker: bool,
    pub show_help: bool,
    pub theme_picker_cursor: usize,
    pub theme_picker_original: Option<Theme>,
    /// The theme id that is currently written to config (used for the ✓ in the picker).
    pub saved_theme_id: String,

    /// When `true`, the next frame will call `terminal.clear()` before drawing.
    ///
    /// Set after any action that dismisses the prompt overlay (spawn, plan, qa,
    /// send) so ratatui repaints every cell from scratch.  This prevents
    /// leftover prompt-box cells from appearing as artifacts after the overlay
    /// disappears — ratatui's incremental diff renderer only repaints changed
    /// cells, and a tmux window-switch round-trip can leave the terminal state
    /// stale enough that the diff misses cells that need clearing.
    pub needs_full_redraw: bool,
}

impl App {
    pub fn new(registry: &Registry, session: String, tui_window_name: String) -> Self {
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

        App {
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
            last_key_at: Instant::now() - Duration::from_secs(10),
            input_history: Vec::new(),
            history_idx: None,
            history_draft: String::new(),
            needs_full_redraw: false,
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

    /// Toggle the collapsed state of the super-group at `entry_idx` and persist
    /// the change to task-master.toml via `write_group_collapsed`.
    pub fn toggle_group_collapse(&mut self, entry_idx: usize, registry: &Registry) {
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
        let _ = write_group_collapsed(&registry.base_dir, &group_name, new_collapsed);
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
        self.history_idx = None;
        self.history_draft.clear();
        // Force a full repaint on the next frame so the prompt overlay cells
        // are cleared even if ratatui's diff renderer would otherwise skip them
        // (e.g. after a tmux window-switch leaves the terminal state stale).
        self.needs_full_redraw = true;
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
}
