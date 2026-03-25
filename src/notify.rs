use crate::registry::Registry;
use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

/// Path of the wake stamp that causes the supervisor loop to fire immediately.
pub const WAKE_STAMP: &str = "/tmp/task-master-supervisor-wake";

/// Path of the notify event file for a given worktree base name.
pub fn notify_path(base_name: &str) -> PathBuf {
    PathBuf::from(format!("/tmp/task-master-notify-{}.json", base_name))
}

/// Write a QA-ready notification for the supervisor.
///
/// Writes `/tmp/task-master-notify-<base>.json` and touches
/// `/tmp/task-master-supervisor-wake` so the supervisor loop wakes within
/// ~2 seconds instead of waiting up to 5 minutes.
///
/// The dev agent calls this instead of `task-master qa` — calling `qa` directly
/// would kill the running opencode session (via `replace_window_process`) before
/// the bash tool call could return, causing a silent failure.
pub fn cmd_notify(registry: &Registry, worktree_name: &str, pr_number: u64) -> Result<()> {
    let base_name = crate::tmux::base_window_name(worktree_name);

    // Validate worktree exists in registry.
    registry
        .require_worktree(worktree_name)
        .with_context(|| format!("Unknown worktree '{}'", worktree_name))?;

    // Write the event file.
    let path = notify_path(base_name);
    let json = format!("{{\"worktree\":\"{}\",\"pr\":{}}}\n", base_name, pr_number);
    fs::write(&path, &json).with_context(|| format!("Failed to write notify file {:?}", path))?;

    // Touch the wake stamp so the supervisor loop skips its remaining sleep.
    fs::write(WAKE_STAMP, b"")
        .with_context(|| format!("Failed to write wake stamp {}", WAKE_STAMP))?;

    println!(
        "Notified supervisor: QA requested for {} PR #{}",
        base_name, pr_number
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notify_path() {
        assert_eq!(
            notify_path("WIS-cedar"),
            PathBuf::from("/tmp/task-master-notify-WIS-cedar.json")
        );
    }

    #[test]
    fn test_notify_path_base_name_only() {
        // Ensure colons in window names (e.g. "WIS-cedar:dev") are stripped.
        // notify_path itself doesn't strip — callers pass base_name already.
        assert_eq!(
            notify_path("WIS-olive"),
            PathBuf::from("/tmp/task-master-notify-WIS-olive.json")
        );
    }

    #[test]
    fn test_wake_stamp_path() {
        assert_eq!(WAKE_STAMP, "/tmp/task-master-supervisor-wake");
    }
}
