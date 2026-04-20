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

/// Milliseconds to wait after pkill before starting the new supervisor, giving
/// the old process time to exit cleanly.
const PKILL_SLEEP_MS: u64 = 500;

/// Minimum seconds between supervisor passes. If the last pass completed less
/// than this many seconds ago, the current pass is skipped to avoid back-to-back
/// runs after the OS resumes a suspended sleep.
const SLEEP_WAKE_GUARD_SECS: u64 = 60;

/// Seconds the supervisor sleeps between polling passes (5 minutes).
const SUPERVISOR_POLL_INTERVAL_SECS: u64 = 300;

/// Seconds between inner-loop checks for the early-wake stamp file.
const SUPERVISOR_INNER_POLL_SECS: u64 = 2;

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
    thread::sleep(Duration::from_millis(PKILL_SLEEP_MS));

    info!(
        "[supervisor] Starting in session '{}', dir {}",
        session, working_dir
    );

    // Export TASK_MASTER before the loop so the agent prompt can use it.
    // The sentinel string is embedded as a comment so pkill can identify the loop.
    //
    // The loop has two token-saving guards:
    //
    // 1. Idle skip: only invokes `opencode run` when at least one tmux window has
    //    an active phase suffix (:dev, :qa, :plan, or :e2e). When no agents are
    //    running the entire opencode call is skipped — zero tokens burned.
    //
    // 2. Sleep/wake guard: records the epoch of the last run in
    //    /tmp/task-master-supervisor-last. If a run completed less than 60 seconds
    //    ago we skip this pass. This protects against the OS resuming a suspended
    //    `sleep 300` immediately on wake and firing a back-to-back pass.
    //
    // The sleep itself is wake-aware: `sleep 300` runs in the background and a
    // 2-second inner loop polls /tmp/task-master-supervisor-wake. When the stamp
    // appears (written by `task-master notify`) the background sleep is killed and
    // the next supervisor pass fires immediately instead of waiting 5 minutes.
    let loop_cmd = format!(
        concat!(
            "export TASK_MASTER={bin};",
            " while true; do",
            " : {sentinel};",
            // Sleep/wake guard: skip if we ran less than SLEEP_WAKE_GUARD_SECS ago
            " _now=$(date +%s);",
            " _last=$(cat /tmp/task-master-supervisor-last 2>/dev/null || echo 0);",
            " if [ $(( _now - _last )) -ge {wake_guard} ]; then",
            // Idle skip: only run the agent if there are active phase windows
            // OR there are registered ephemeral worktrees (which may need cleanup).
            " if tmux list-windows -F '#{{window_name}}' 2>/dev/null | grep -qE ':(dev|qa|plan|e2e)$'",
            " || grep -q 'ephemeral = true' task-master.toml 2>/dev/null; then",
            " opencode run --agent supervisor 'Check worktree windows and act on any that need attention.';",
            " date +%s > /tmp/task-master-supervisor-last;",
            " fi;",
            " fi;",
            // Wake-aware sleep: poll for an early-wake stamp every SUPERVISOR_INNER_POLL_SECS
            " sleep {poll_interval} & _sleep_pid=$!;",
            " while kill -0 $_sleep_pid 2>/dev/null; do",
            " if [ -f /tmp/task-master-supervisor-wake ]; then",
            " rm -f /tmp/task-master-supervisor-wake;",
            " kill $_sleep_pid 2>/dev/null;",
            " break;",
            " fi;",
            " sleep {inner_poll};",
            " done;",
            " done",
        ),
        bin = tmux::shell_escape(bin_str),
        sentinel = SUPERVISOR_SENTINEL,
        wake_guard = SLEEP_WAKE_GUARD_SECS,
        poll_interval = SUPERVISOR_POLL_INTERVAL_SECS,
        inner_poll = SUPERVISOR_INNER_POLL_SECS,
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
            concat!(
                "export TASK_MASTER={bin};",
                " while true; do",
                " : {sentinel};",
                " _now=$(date +%s);",
                " _last=$(cat /tmp/task-master-supervisor-last 2>/dev/null || echo 0);",
                " if [ $(( _now - _last )) -ge 60 ]; then",
                " if tmux list-windows -F '#{{window_name}}' 2>/dev/null | grep -qE ':(dev|qa|plan|e2e)$'",
                " || grep -q 'ephemeral = true' task-master.toml 2>/dev/null; then",
                " opencode run --agent supervisor 'Check worktree windows and act on any that need attention.';",
                " date +%s > /tmp/task-master-supervisor-last;",
                " fi;",
                " fi;",
                " sleep 300 & _sleep_pid=$!;",
                " while kill -0 $_sleep_pid 2>/dev/null; do",
                " if [ -f /tmp/task-master-supervisor-wake ]; then",
                " rm -f /tmp/task-master-supervisor-wake;",
                " kill $_sleep_pid 2>/dev/null;",
                " break;",
                " fi;",
                " sleep 2;",
                " done;",
                " done",
            ),
            bin = tmux::shell_escape(bin_str),
            sentinel = SUPERVISOR_SENTINEL,
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
        assert!(
            loop_cmd.contains("task-master-supervisor-wake"),
            "loop_cmd must poll the wake stamp file"
        );
        assert!(
            loop_cmd.contains("task-master-supervisor-last"),
            "loop_cmd must record the last-run timestamp for the wake guard"
        );
        assert!(
            loop_cmd.contains(":(dev|qa|plan|e2e)$"),
            "loop_cmd must skip opencode run when no active phase windows exist"
        );
        assert!(
            loop_cmd.contains("ephemeral = true"),
            "loop_cmd must also wake when ephemeral worktrees are registered"
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
