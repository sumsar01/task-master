use crate::registry::Registry;
use crate::tmux;
use anyhow::{Context, Result};
use std::process::Command;
use std::thread;
use std::time::Duration;
use tracing::info;

/// The sentinel string embedded in the supervisor loop command, used by
/// `pkill -f` to identify and kill existing supervisor processes.
const SUPERVISOR_SENTINEL: &str = "task-master-supervisor-loop";

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
///
/// `TASK_MASTER` is exported into the loop environment using the path of the
/// currently-running binary (`std::env::current_exe`), so the supervisor agent
/// can invoke `$TASK_MASTER qa <worktree> <pr>` regardless of whether the
/// binary is on PATH or whether a release build exists.
pub fn cmd_supervise(registry: &Registry) -> Result<()> {
    let session = tmux::current_session()?;
    let working_dir = registry.base_dir.to_string_lossy().to_string();

    // Resolve the path of the currently-running binary so the supervisor can
    // invoke it as $TASK_MASTER without relying on PATH or a specific build dir.
    let bin_path = std::env::current_exe().context("Failed to resolve current executable path")?;
    let bin_str = bin_path
        .to_str()
        .context("Executable path is not valid UTF-8")?;

    // Kill any existing supervisor loop to avoid two supervisors running
    // simultaneously and fighting over window names.
    info!("[supervisor] Killing any existing supervisor processes...");
    let _ = Command::new("pkill")
        .args(["-f", SUPERVISOR_SENTINEL])
        .status();
    thread::sleep(Duration::from_millis(500));

    info!(
        "[supervisor] Starting in session '{}', dir {}",
        session, working_dir
    );

    // Export TASK_MASTER before the loop so the agent prompt can use it.
    // The sentinel string is embedded as a comment so pkill can identify the loop.
    let loop_cmd = format!(
        "export TASK_MASTER={}; while true; do : {}; opencode run --agent supervisor 'Check worktree windows and act on any that need attention.'; sleep 300; done",
        tmux::shell_escape(bin_str),
        SUPERVISOR_SENTINEL,
    );

    tmux::spawn_named_window_raw(&session, "supervisor", &working_dir, &loop_cmd)
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
    use super::*;

    #[test]
    fn test_supervisor_sentinel_in_loop_cmd() {
        // The sentinel must appear in every loop command so pkill can find it.
        // We can't call cmd_supervise (needs live tmux) but we can verify the
        // format string produces the right output for a known bin path.
        let bin_str = "/usr/local/bin/task-master";
        let loop_cmd = format!(
            "export TASK_MASTER={}; while true; do : {}; opencode run --agent supervisor 'Check worktree windows and act on any that need attention.'; sleep 300; done",
            tmux::shell_escape(bin_str),
            SUPERVISOR_SENTINEL,
        );
        assert!(
            loop_cmd.contains(SUPERVISOR_SENTINEL),
            "loop_cmd must contain the sentinel so pkill can target it"
        );
        assert!(
            loop_cmd.contains("TASK_MASTER="),
            "loop_cmd must export TASK_MASTER"
        );
        assert!(
            loop_cmd.contains(bin_str),
            "loop_cmd must embed the binary path"
        );
        assert!(
            loop_cmd.contains("opencode run --agent supervisor"),
            "loop_cmd must invoke the supervisor agent"
        );
    }

    #[test]
    fn test_supervisor_pkill_targets_sentinel_not_opencode() {
        // The pkill pattern must target our sentinel, not "opencode run --agent supervisor",
        // to avoid killing unrelated opencode processes.
        assert_ne!(
            SUPERVISOR_SENTINEL, "opencode run --agent supervisor",
            "pkill must target the sentinel, not opencode directly"
        );
        assert!(
            SUPERVISOR_SENTINEL.contains("task-master"),
            "sentinel should be namespaced to task-master to avoid false matches"
        );
    }
}
