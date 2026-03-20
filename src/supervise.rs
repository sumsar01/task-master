use crate::registry::Registry;
use crate::tmux;
use anyhow::{Context, Result};
use std::process::Command;
use std::thread;
use std::time::Duration;
use tracing::info;

/// Spawn a supervisor agent for the current session.
///
/// Kills any existing supervisor loop processes first (prevents double-supervisor
/// races from repeated invocations), then opens a tmux window named `supervisor`
/// running a `while true` shell loop that invokes `opencode run --agent supervisor`
/// once per iteration and sleeps 300 seconds between passes.
///
/// Each `opencode run` invocation is a single-pass check: inspect windows, act,
/// print summary, exit. The shell loop handles repetition so the agent itself
/// does not need to manage its own polling loop.
///
/// The supervisor agent's system prompt is in `.opencode/agents/supervisor.md`.
pub fn cmd_supervise(registry: &Registry) -> Result<()> {
    let session = tmux::current_session()?;
    let working_dir = registry.base_dir.to_string_lossy().to_string();

    // Kill any existing supervisor loop to avoid two supervisors running
    // simultaneously and fighting over window names.
    info!("[supervisor] Killing any existing supervisor processes...");
    let _ = Command::new("pkill")
        .args(["-f", "opencode run --agent supervisor"])
        .status();
    thread::sleep(Duration::from_millis(500));

    info!(
        "[supervisor] Starting in session '{}', dir {}",
        session, working_dir
    );

    // Build a shell loop: each pass runs one opencode invocation (single-pass
    // agent), then sleeps 300 s before the next pass. C-c in the tmux window
    // kills the loop.
    let loop_cmd =
        "while true; do opencode run --agent supervisor 'Check worktree windows and act on any that need attention.'; sleep 300; done";

    tmux::spawn_named_window_raw(&session, "supervisor", &working_dir, loop_cmd)
        .context("Failed to open supervisor window")?;

    println!(
        "Supervisor started in window 'supervisor' (session '{}').",
        session
    );
    println!("It polls registered worktree windows every 5 minutes.");
    println!("To stop it: switch to the 'supervisor' window and press C-c.");

    Ok(())
}

#[cfg(test)]
mod tests {
    // cmd_supervise requires a live tmux session and is integration-tested manually.
}
