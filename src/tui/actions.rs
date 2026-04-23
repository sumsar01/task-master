use super::app::{ActionKind, AddProjectStep, App, CloningOp, Mode};
use crate::tmux;
use anyhow::Result;

/// How long to wait (ms) after spawning/killing a tmux window before
/// re-selecting the TUI window. opencode's startup terminal takeover can
/// trigger a tmux activity event that steals focus; this pause wins the race.
const TMUX_REFOCUS_DELAY_MS: u64 = 250;

// ---------------------------------------------------------------------------
// Action execution
// ---------------------------------------------------------------------------

/// Which low-level operation `execute_send_build` should perform for a given
/// window phase. Pure (no I/O) so it can be unit-tested without a live tmux
/// session.
#[derive(Debug, PartialEq)]
pub enum SendBuildAction {
    /// Phase is "plan": send Tab to switch opencode to build agent, then send
    /// the prompt.
    SwitchThenSend,
    /// Phase is "dev": opencode is already in build mode — send the prompt
    /// directly.
    SendDirect,
    /// Phase is "ready": the planning agent exited but the window still exists.
    /// Send the prompt to the window and rename the phase to "dev".
    SendToReady,
    /// Any other phase (qa, review, blocked, idle, …): the action is rejected
    /// with an error message for the user.
    Rejected(String),
}

/// Determine what `execute_send_build` should do based on the current window
/// phase. Pure function — no side effects, no tmux calls.
pub fn send_build_action_for_phase(phase: &str) -> SendBuildAction {
    match phase {
        "plan" => SendBuildAction::SwitchThenSend,
        "dev" => SendBuildAction::SendDirect,
        "ready" => SendBuildAction::SendToReady,
        other => SendBuildAction::Rejected(format!(
            "Cannot send in phase '{}' — use 's' to spawn a fresh agent.",
            other
        )),
    }
}

pub fn execute_action(app: &mut App, kind: &ActionKind, force: bool) -> Result<()> {
    match kind {
        ActionKind::Spawn => execute_spawn(app, force),
        ActionKind::Plan => execute_plan(app),
        ActionKind::Qa => execute_qa(app),
        ActionKind::Send => execute_send(app),
        ActionKind::SendBuild => execute_send_build(app),
        ActionKind::AddWorktree => execute_add_worktree(app),
        ActionKind::AddProject => execute_add_project(app),
        ActionKind::SpawnEphemeral => execute_spawn_ephemeral(app),
    }
}

pub fn execute_spawn(app: &mut App, force: bool) -> Result<()> {
    let (wt_name, prompt) = match collect_spawn_inputs(app) {
        Some(x) => x,
        None => return Ok(()),
    };

    // Quick synchronous check: if the worktree has uncommitted changes and
    // force is false, surface the ForceConfirm prompt before entering Cloning
    // mode (the user needs to confirm interactively first).
    //
    // We do this by running a lightweight git-status check before spawning the
    // background thread so we don't block the TUI for the full fetch+reset.
    if !force {
        if let Some(wt) = app.registry.find_worktree(&wt_name) {
            let has_changes = std::process::Command::new("git")
                .arg("-C")
                .arg(&wt.abs_path)
                .args(["status", "--porcelain"])
                .output()
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false);
            if has_changes {
                app.set_status(format!(
                    "{} has uncommitted changes. Press Enter to force-reset and spawn, Esc to cancel.",
                    wt_name
                ));
                app.mode = Mode::ForceConfirm;
                return Ok(());
            }
        }
    }

    // Kick off background thread: reset worktree + spawn tmux window.
    let registry = app.registry.clone();
    let label = format!("Spawning {}…", wt_name);

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = crate::spawn::cmd_spawn(&registry, &wt_name, &prompt, force);
        let msg = result
            .map(|_| format!("Spawned {}:dev", wt_name))
            .map_err(|e| format!("Spawn failed: {}", e));
        let _ = tx.send(msg);
    });

    // Save the prompt for history so run_loop can push it after completion.
    let prompt_for_history = collect_spawn_inputs_prompt(app);
    app.clone_rx = Some(rx);
    app.cloning_label = label;
    app.cloning_op = CloningOp::Spawn;
    app.pending_history_entry = prompt_for_history;
    app.reset_input();
    app.mode = Mode::Cloning;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_send_build_action_plan_phase() {
        assert_eq!(
            send_build_action_for_phase("plan"),
            SendBuildAction::SwitchThenSend
        );
    }

    #[test]
    fn test_send_build_action_dev_phase() {
        assert_eq!(
            send_build_action_for_phase("dev"),
            SendBuildAction::SendDirect
        );
    }

    #[test]
    fn test_send_build_action_ready_phase() {
        assert_eq!(
            send_build_action_for_phase("ready"),
            SendBuildAction::SendToReady
        );
    }

    #[test]
    fn test_send_build_action_rejected_phases() {
        for phase in &["qa", "review", "blocked", "idle", "", "?", "dev-stalled"] {
            match send_build_action_for_phase(phase) {
                SendBuildAction::Rejected(msg) => {
                    assert!(
                        msg.contains(phase),
                        "Rejected message should contain the phase name '{}', got: {}",
                        phase,
                        msg
                    );
                }
                other => panic!(
                    "Expected Rejected for phase '{}', got {:?}",
                    phase, other
                ),
            }
        }
    }

    #[test]
    fn test_send_build_action_rejected_contains_hint() {
        // The rejected message should tell the user to use 's' to spawn.
        match send_build_action_for_phase("qa") {
            SendBuildAction::Rejected(msg) => {
                assert!(msg.contains("'s'"), "Hint to use 's' missing: {}", msg);
            }
            other => panic!("Expected Rejected, got {:?}", other),
        }
    }
}


/// Extract only the prompt text from the input buffer (without consuming it).
/// Returns `Some(prompt)` if non-empty, `None` otherwise.
fn collect_spawn_inputs_prompt(app: &App) -> Option<String> {
    let p = app.input_buf.trim().to_string();
    if p.is_empty() {
        None
    } else {
        Some(p)
    }
}

pub fn execute_plan(app: &mut App) -> Result<()> {
    let (wt_name, prompt) = match collect_spawn_inputs(app) {
        Some(x) => x,
        None => return Ok(()),
    };

    let registry = app.registry.clone();
    let label = format!("Planning {}…", wt_name);

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = crate::plan::cmd_plan(&registry, &wt_name, &prompt);
        let msg = result
            .map(|_| format!("Plan agent started in {}:plan", wt_name))
            .map_err(|e| format!("Plan failed: {}", e));
        let _ = tx.send(msg);
    });

    let prompt_for_history = collect_spawn_inputs_prompt(app);
    app.clone_rx = Some(rx);
    app.cloning_label = label;
    app.cloning_op = CloningOp::Plan;
    app.pending_history_entry = prompt_for_history;
    app.reset_input();
    app.mode = Mode::Cloning;
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

/// Send a message to the running opencode session, switching to build mode first
/// if the worktree is currently in plan phase.
///
/// - If the window phase is "plan": sends a Tab keypress (cycles opencode from
///   plan → build agent) followed by the prompt, then renames the window to :dev.
/// - If the window phase is "dev" (already build mode): sends the prompt directly,
///   same as execute_send.
/// - Any other active phase: shows an error; the user should use 'm' (Send) instead.
///
/// Relies on qa/supervisor/e2e agents having mode: subagent so that Tab cycling
/// only alternates between the two project-defined primary agents: build and plan.
pub fn execute_send_build(app: &mut App) -> Result<()> {
    let wt_name = match app.selected_worktree() {
        Some(wt) => wt.window_name.clone(),
        None => return Ok(()),
    };
    let prompt = app.input_buf.trim().to_string();
    if prompt.is_empty() {
        app.set_status("Message is empty — type something first.");
        return Ok(());
    }
    let phase = app.selected_phase().to_string();
    let session = match crate::tmux::current_session() {
        Ok(s) => s,
        Err(e) => {
            app.set_status(format!("Send (build) failed: {}", e));
            app.reset_input();
            return Ok(());
        }
    };
    match send_build_action_for_phase(&phase) {
        SendBuildAction::SwitchThenSend => {
            // Tab switches opencode from plan → build agent, then send the message.
            match crate::tmux::send_tab_then_message(&session, &wt_name, &prompt) {
                Ok(()) => {
                    // Rename the window from :plan to :dev to reflect the new mode.
                    let _ = crate::tmux::set_window_phase(&session, &wt_name, Some("dev"));
                    let _ = tmux::select_window_by_id(&app.session, &app.tui_window_id);
                    app.set_status(format!(
                        "Switched to build mode and sent message to {}.",
                        wt_name
                    ));
                    push_history(app, &prompt);
                    app.reset_input();
                    app.refresh_phases();
                }
                Err(e) => {
                    app.set_status(format!("Send (build) failed: {}", e));
                    app.reset_input();
                }
            }
        }
        SendBuildAction::SendToReady => {
            // Planning is done; opencode has exited but the tmux window still
            // exists. Send the message directly and rename the window to :dev.
            // We do NOT spawn a fresh agent — 'b' means "send message", not
            // "spawn". Use 's' to spawn a fresh agent.
            match crate::cmd_send(&app.registry, &wt_name, &prompt) {
                Ok(()) => {
                    let _ = crate::tmux::set_window_phase(&session, &wt_name, Some("dev"));
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
        }
        SendBuildAction::SendDirect => {
            // Already in build mode — send normally.
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
        }
        SendBuildAction::Rejected(msg) => {
            app.set_status(msg);
            app.reset_input();
        }
    }
    Ok(())
}

/// Create a new ephemeral git worktree for the currently selected project.
///
/// The worktree name is read from `app.input_buf`. The project is inferred
/// from the selected entry (Worktree row → its project; ProjectHeader → that
/// project). The operation is kicked off in a background thread so the TUI
/// remains animated (spinner) while git worktree add + bd init are running.
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
    let registry = app.registry.clone();
    let label = format!("Adding {}-{}…", project_short, name);

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result =
            crate::worktree::cmd_add_worktree(&registry, &base_dir, &project_short, &name, None);
        let msg = result
            .map(|_| {
                format!(
                    "Added {}-{}. Press s to spawn an agent.",
                    project_short, name
                )
            })
            .map_err(|e| format!("Add worktree failed: {}", e));
        let _ = tx.send(msg);
    });

    app.clone_rx = Some(rx);
    app.cloning_label = label;
    app.cloning_op = CloningOp::AddWorktree;
    app.reset_input();
    app.mode = Mode::Cloning;
    Ok(())
}

/// Remove the currently selected git worktree (runs `git worktree remove` and
/// removes the entry from `task-master.toml`). The operation is kicked off in
/// a background thread so the TUI remains animated while git cleans up.
pub fn execute_remove_worktree(app: &mut App) -> Result<()> {
    let window_name = match app.selected_worktree() {
        Some(wt) => wt.window_name.clone(),
        None => return Ok(()),
    };

    let base_dir = app.registry.base_dir.clone();
    let registry = app.registry.clone();
    let label = format!("Removing {}…", window_name);

    // Run a quick synchronous check first: if the worktree has dirty files we
    // need to surface the ForceConfirm prompt *before* entering Cloning mode,
    // because the user needs to confirm interactively.
    match crate::worktree::cmd_remove_worktree(&registry, &base_dir, &window_name, false, false) {
        Ok(()) => {
            // Fast path: removal succeeded synchronously (no dirty files check
            // — worktree remove is usually fast when clean). Use the background
            // channel pattern so the run_loop handler reloads the registry.
            let (tx, rx) = std::sync::mpsc::channel();
            let _ = tx.send(Ok(format!("Removed {}.", window_name)));
            app.clone_rx = Some(rx);
            app.cloning_label = label;
            app.cloning_op = CloningOp::RemoveWorktree;
            app.mode = Mode::Cloning;
            app.needs_full_redraw = true;
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

/// Returns distinct sorted values for a string field from all projects.
fn collect_cycle_options<F>(app: &App, f: F) -> Vec<String>
where
    F: Fn(&crate::registry::ProjectConfig) -> Option<&str>,
{
    let mut seen = std::collections::HashSet::new();
    let mut opts: Vec<String> = app
        .registry
        .projects
        .iter()
        .filter_map(|p| f(p).map(str::to_owned))
        .filter(|s| seen.insert(s.clone()))
        .collect();
    opts.sort();
    opts
}

/// Add a new project via a six-step prompt sequence:
///   Name → Short → URL → Account → Group (optional) → Context (optional)
///
/// Called on each Enter press while `Mode::Prompt(ActionKind::AddProject)` is active.
/// After the Context step the clone is kicked off in a background thread and
/// `app.mode` transitions to `Mode::Cloning`.
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
            // Pre-populate with the first logged-in gh account.
            let accounts = gh_accounts();
            let hint = accounts.join(" / ");
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
            app.pending_project_account = input;
            app.input_buf.clear();
            app.cursor_pos = 0;
            app.add_project_step = Some(AddProjectStep::Group);
            // Build cycle options from existing groups.
            let opts = collect_cycle_options(app, |p| p.group.as_deref());
            let hint = if opts.is_empty() {
                "none yet".to_string()
            } else {
                opts.join(" / ")
            };
            app.group_cycle_options = opts;
            // Leave input empty — user can Tab to cycle or type a new name.
            app.set_status(format!(
                "Enter group (Tab to cycle: {}) or leave empty:",
                hint
            ));
        }
        AddProjectStep::Group => {
            // Empty = no group (ungrouped), non-empty = group name.
            app.pending_project_group = if input.is_empty() { None } else { Some(input) };
            app.input_buf.clear();
            app.cursor_pos = 0;
            app.add_project_step = Some(AddProjectStep::Context);
            // Build cycle options from existing contexts.
            let opts = collect_cycle_options(app, |p| p.context.as_deref());
            let hint = if opts.is_empty() {
                "none yet".to_string()
            } else {
                opts.join(" / ")
            };
            app.context_cycle_options = opts;
            app.set_status(format!(
                "Enter bounded context (Tab to cycle: {}) or leave empty:",
                hint
            ));
        }
        AddProjectStep::Context => {
            // Empty = no context, non-empty = context tag.
            app.pending_project_context = if input.is_empty() { None } else { Some(input) };

            // All metadata collected — kick off background clone.
            let name = app.pending_project_name.clone();
            let short = app.pending_project_short.clone();
            let url = app.pending_project_url.clone();
            let account = app.pending_project_account.clone();
            let group = app.pending_project_group.clone();
            let context = app.pending_project_context.clone();
            let base_dir = app.registry.base_dir.clone();

            // Derive the spinner label before url is moved into the thread.
            let label = url
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or(&url)
                .trim_end_matches(".git")
                .to_string();

            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let result = crate::cmd_add_project(
                    &base_dir,
                    &name,
                    &short,
                    &url,
                    Some(&account),
                    group.as_deref(),
                    context.as_deref(),
                );
                let msg = result
                    .map(|_| {
                        format!(
                            "Added project {} ({}). Press N to add a worktree.",
                            name, short
                        )
                    })
                    .map_err(|e| e.to_string());
                let _ = tx.send(msg);
            });

            app.clone_rx = Some(rx);
            app.cloning_label = format!("Cloning {}…", label);
            app.cloning_op = CloningOp::AddProject;
            // reset_input clears input/mode fields but we override mode to Cloning.
            app.reset_input();
            app.mode = Mode::Cloning;
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
    let registry = app.registry.clone();
    let label = format!("Force-removing {}…", window_name);

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result =
            crate::worktree::cmd_remove_worktree(&registry, &base_dir, &window_name, true, false);
        let msg = result
            .map(|()| format!("Force-removed {}.", window_name))
            .map_err(|e| format!("Force-remove failed: {}", e));
        let _ = tx.send(msg);
    });

    app.clone_rx = Some(rx);
    app.cloning_label = label;
    app.cloning_op = CloningOp::RemoveWorktree;
    app.mode = Mode::Cloning;
    app.needs_full_redraw = true;
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
pub(super) fn refocus_tui_window(session: &str, tui_window_id: &str) {
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

/// Open the PR for the selected worktree's current branch in the default browser.
///
/// Branch detection is synchronous (local, instant). The `gh pr view` network
/// call and browser open are dispatched to a background thread so the TUI
/// stays fully responsive. Result is delivered via `app.bg_status_rx`.
pub fn execute_open_pr(app: &mut App) -> Result<()> {
    let wt = match app.selected_worktree() {
        Some(w) => w.clone(),
        None => return Ok(()),
    };

    // Get current branch name synchronously — this is a local git operation, instant.
    let branch_out = std::process::Command::new("git")
        .args(["-C", wt.abs_path.to_str().unwrap_or("."), "rev-parse", "--abbrev-ref", "HEAD"])
        .output();
    let branch = match branch_out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => {
            app.set_status("Could not determine current branch.");
            return Ok(());
        }
    };

    // Show immediate feedback so the user knows the key press registered.
    app.set_status(format!("Looking up PR for '{}'…", branch));

    // Spawn background thread for the network call + browser open.
    let abs_path = wt.abs_path.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // Look up PR URL via gh.
        let pr_out = std::process::Command::new("gh")
            .args(["pr", "view", &branch, "--json", "url", "--jq", ".url"])
            .current_dir(&abs_path)
            .output();

        let url = match pr_out {
            Ok(o) if o.status.success() => {
                let u = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if u.is_empty() {
                    let _ = tx.send(format!("No open PR found for branch '{}'.", branch));
                    return;
                }
                u
            }
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
                let _ = tx.send(format!("gh error: {}", err));
                return;
            }
            Err(e) => {
                let _ = tx.send(format!("Failed to run gh: {}", e));
                return;
            }
        };

        // Open in default browser.
        #[cfg(target_os = "macos")]
        let open_cmd = "open";
        #[cfg(not(target_os = "macos"))]
        let open_cmd = "xdg-open";

        match std::process::Command::new(open_cmd).arg(&url).status() {
            Ok(_) => { let _ = tx.send(format!("Opened PR in browser: {}", url)); }
            Err(e) => { let _ = tx.send(format!("Failed to open browser: {}", e)); }
        }
    });

    app.bg_status_rx = Some(rx);
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

// ---------------------------------------------------------------------------
// Ephemeral spawn
// ---------------------------------------------------------------------------

/// Spawn an agent in a freshly created ephemeral worktree.
///
/// The project is inferred from the selected row (Worktree → its project;
/// ProjectHeader → that project). The worktree name is auto-generated; the
/// user only supplies the agent task prompt.
pub fn execute_spawn_ephemeral(app: &mut App) -> Result<()> {
    let project_short = match app.selected_project_short() {
        Some(s) => s,
        None => {
            app.set_status("Select a project or worktree first (use j/k to navigate).");
            return Ok(());
        }
    };

    let prompt = app.input_buf.trim().to_string();
    if prompt.is_empty() {
        app.set_status("Prompt cannot be empty.");
        return Ok(());
    }

    push_history(app, &prompt);
    app.reset_input();

    let base_dir = app.registry.base_dir.clone();
    match crate::spawn::cmd_spawn_ephemeral(&app.registry, &base_dir, &project_short, &prompt) {
        Ok(msg) => {
            match crate::registry::Registry::load(base_dir) {
                Ok(new_reg) => app.reload_from_registry(new_reg),
                Err(e) => {
                    app.set_status(format!(
                        "Spawned ephemeral worktree but failed to reload config: {}",
                        e
                    ));
                    app.mode = Mode::Normal;
                    app.needs_full_redraw = true;
                    return Ok(());
                }
            }
            refocus_tui_window(&app.session, &app.tui_window_id);
            app.mode = Mode::Normal;
            // Show first line of cmd_spawn_ephemeral's success message (avoids multi-line status).
            let short_msg = msg.lines().next().unwrap_or(&msg).to_string();
            app.set_status(short_msg);
            app.needs_full_redraw = true;
            app.refresh_phases();
        }
        Err(e) => {
            app.mode = Mode::Normal;
            app.set_status(format!("Ephemeral spawn failed: {}", e));
            app.needs_full_redraw = true;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Cleanup merged ephemeral worktrees
// ---------------------------------------------------------------------------

/// Remove all ephemeral worktrees whose branch is merged or PR is closed.
///
/// Called after the user confirms with 'y' from `ConfirmCleanup` mode.
/// Always runs with `force = true` (non-interactive; the TUI modal already
/// obtained confirmation).
pub fn execute_cleanup_merged(app: &mut App) -> Result<()> {
    let base_dir = app.registry.base_dir.clone();
    match crate::cleanup::cmd_cleanup(&app.registry, &base_dir, true, false, true) {
        Ok(()) => {
            match crate::registry::Registry::load(base_dir) {
                Ok(new_reg) => app.reload_from_registry(new_reg),
                Err(e) => {
                    app.set_status(format!("Cleanup ran but failed to reload config: {}", e));
                    app.mode = Mode::Normal;
                    app.needs_full_redraw = true;
                    return Ok(());
                }
            }
            app.mode = Mode::Normal;
            app.set_status("Cleanup complete — merged ephemeral worktrees removed.");
            app.needs_full_redraw = true;
            app.refresh_phases();
        }
        Err(e) => {
            app.mode = Mode::Normal;
            app.set_status(format!("Cleanup failed: {}", e));
            app.needs_full_redraw = true;
        }
    }
    Ok(())
}
