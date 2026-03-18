use anyhow::{bail, Context, Result};
use std::process::Command;
use tracing::debug;

fn tmux(args: &[&str]) -> Result<String> {
    debug!("tmux {}", args.join(" "));
    let output = Command::new("tmux").args(args).output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("tmux {} failed: {}", args.join(" "), stderr.trim())
    }
}

/// Return the name of the current tmux session, or error if not inside tmux.
pub fn current_session() -> Result<String> {
    std::env::var("TMUX")
        .context("Not inside a tmux session. task-master spawn must be run from within tmux.")?;
    tmux(&["display-message", "-p", "#S"]).context("Failed to get current tmux session name")
}

/// Strip any existing phase suffix from a window name.
/// "WIS-olive:dev" -> "WIS-olive", "WIS-olive" -> "WIS-olive"
pub fn base_window_name(name: &str) -> &str {
    name.find(':').map(|i| &name[..i]).unwrap_or(name)
}

/// Find the index of the window whose name starts with `base_name` (before any colon)
/// in the given session. Returns None if not found.
fn find_window_index(session: &str, base_name: &str) -> Option<String> {
    // list-windows -F "#{window_index} #{window_name}"
    let output = Command::new("tmux")
        .args([
            "list-windows",
            "-t",
            session,
            "-F",
            "#{window_index} #{window_name}",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let mut parts = line.splitn(2, ' ');
        let idx = parts.next()?;
        let name = parts.next()?;
        if base_window_name(name) == base_name {
            return Some(idx.to_string());
        }
    }
    None
}

/// Rename a window identified by its current base name to a new full name.
/// Looks up the window by base name (prefix before ':') to avoid tmux
/// target-parsing ambiguity with colons in window names.
///
/// `new_name` is the complete new name (may include a colon suffix).
pub fn rename_window(session: &str, current_base: &str, new_name: &str) -> Result<()> {
    let idx = find_window_index(session, current_base).with_context(|| {
        format!(
            "Window with base name '{}' not found in session '{}'",
            current_base, session
        )
    })?;
    let target = format!("{}:{}", session, idx);
    tmux(&["rename-window", "-t", &target, new_name])
        .with_context(|| format!("Failed to rename window {} to '{}'", target, new_name))?;
    Ok(())
}

/// Set the phase suffix on a worktree's dev window.
///
/// - `base_name`: the base window name without any phase, e.g. "WIS-olive"
/// - `phase`: Some("dev") -> "WIS-olive:dev", None -> "WIS-olive" (clears phase)
///
/// Non-fatal: if the window doesn't exist (e.g. was closed), logs a debug
/// message and returns Ok so callers don't need to handle the error.
pub fn set_window_phase(session: &str, base_name: &str, phase: Option<&str>) -> Result<()> {
    let new_name = match phase {
        Some(p) => format!("{}:{}", base_name, p),
        None => base_name.to_string(),
    };
    match rename_window(session, base_name, &new_name) {
        Ok(()) => {
            debug!("Window '{}' -> '{}'", base_name, new_name);
            Ok(())
        }
        Err(e) => {
            // Non-fatal: window may not exist yet or may have been closed.
            debug!("set_window_phase: {}", e);
            Ok(())
        }
    }
}

/// Spawn an opencode agent for a worktree.
///
/// `window_name` must be the **base** name (no phase suffix), e.g. "WIS-olive".
///
/// - If a window with that base name already exists (regardless of phase suffix):
///   send the prompt to the running opencode session and return false.
/// - Otherwise: create a new window named `<base>:dev`, start opencode, return true.
pub fn spawn_window(
    session: &str,
    window_name: &str,
    working_dir: &str,
    prompt: &str,
) -> Result<bool> {
    // Look up by base name so we find it even if it already has a phase suffix.
    if let Some(idx) = find_window_index(session, window_name) {
        let target = format!("{}:{}", session, idx);
        tmux(&["send-keys", "-t", &target, prompt])?;
        tmux(&["send-keys", "-t", &target, "Enter"])?;
        return Ok(false);
    }

    // New window — create it with the :dev phase suffix immediately.
    let dev_name = format!("{}:dev", window_name);
    let end_target = format!("{}:", session);
    let opencode_cmd = format!("opencode --prompt {}", shell_escape(prompt));
    tmux(&[
        "new-window",
        "-d", // don't switch to it
        "-t",
        &end_target,
        "-n",
        &dev_name,
        "-c",
        working_dir,
    ])?;

    // After creation the window is named dev_name; find it by base to get its index.
    let idx = find_window_index(session, window_name)
        .with_context(|| format!("Could not find newly created window '{}'", dev_name))?;
    let target = format!("{}:{}", session, idx);
    tmux(&["send-keys", "-t", &target, &opencode_cmd])?;
    tmux(&["send-keys", "-t", &target, "Enter"])?;

    Ok(true) // true = new window
}

fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base_window_name_with_phase() {
        assert_eq!(base_window_name("WIS-olive:dev"), "WIS-olive");
        assert_eq!(base_window_name("WIS-olive:qa"), "WIS-olive");
        assert_eq!(base_window_name("WIS-olive:review"), "WIS-olive");
        assert_eq!(base_window_name("WIS-olive:blocked"), "WIS-olive");
    }

    #[test]
    fn test_base_window_name_no_phase() {
        assert_eq!(base_window_name("WIS-olive"), "WIS-olive");
    }
}
