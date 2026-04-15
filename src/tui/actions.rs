use super::app::{ActionKind, AddProjectStep, App, Mode};
use crate::tmux;
use anyhow::Result;

/// How long to wait (ms) after spawning/killing a tmux window before
/// re-selecting the TUI window. opencode's startup terminal takeover can
/// trigger a tmux activity event that steals focus; this pause wins the race.
const TMUX_REFOCUS_DELAY_MS: u64 = 250;

// ---------------------------------------------------------------------------
// Action execution
// ---------------------------------------------------------------------------

pub fn execute_action(app: &mut App, kind: &ActionKind, force: bool) -> Result<()> {
    match kind {
        ActionKind::Spawn => execute_spawn(app, force),
        ActionKind::Plan => execute_plan(app),
        ActionKind::Qa => execute_qa(app),
        ActionKind::Send => execute_send(app),
        ActionKind::AddWorktree => execute_add_worktree(app),
        ActionKind::AddProject => execute_add_project(app),
    }
}

pub fn execute_spawn(app: &mut App, force: bool) -> Result<()> {
    let (wt_name, prompt) = match collect_spawn_inputs(app) {
        Some(x) => x,
        None => return Ok(()),
    };
    match crate::spawn::cmd_spawn(&app.registry, &wt_name, &prompt, force) {
        Ok(_) => {
            refocus_tui_window(&app.session, &app.tui_window_id);
            app.set_status(format!("Spawned {}:dev", wt_name));
            push_history(app, &prompt);
            app.reset_input();
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
                app.reset_input();
            }
        }
    }
    Ok(())
}

pub fn execute_plan(app: &mut App) -> Result<()> {
    let (wt_name, prompt) = match collect_spawn_inputs(app) {
        Some(x) => x,
        None => return Ok(()),
    };
    match crate::plan::cmd_plan(&app.registry, &wt_name, &prompt) {
        Ok(_) => {
            refocus_tui_window(&app.session, &app.tui_window_id);
            app.set_status(format!("Plan agent started in {}:plan", wt_name));
            push_history(app, &prompt);
            app.reset_input();
            app.refresh_phases();
        }
        Err(e) => {
            app.set_status(format!("Plan failed: {}", e));
            app.reset_input();
        }
    }
    Ok(())
}

pub fn execute_qa(app: &mut App) -> Result<()> {
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
    match crate::qa::cmd_qa(&app.registry, &wt_name, Some(pr_number)) {
        Ok(_) => {
            refocus_tui_window(&app.session, &app.tui_window_id);
            app.set_status(format!(
                "QA agent started for {} PR #{}",
                wt_name, pr_number
            ));
            let hist_entry = app.input_buf.trim().to_string();
            push_history(app, &hist_entry);
            app.reset_input();
            app.refresh_phases();
        }
        Err(e) => {
            app.set_status(format!("QA failed: {}", e));
            app.reset_input();
        }
    }
    Ok(())
}

pub fn execute_close(app: &mut App) -> Result<()> {
    let wt_name = match app.selected_worktree() {
        Some(wt) => wt.window_name.clone(),
        None => return Ok(()),
    };
    match crate::cmd_close(&app.session, &wt_name) {
        Ok(()) => {
            // Reclaim focus after the kill-window call. Killing a tmux window
            // can briefly steal focus away from the TUI window.
            refocus_tui_window(&app.session, &app.tui_window_id);

            app.mode = Mode::Normal;
            app.set_status(format!("Closed {}.", wt_name));
            // Force a full repaint so the confirm-close modal cells and any
            // other stale areas are cleared on the next frame.
            app.needs_full_redraw = true;
            app.refresh_phases();
        }
        Err(e) => {
            app.mode = Mode::Normal;
            app.set_status(format!("Close failed: {}", e));
        }
    }
    Ok(())
}

pub fn execute_send(app: &mut App) -> Result<()> {
    let wt_name = match app.selected_worktree() {
        Some(wt) => wt.window_name.clone(),
        None => return Ok(()),
    };
    let prompt = app.input_buf.trim().to_string();
    if prompt.is_empty() {
        app.set_status("Message is empty — type something first.");
        return Ok(());
    }
    match crate::cmd_send(&app.registry, &wt_name, &prompt) {
        Ok(()) => {
            let _ = tmux::select_window_by_id(&app.session, &app.tui_window_id);
            app.set_status(format!("Sent message to {}.", wt_name));
            push_history(app, &prompt);
            app.reset_input();
            app.refresh_phases();
        }
        Err(e) => {
            app.set_status(format!("Send failed: {}", e));
            app.reset_input();
        }
    }
    Ok(())
}

/// Create a new ephemeral git worktree for the currently selected project.
///
/// The worktree name is read from `app.input_buf`. The project is inferred
/// from the selected entry (Worktree row → its project; ProjectHeader → that
/// project). On success the registry is reloaded from disk so the new row
/// appears immediately.
pub fn execute_add_worktree(app: &mut App) -> Result<()> {
    let name = app.input_buf.trim().to_string();
    if name.is_empty() {
        app.set_status("Worktree name cannot be empty.");
        return Ok(());
    }

    let project_short = match app.selected_project_short() {
        Some(s) => s,
        None => {
            app.set_status("Select a project or worktree first (use j/k to navigate).");
            return Ok(());
        }
    };

    let base_dir = app.registry.base_dir.clone();
    match crate::worktree::cmd_add_worktree(&app.registry, &base_dir, &project_short, &name, None) {
        Ok(_) => {
            // Reload the registry from disk so the new worktree is visible
            // and all subsequent actions (spawn, remove) use the updated state.
            match crate::registry::Registry::load(base_dir) {
                Ok(new_reg) => app.reload_from_registry(new_reg),
                Err(e) => {
                    app.set_status(format!("Added worktree but failed to reload config: {}", e));
                    app.reset_input();
                    return Ok(());
                }
            }
            refocus_tui_window(&app.session, &app.tui_window_id);
            app.set_status(format!(
                "Added {}-{}. Press s to spawn an agent.",
                project_short, name
            ));
            app.reset_input();
            app.refresh_phases();
        }
        Err(e) => {
            app.set_status(format!("Add worktree failed: {}", e));
            app.reset_input();
        }
    }
    Ok(())
}

/// Remove the currently selected git worktree (runs `git worktree remove` and
/// removes the entry from `task-master.toml`).  On success the registry is
/// reloaded so the row disappears immediately.
pub fn execute_remove_worktree(app: &mut App) -> Result<()> {
    let window_name = match app.selected_worktree() {
        Some(wt) => wt.window_name.clone(),
        None => return Ok(()),
    };

    let base_dir = app.registry.base_dir.clone();
    match crate::worktree::cmd_remove_worktree(&app.registry, &base_dir, &window_name, false) {
        Ok(()) => {
            // Reload the registry from disk so the removed row disappears
            // and all subsequent actions use the updated state.
            match crate::registry::Registry::load(base_dir) {
                Ok(new_reg) => app.reload_from_registry(new_reg),
                Err(e) => {
                    app.set_status(format!(
                        "Removed worktree but failed to reload config: {}",
                        e
                    ));
                    app.mode = Mode::Normal;
                    app.needs_full_redraw = true;
                    return Ok(());
                }
            }
            app.mode = Mode::Normal;
            app.set_status(format!("Removed {}.", window_name));
            app.needs_full_redraw = true;
            app.refresh_phases();
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("dirty") || msg.contains("modified or untracked") {
                app.set_status(format!(
                    "{} has modified/untracked files. Press Enter to force-remove, Esc to cancel.",
                    window_name
                ));
                app.mode = Mode::ForceConfirmRemoveWorktree;
            } else {
                app.mode = Mode::Normal;
                app.set_status(format!("Remove worktree failed: {}", msg));
            }
            app.needs_full_redraw = true;
        }
    }
    Ok(())
}

/// Returns the list of logged-in gh accounts by parsing `gh auth status`.
fn gh_accounts() -> Vec<String> {
    let out = std::process::Command::new("gh")
        .args(["auth", "status"])
        .output();
    match out {
        Ok(o) => {
            let text = String::from_utf8_lossy(&o.stderr).to_string()
                + &String::from_utf8_lossy(&o.stdout);
            text.lines()
                .filter_map(|l| {
                    let l = l.trim();
                    // Lines look like: "✓ Logged in to github.com account sumsar01 (keyring)"
                    if l.contains("Logged in to") && l.contains("account") {
                        l.split("account ")
                            .nth(1)
                            .map(|s| s.split_whitespace().next().unwrap_or("").to_string())
                    } else {
                        None
                    }
                })
                .filter(|s| !s.is_empty())
                .collect()
        }
        Err(_) => vec![],
    }
}

/// Add a new project via a four-step prompt sequence (name → short → url → account).
///
/// Called on each Enter press while `Mode::Prompt(ActionKind::AddProject)` is
/// active.  The current step is tracked in `app.add_project_step`:
///
/// - `Name`    → validates & stores into `app.pending_project_name`, advances to `Short`.
/// - `Short`   → validates & stores into `app.pending_project_short`, advances to `Url`.
/// - `Url`     → validates & stores into `app.pending_project_url`, advances to `Account`.
/// - `Account` → runs `cmd_add_project` with the chosen account, reloads registry on success.
///
/// On success the flow resets (`add_project_step = None`, pending fields
/// cleared).  On any error the flow is cancelled (same reset) and the
/// error message is shown in the status bar.
pub fn execute_add_project(app: &mut App) -> Result<()> {
    let step = match app.add_project_step.clone() {
        Some(s) => s,
        None => return Ok(()),
    };

    let input = app.input_buf.trim().to_string();

    match step {
        AddProjectStep::Name => {
            if input.is_empty() {
                app.set_status("Project name cannot be empty.");
                return Ok(());
            }
            app.pending_project_name = input;
            app.input_buf.clear();
            app.cursor_pos = 0;
            app.add_project_step = Some(AddProjectStep::Short);
            app.set_status(format!(
                "Project '{}' — enter short name (e.g. WIS):",
                app.pending_project_name
            ));
        }
        AddProjectStep::Short => {
            if input.is_empty() {
                app.set_status("Short name cannot be empty.");
                return Ok(());
            }
            app.pending_project_short = input;
            app.input_buf.clear();
            app.cursor_pos = 0;
            app.add_project_step = Some(AddProjectStep::Url);
            app.set_status(format!(
                "'{}' ({}) — enter git repo URL:",
                app.pending_project_name, app.pending_project_short
            ));
        }
        AddProjectStep::Url => {
            if input.is_empty() {
                app.set_status("Repo URL cannot be empty.");
                return Ok(());
            }
            app.pending_project_url = input;
            app.input_buf.clear();
            app.cursor_pos = 0;
            app.add_project_step = Some(AddProjectStep::Account);
            // Pre-populate input with the first account so user can just hit Enter
            // for the default, or edit to pick a different one.
            let accounts = gh_accounts();
            let hint = if accounts.is_empty() {
                String::new()
            } else {
                accounts.join(" / ")
            };
            if let Some(first) = accounts.into_iter().next() {
                app.input_buf = first.clone();
                app.cursor_pos = first.len();
            }
            app.set_status(format!(
                "URL saved — enter gh account to clone with ({}): ",
                hint
            ));
        }
        AddProjectStep::Account => {
            if input.is_empty() {
                app.set_status("Account cannot be empty.");
                return Ok(());
            }
            let name = app.pending_project_name.clone();
            let short = app.pending_project_short.clone();
            let url = app.pending_project_url.clone();
            let account = input;
            let base_dir = app.registry.base_dir.clone();

            match crate::cmd_add_project(&base_dir, &name, &short, &url, Some(&account)) {
                Ok(()) => {
                    match crate::registry::Registry::load(base_dir) {
                        Ok(new_reg) => app.reload_from_registry(new_reg),
                        Err(e) => {
                            app.set_status(format!(
                                "Added project but failed to reload config: {}",
                                e
                            ));
                            app.reset_input();
                            return Ok(());
                        }
                    }
                    app.set_status(format!(
                        "Added project {} ({}). Press N to add a worktree.",
                        name, short
                    ));
                    app.reset_input();
                    app.refresh_phases();
                }
                Err(e) => {
                    app.set_status(format!("Add project failed: {}", e));
                    app.reset_input();
                }
            }
        }
    }

    Ok(())
}

/// Force-remove the currently selected worktree, discarding any local changes.
/// Called after the user confirmed a second time from `ForceConfirmRemoveWorktree` mode.
pub fn execute_force_remove_worktree(app: &mut App) -> Result<()> {
    let window_name = match app.selected_worktree() {
        Some(wt) => wt.window_name.clone(),
        None => return Ok(()),
    };

    let base_dir = app.registry.base_dir.clone();
    match crate::worktree::cmd_remove_worktree(&app.registry, &base_dir, &window_name, true) {
        Ok(()) => {
            match crate::registry::Registry::load(base_dir) {
                Ok(new_reg) => app.reload_from_registry(new_reg),
                Err(e) => {
                    app.set_status(format!(
                        "Removed worktree but failed to reload config: {}",
                        e
                    ));
                    app.mode = Mode::Normal;
                    app.needs_full_redraw = true;
                    return Ok(());
                }
            }
            app.mode = Mode::Normal;
            app.set_status(format!("Force-removed {}.", window_name));
            app.needs_full_redraw = true;
            app.refresh_phases();
        }
        Err(e) => {
            app.mode = Mode::Normal;
            app.set_status(format!("Force-remove failed: {}", e));
            app.needs_full_redraw = true;
        }
    }
    Ok(())
}

///
/// Uses the stable `#{window_id}` (@N) rather than the window name, so that a
/// worktree whose base name collides with the TUI window's name can never cause
/// the wrong window to be selected.
///
/// Sends select-window twice with a brief sleep between them: the first
/// call reclaims focus immediately; the sleep lets opencode's startup settle;
/// the second call wins the race against any delayed tmux activity event.
fn refocus_tui_window(session: &str, tui_window_id: &str) {
    let _ = tmux::select_window_by_id(session, tui_window_id);
    std::thread::sleep(std::time::Duration::from_millis(TMUX_REFOCUS_DELAY_MS));
    let _ = tmux::select_window_by_id(session, tui_window_id);
}

pub fn push_history(app: &mut App, text: &str) {
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

pub fn collect_spawn_inputs(app: &mut App) -> Option<(String, String)> {
    let wt_name = app.selected_worktree().map(|wt| wt.window_name.clone())?;
    let prompt = app.input_buf.trim().to_string();
    if prompt.is_empty() {
        app.set_status("Prompt cannot be empty");
        return None;
    }
    Some((wt_name, prompt))
}

pub fn attach_to_window(session: &str, base_name: &str, full_name: &str) {
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
