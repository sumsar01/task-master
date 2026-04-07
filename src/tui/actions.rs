use super::app::{ActionKind, App, Mode};
use crate::registry::Registry;
use crate::tmux;
use anyhow::Result;

// ---------------------------------------------------------------------------
// Action execution
// ---------------------------------------------------------------------------

pub fn execute_action(
    app: &mut App,
    registry: &Registry,
    kind: &ActionKind,
    force: bool,
) -> Result<()> {
    match kind {
        ActionKind::Spawn => execute_spawn(app, registry, force),
        ActionKind::Plan => execute_plan(app, registry),
        ActionKind::Qa => execute_qa(app, registry),
        ActionKind::Send => execute_send(app, registry),
        ActionKind::AddWorktree => execute_add_worktree(app, registry),
    }
}

pub fn execute_spawn(app: &mut App, registry: &Registry, force: bool) -> Result<()> {
    let (wt_name, prompt) = match collect_spawn_inputs(app) {
        Some(x) => x,
        None => return Ok(()),
    };
    match crate::cmd_spawn(registry, &wt_name, &prompt, force) {
        Ok(_) => {
            let _ = tmux::select_tui_window(&app.session, &app.tui_window_name);
            // opencode startup in the new window can trigger a tmux activity event
            // that steals focus. Re-select after a brief pause to win the race.
            std::thread::sleep(std::time::Duration::from_millis(250));
            let _ = tmux::select_tui_window(&app.session, &app.tui_window_name);
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

pub fn execute_plan(app: &mut App, registry: &Registry) -> Result<()> {
    let (wt_name, prompt) = match collect_spawn_inputs(app) {
        Some(x) => x,
        None => return Ok(()),
    };
    match crate::plan::cmd_plan(registry, &wt_name, &prompt) {
        Ok(_) => {
            let _ = tmux::select_tui_window(&app.session, &app.tui_window_name);
            // opencode startup in the new window can trigger a tmux activity event
            // that steals focus. Re-select after a brief pause to win the race.
            std::thread::sleep(std::time::Duration::from_millis(250));
            let _ = tmux::select_tui_window(&app.session, &app.tui_window_name);
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

pub fn execute_qa(app: &mut App, registry: &Registry) -> Result<()> {
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
        Ok(_) => {
            let _ = tmux::select_tui_window(&app.session, &app.tui_window_name);
            // The replaced window's opencode process exits (C-c) then a new one starts.
            // Both events can trigger tmux focus switches. Re-select after a brief
            // pause to win the race against opencode's startup terminal takeover.
            std::thread::sleep(std::time::Duration::from_millis(250));
            let _ = tmux::select_tui_window(&app.session, &app.tui_window_name);
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
            // can briefly steal focus away from the TUI window (tmux switches to
            // an adjacent window), which corrupts the alternate-screen buffer
            // and causes ratatui's incremental diff renderer to leave stale cells.
            // The double-select + 250ms sleep is the same pattern used by
            // execute_spawn / execute_plan / execute_qa (see lines above).
            let _ = tmux::select_tui_window(&app.session, &app.tui_window_name);
            std::thread::sleep(std::time::Duration::from_millis(250));
            let _ = tmux::select_tui_window(&app.session, &app.tui_window_name);

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

pub fn execute_send(app: &mut App, registry: &Registry) -> Result<()> {
    let wt_name = match app.selected_worktree() {
        Some(wt) => wt.window_name.clone(),
        None => return Ok(()),
    };
    let prompt = app.input_buf.trim().to_string();
    if prompt.is_empty() {
        app.set_status("Message is empty — type something first.");
        return Ok(());
    }
    match crate::cmd_send(registry, &wt_name, &prompt) {
        Ok(()) => {
            let _ = tmux::select_tui_window(&app.session, &app.tui_window_name);
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

pub fn execute_add_worktree(app: &mut App, registry: &Registry) -> Result<()> {
    // Input format: "<project-short> <worktree-name> [branch]"
    let raw = app.input_buf.trim().to_string();
    let mut parts = raw.splitn(3, ' ').map(str::trim).filter(|s| !s.is_empty());
    let project_short = match parts.next() {
        Some(p) => p.to_string(),
        None => {
            app.set_status("Usage: <project-short> <name> [branch]");
            return Ok(());
        }
    };
    let worktree_name = match parts.next() {
        Some(n) => n.to_string(),
        None => {
            app.set_status("Usage: <project-short> <name> [branch]");
            return Ok(());
        }
    };
    let branch: Option<String> = parts.next().map(|s| s.to_string());

    match crate::worktree::cmd_add_worktree(
        registry,
        &registry.base_dir,
        &project_short,
        &worktree_name,
        branch.as_deref(),
    ) {
        Ok(()) => {
            // Reload registry so the new worktree appears in the list.
            match crate::registry::Registry::load(registry.base_dir.clone()) {
                Ok(new_reg) => {
                    let new_count = new_reg.worktrees.len();
                    app.worktrees = new_reg.worktrees;
                    app.projects = new_reg.projects;
                    app.phases.resize(new_count, "?".to_string());
                    app.rebuild_entries();
                }
                Err(e) => {
                    app.set_status(format!("Worktree added but reload failed: {}", e));
                    app.reset_input();
                    return Ok(());
                }
            }
            app.set_status(format!(
                "Added worktree {}-{}.",
                project_short, worktree_name
            ));
            push_history(app, &raw);
            app.reset_input();
            app.refresh_phases();
        }
        Err(e) => {
            app.set_status(format!("Add worktree failed: {}", e));
            // Keep input so the user can fix it.
        }
    }
    Ok(())
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
