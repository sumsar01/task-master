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
        .context("This task-master command must be run from within a tmux session.")?;
    tmux(&["display-message", "-p", "#S"]).context("Failed to get current tmux session name")
}

/// Strip any existing phase suffix from a window name.
/// "WIS-olive:dev" -> "WIS-olive", "WIS-olive" -> "WIS-olive"
pub fn base_window_name(name: &str) -> &str {
    name.find(':').map(|i| &name[..i]).unwrap_or(name)
}

/// Find the index of the window whose name starts with `base_name` (before any colon)
/// in the given session. Returns None if not found.
pub fn find_window_index(session: &str, base_name: &str) -> Option<String> {
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

/// Interrupt whatever is running in an existing window and start a fresh
/// opencode session with the given prompt.
///
/// Sends C-c twice (to reliably exit the opencode TUI), waits briefly, then
/// runs `opencode run [--agent <agent>] <prompt>` in the same window. The
/// window is identified by its base name (prefix before any ':').
pub fn replace_window_process(
    session: &str,
    base_name: &str,
    working_dir: &str,
    prompt: &str,
    agent: Option<&str>,
) -> Result<()> {
    let idx = find_window_index(session, base_name).with_context(|| {
        format!(
            "Window with base name '{}' not found in session '{}'",
            base_name, session
        )
    })?;
    let target = format!("{}:{}", session, idx);

    // Two C-c presses to ensure the opencode TUI exits cleanly.
    tmux(&["send-keys", "-t", &target, "C-c"])?;
    std::thread::sleep(std::time::Duration::from_millis(200));
    tmux(&["send-keys", "-t", &target, "C-c"])?;
    std::thread::sleep(std::time::Duration::from_millis(500));

    // cd to working dir then launch fresh opencode TUI with the prompt.
    let opencode_cmd = build_opencode_cmd(prompt, agent);
    let cmd = format!("cd {} && {}", shell_escape(working_dir), opencode_cmd);
    tmux(&["send-keys", "-t", &target, &cmd])?;
    tmux(&["send-keys", "-t", &target, "Enter"])?;

    Ok(())
}

/// Spawn an opencode agent for a worktree.
///
/// `window_name` must be the **base** name (no phase suffix), e.g. "WIS-olive".
/// `agent` is an optional opencode agent name (e.g. `"plan"`, `"build"`). When
/// `None` the opencode default agent is used.
///
/// - If a window with that base name already exists (regardless of phase suffix):
///   send the prompt to the running opencode session and return false.
/// - Otherwise: create a new window named `<base>:dev`, start opencode, return true.
pub fn spawn_window(
    session: &str,
    window_name: &str,
    working_dir: &str,
    prompt: &str,
    agent: Option<&str>,
) -> Result<bool> {
    // Look up by base name so we find it even if it already has a phase suffix.
    if let Some(idx) = find_window_index(session, window_name) {
        let target = format!("{}:{}", session, idx);
        tmux(&["send-keys", "-t", &target, prompt])?;
        tmux(&["send-keys", "-t", &target, "Enter"])?;
        return Ok(false);
    }

    // New window — always created with the :dev phase suffix.
    let initial_name = format!("{}:dev", window_name);
    let end_target = format!("{}:", session);
    let opencode_cmd = build_opencode_cmd(prompt, agent);
    tmux(&[
        "new-window",
        "-d", // don't switch to it
        "-t",
        &end_target,
        "-n",
        &initial_name,
        "-c",
        working_dir,
    ])?;

    // After creation the window is named initial_name; find it by base to get its index.
    let idx = find_window_index(session, window_name)
        .with_context(|| format!("Could not find newly created window '{}'", initial_name))?;
    let target = format!("{}:{}", session, idx);
    tmux(&["send-keys", "-t", &target, &opencode_cmd])?;
    tmux(&["send-keys", "-t", &target, "Enter"])?;

    Ok(true) // true = new window
}

/// Spawn or replace a named tmux window running an arbitrary shell command.
///
/// Used for utility windows like `supervisor` where the caller needs full control
/// over what runs in the window (e.g. a `while true` polling loop). If a window
/// with `name` already exists it is replaced (current process killed, fresh
/// command started). Otherwise a new window is created.
///
/// `cmd` is the raw shell command to execute (not escaped further).
pub fn spawn_named_window_raw(
    session: &str,
    name: &str,
    working_dir: &str,
    cmd: &str,
) -> Result<()> {
    // Kill any existing window with this name outright — sending C-c to a
    // `while true` shell loop is unreliable because SIGINT propagates to the
    // entire process group and can leave the shell in an indeterminate state.
    // A fresh window is always clean.
    if let Some(idx) = find_window_index(session, name) {
        let target = format!("{}:{}", session, idx);
        let _ = tmux(&["kill-window", "-t", &target]);
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    let end_target = format!("{}:", session);
    tmux(&[
        "new-window",
        "-d",
        "-t",
        &end_target,
        "-n",
        name,
        "-c",
        working_dir,
    ])?;
    let idx = find_window_index(session, name)
        .with_context(|| format!("Could not find newly created window '{}'", name))?;
    let target = format!("{}:{}", session, idx);
    tmux(&["send-keys", "-t", &target, cmd])?;
    tmux(&["send-keys", "-t", &target, "Enter"])?;

    Ok(())
}

/// Build the opencode launch command string with optional agent and prompt flags.
///
/// Launches the interactive opencode TUI (`opencode --prompt <msg>`) so the
/// agent runs in a visible, human-inspectable session. The `--prompt` flag
/// auto-submits the first message without requiring a keypress.
fn build_opencode_cmd(prompt: &str, agent: Option<&str>) -> String {
    let mut cmd = String::from("opencode");
    if let Some(a) = agent {
        cmd.push_str(&format!(" --agent {}", shell_escape(a)));
    }
    cmd.push_str(&format!(" --prompt {}", shell_escape(prompt)));
    cmd
}

pub fn shell_escape(s: &str) -> String {
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

    #[test]
    fn test_base_window_name_multiple_colons_returns_before_first() {
        // Only the first colon is the phase separator; anything after is part of the phase label.
        assert_eq!(base_window_name("WIS-olive:dev:extra"), "WIS-olive");
    }

    #[test]
    fn test_base_window_name_colon_at_start() {
        // Degenerate: name begins with colon — base is the empty slice.
        assert_eq!(base_window_name(":dev"), "");
    }

    #[test]
    fn test_base_window_name_empty_string() {
        assert_eq!(base_window_name(""), "");
    }

    // ---------------------------------------------------------------------------
    // shell_escape
    // ---------------------------------------------------------------------------

    #[test]
    fn test_shell_escape_plain_string() {
        assert_eq!(shell_escape("hello world"), "'hello world'");
    }

    #[test]
    fn test_shell_escape_empty_string() {
        assert_eq!(shell_escape(""), "''");
    }

    #[test]
    fn test_shell_escape_contains_single_quote() {
        // Single quotes inside the string must be escaped as '\''
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn test_shell_escape_multiple_single_quotes() {
        assert_eq!(shell_escape("a'b'c"), "'a'\\''b'\\''c'");
    }

    #[test]
    fn test_shell_escape_no_mutation_for_double_quotes() {
        // Double quotes are safe inside single-quoted shell strings.
        assert_eq!(shell_escape("say \"hi\""), "'say \"hi\"'");
    }

    #[test]
    fn test_shell_escape_special_chars_preserved() {
        // $, !, backtick etc. are safe inside single-quoted strings.
        let input = "$(rm -rf /) && echo `id`";
        let escaped = shell_escape(input);
        assert!(escaped.starts_with('\''));
        assert!(escaped.ends_with('\''));
        // The dangerous characters must not be expanded — they remain literal.
        assert!(escaped.contains("$(rm -rf /)"));
    }

    // -------------------------------------------------------------------------
    // shell_escape round-trip: verify bash evaluates the escaped string back
    // to the original. Requires /bin/bash to be available.
    // -------------------------------------------------------------------------

    fn bash_eval(expr: &str) -> Option<String> {
        let out = std::process::Command::new("bash")
            .args(["-c", &format!("printf '%s' {}", expr)])
            .output()
            .ok()?;
        if out.status.success() {
            Some(String::from_utf8_lossy(&out.stdout).to_string())
        } else {
            None
        }
    }

    #[test]
    fn test_shell_escape_roundtrip_plain() {
        let input = "hello world";
        let escaped = shell_escape(input);
        assert_eq!(bash_eval(&escaped).unwrap(), input);
    }

    #[test]
    fn test_shell_escape_roundtrip_single_quote() {
        let input = "it's a test";
        let escaped = shell_escape(input);
        assert_eq!(bash_eval(&escaped).unwrap(), input);
    }

    #[test]
    fn test_shell_escape_roundtrip_dollar_and_backtick() {
        let input = "price: $100 and `whoami`";
        let escaped = shell_escape(input);
        assert_eq!(bash_eval(&escaped).unwrap(), input);
    }

    #[test]
    fn test_shell_escape_roundtrip_empty() {
        let input = "";
        let escaped = shell_escape(input);
        assert_eq!(bash_eval(&escaped).unwrap(), input);
    }

    #[test]
    fn test_shell_escape_roundtrip_newlines_and_tabs() {
        let input = "line1\nline2\ttabbed";
        let escaped = shell_escape(input);
        assert_eq!(bash_eval(&escaped).unwrap(), input);
    }

    // -------------------------------------------------------------------------
    // build_opencode_cmd
    // -------------------------------------------------------------------------

    #[test]
    fn test_build_opencode_cmd_no_agent() {
        let cmd = build_opencode_cmd("do something", None);
        assert_eq!(cmd, "opencode --prompt 'do something'");
        assert!(!cmd.contains("--agent"));
    }

    #[test]
    fn test_build_opencode_cmd_with_agent() {
        let cmd = build_opencode_cmd("plan this", Some("plan"));
        assert!(cmd.contains("--agent 'plan'"), "got: {}", cmd);
        assert!(cmd.contains("--prompt 'plan this'"), "got: {}", cmd);
        // agent flag must come before prompt
        let agent_pos = cmd.find("--agent").unwrap();
        let prompt_pos = cmd.find("--prompt").unwrap();
        assert!(agent_pos < prompt_pos);
    }

    #[test]
    fn test_build_opencode_cmd_with_build_agent() {
        let cmd = build_opencode_cmd("implement x", Some("build"));
        assert!(cmd.contains("--agent 'build'"), "got: {}", cmd);
    }
}
