use anyhow::{bail, Context, Result};
use std::process::Command;
use tracing::debug;

/// Settle delay after killing a tmux window before opening a new one in its place.
/// Gives tmux time to process the kill and update its internal window list.
const KILL_WINDOW_SETTLE_MS: u64 = 300;

/// Minimum run of consecutive spaces that marks the start of the tmux sidebar
/// column separator. The first 20 visible characters are skipped before scanning.
const SIDEBAR_MIN_RUN_LEN: usize = 8;

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

/// Return the stable ID of the current tmux window (e.g. `@3`).
///
/// Unlike the window index (`#I`) or name (`#W`), the window ID is assigned
/// once at creation and never changes — it survives renames, moves between
/// sessions, and index renumbering caused by other windows being
/// created/destroyed. Use this to reliably re-select the TUI window even after
/// worktree windows have been renamed.
pub fn current_window_id() -> Result<String> {
    tmux(&["display-message", "-p", "#{window_id}"]).context("Failed to get current tmux window ID")
}

/// Re-select the TUI window by its stable `#{window_id}` (e.g. `@3`).
///
/// Targeting by ID is immune to both name collisions (where a worktree's base
/// name matches the TUI window name and `find_window_index` would return the
/// wrong window) and to index staleness (indices shift whenever windows are
/// created or destroyed).
pub fn select_window_by_id(session: &str, window_id: &str) -> Result<()> {
    // tmux accepts @N IDs directly as window targets: "session:@N"
    let target = format!("{}:{}", session, window_id);
    tmux(&["select-window", "-t", &target]).with_context(|| {
        format!(
            "Failed to re-focus window '{}' (id {}) in session '{}'",
            window_id, window_id, session
        )
    })?;
    Ok(())
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

    let prompt_file = write_prompt_file(prompt)?;
    let opencode_cmd = build_opencode_cmd(&prompt_file, agent);
    let cmd = format!("cd {} && {}", shell_escape(working_dir), opencode_cmd);

    // respawn-pane -k atomically kills the running process and starts the new
    // command — no C-c, no sleeps, no ZLE timing races.
    launch_in_existing_window(&target, working_dir, &cmd)?;

    Ok(())
}

/// Send a prompt to the running opencode TUI in an existing window.
///
/// Finds the window by its base name (ignoring any phase suffix such as `:dev`
/// or `:qa`), then sends the prompt text followed by Enter — exactly as if the
/// user had typed it into the TUI input box.
///
/// Returns an error if no window with the given base name exists.
pub fn send_to_window(session: &str, base_name: &str, prompt: &str) -> Result<()> {
    let idx = find_window_index(session, base_name).with_context(|| {
        format!(
            "No tmux window found for '{}' in session '{}'. Is an opencode session running?",
            base_name, session
        )
    })?;
    let target = format!("{}:{}", session, idx);
    tmux(&["send-keys", "-t", &target, prompt])?;
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
    // Write the prompt to a temp file so we don't send a huge/multi-line string
    // through tmux send-keys (which would mangle it or fire premature Enters).
    let prompt_file = write_prompt_file(prompt)?;

    // Look up by base name so we find it even if it already has a phase suffix.
    if let Some(idx) = find_window_index(session, window_name) {
        let target = format!("{}:{}", session, idx);
        // The window already has opencode running — send the prompt text directly
        // to its TUI input (not as a shell command).
        tmux(&["send-keys", "-t", &target, prompt])?;
        tmux(&["send-keys", "-t", &target, "Enter"])?;
        return Ok(false);
    }

    // New window — always created with the :dev phase suffix.
    let initial_name = format!("{}:dev", window_name);
    let opencode_cmd = build_opencode_cmd(&prompt_file, agent);

    // Pass the command directly to new-window so tmux runs it via /bin/sh
    // immediately — no send-keys ZLE timing races, no startup sleep needed.
    launch_in_new_window(session, &initial_name, working_dir, &opencode_cmd)?;

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
        std::thread::sleep(std::time::Duration::from_millis(KILL_WINDOW_SETTLE_MS));
    }

    // Pass the command directly to new-window so tmux runs it via /bin/sh
    // immediately — no send-keys ZLE timing races.
    launch_in_new_window(session, name, working_dir, cmd)?;
    Ok(())
}

/// Write the prompt to a temporary file and return its path.
///
/// Avoids the character-mangling that occurs when large, multi-line strings
/// are sent verbatim through `tmux send-keys`. The file is named using the
/// current PID so parallel invocations don't clobber each other.
fn write_prompt_file(prompt: &str) -> Result<String> {
    let path = format!("/tmp/task-master-prompt-{}.txt", std::process::id());
    std::fs::write(&path, prompt)
        .with_context(|| format!("Failed to write prompt temp file '{}'", path))?;
    Ok(path)
}

/// Build the opencode launch command string with optional agent and prompt flags.
///
/// `prompt_file` is the path to a temp file containing the prompt text.
/// Using a file avoids sending large/multi-line strings through `tmux send-keys`,
/// which can mangle them or trigger premature Enter presses on embedded newlines.
///
/// The command uses `"$(cat <file>)"` so the shell reads the prompt at startup.
fn build_opencode_cmd(prompt_file: &str, agent: Option<&str>) -> String {
    let mut cmd = String::from("opencode");
    if let Some(a) = agent {
        cmd.push_str(&format!(" --agent {}", shell_escape(a)));
    }
    // Double-quote the command substitution so the prompt value (which may
    // contain spaces, newlines, single-quotes, etc.) is passed as one argument.
    cmd.push_str(&format!(
        " --prompt \"$(cat {})\"",
        shell_escape(prompt_file)
    ));
    cmd
}

pub fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Launch `cmd` in a new background tmux window named `name`, rooted at
/// `working_dir`. The command is passed directly as a positional argument to
/// `tmux new-window`, so tmux runs it via `/bin/sh -c` immediately — no
/// `send-keys` ZLE timing races. `; exec $SHELL` is appended so the window
/// stays open (drops to a shell) when the command exits.
fn launch_in_new_window(session: &str, name: &str, working_dir: &str, cmd: &str) -> Result<()> {
    let shell_cmd = format!("{}; exec $SHELL", cmd);
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
        &shell_cmd,
    ])?;
    Ok(())
}

/// Kill whatever is running in pane `target` and start `cmd` atomically using
/// `tmux respawn-pane -k`. No C-c, no sleeps, no ZLE races. `; exec $SHELL`
/// is appended so the window stays open (drops to a shell) when the command
/// exits.
fn launch_in_existing_window(target: &str, working_dir: &str, cmd: &str) -> Result<()> {
    let shell_cmd = format!("{}; exec $SHELL", cmd);
    tmux(&[
        "respawn-pane",
        "-k",
        "-t",
        target,
        "-c",
        working_dir,
        &shell_cmd,
    ])?;
    Ok(())
}

/// Strip the right-hand sidebar column that opencode renders in its two-column
/// TUI layout. The sidebar is separated from the conversation by a long run of
/// spaces. We find the first run of 8 or more consecutive spaces starting after
/// the 20th *character* (so short lines and leading indentation are left intact)
/// and truncate there, then trim any residual trailing whitespace.
fn strip_sidebar_column(line: &str) -> String {
    // Find the byte offset of the 20th character (or end-of-string if shorter).
    // Using char_indices ensures we never slice mid-codepoint, which would panic
    // when the line contains multi-byte characters (e.g. em-dashes, box-drawing
    // chars) whose bytes straddle the 20-byte mark.
    let search_start = line
        .char_indices()
        .nth(20)
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    let tail = &line[search_start..];

    // Find 8+ consecutive spaces in the tail.
    let mut run_start: Option<usize> = None;
    let mut run_len = 0usize;
    for (i, b) in tail.bytes().enumerate() {
        if b == b' ' {
            if run_start.is_none() {
                run_start = Some(i);
            }
            run_len += 1;
            if run_len >= SIDEBAR_MIN_RUN_LEN {
                let cut = search_start + run_start.unwrap();
                return line[..cut].trim_end().to_string();
            }
        } else {
            run_start = None;
            run_len = 0;
        }
    }

    // No wide gap found — just trim trailing whitespace.
    line.trim_end().to_string()
}

/// Capture the current visible content of the tmux pane for the given worktree.
///
/// Uses `tmux capture-pane -p -e` to preserve ANSI escape sequences (colors,
/// bold, etc.) so they can be parsed and rendered by the ratatui TUI.
///
/// Returns `None` if the window does not exist, tmux is unavailable, or the
/// capture fails for any reason.
pub fn capture_pane(session: &str, base_name: &str) -> Option<Vec<String>> {
    let idx = find_window_index(session, base_name)?;
    let target = format!("{}:{}", session, idx);

    let output = Command::new("tmux")
        .args(["capture-pane", "-p", "-e", "-t", &target])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    // opencode renders a two-column TUI: conversation on the left, a sidebar
    // on the right, separated by a wide run of spaces. Strip the sidebar by
    // truncating at the first run of 8+ spaces found after column 20, then
    // trim residual trailing whitespace. The ANSI sequences between the two
    // columns are plain-space padding (not colored), so byte-level space
    // detection still works correctly on ANSI-decorated lines.
    let mut lines: Vec<String> = text.lines().map(|l| strip_sidebar_column(l)).collect();
    while lines
        .last()
        .map(|l: &String| l.trim().is_empty())
        .unwrap_or(false)
    {
        lines.pop();
    }
    Some(lines)
}

/// Build the awk-based tmux rename-window command used by QA, plan, and e2e
/// agents to update the window's phase suffix.
///
/// The resulting shell fragment, when executed, finds the window whose base
/// name matches `base` and renames it to the caller-supplied suffix — e.g.:
///
/// ```text
/// format!("{} 'WIS-olive:review'", build_rename_cmd(session, "WIS-olive"))
/// ```
pub fn build_rename_cmd(session: &str, base: &str) -> String {
    format!(
        "tmux list-windows -t {session} -F '#{{window_index}} #{{window_name}}' \
         | awk -F'[ :]' '$2==\"{base}\" {{print $1}}' \
         | xargs -I{{}} tmux rename-window -t {session}:{{}}",
        session = session,
        base = base,
    )
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
        // build_opencode_cmd now takes a prompt *file path*, not the raw prompt.
        let cmd = build_opencode_cmd("/tmp/task-master-prompt-1.txt", None);
        assert!(
            cmd.contains("--prompt \"$(cat '/tmp/task-master-prompt-1.txt')\""),
            "got: {cmd}"
        );
        assert!(!cmd.contains("--agent"));
    }

    #[test]
    fn test_build_opencode_cmd_with_agent() {
        let cmd = build_opencode_cmd("/tmp/task-master-prompt-2.txt", Some("plan"));
        assert!(cmd.contains("--agent 'plan'"), "got: {cmd}");
        assert!(
            cmd.contains("--prompt \"$(cat '/tmp/task-master-prompt-2.txt')\""),
            "got: {cmd}"
        );
        // agent flag must come before prompt
        let agent_pos = cmd.find("--agent").unwrap();
        let prompt_pos = cmd.find("--prompt").unwrap();
        assert!(agent_pos < prompt_pos);
    }

    #[test]
    fn test_build_opencode_cmd_with_build_agent() {
        let cmd = build_opencode_cmd("/tmp/task-master-prompt-3.txt", Some("build"));
        assert!(cmd.contains("--agent 'build'"), "got: {cmd}");
    }

    // -------------------------------------------------------------------------
    // strip_sidebar_column — regression tests for UTF-8 char-boundary safety
    // -------------------------------------------------------------------------

    #[test]
    fn test_strip_sidebar_column_ascii_no_gap() {
        // Short ASCII line with no 8-space run — returned as-is (trimmed).
        let result = strip_sidebar_column("hello world   ");
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_strip_sidebar_column_ascii_wide_gap() {
        // ASCII line with 8+ spaces after column 20 — sidebar is stripped.
        let line = "12345678901234567890abcde        sidebar";
        let result = strip_sidebar_column(line);
        // Cut happens at the run of spaces; trailing whitespace trimmed.
        assert_eq!(result, "12345678901234567890abcde");
    }

    #[test]
    fn test_strip_sidebar_column_multibyte_at_boundary_no_panic() {
        // Em-dash is 3 bytes (U+2014, bytes: E2 80 94).
        // Place it so its bytes straddle byte offset 20 (bytes 18,19,20).
        // Before the fix this caused: 'byte index 20 is not a char boundary'.
        // "123456789012345678" = 18 bytes, then "—" = 3 bytes (positions 18-20),
        // then trailing content.
        let line = "123456789012345678\u{2014}trailing content";
        // Must not panic.
        let result = strip_sidebar_column(line);
        assert!(!result.is_empty());
        // The em-dash is character 19 (0-indexed), so search starts *after* char 20.
        // No 8-space run exists, so full line trimmed.
        assert_eq!(result, "123456789012345678\u{2014}trailing content");
    }

    #[test]
    fn test_strip_sidebar_column_multibyte_with_sidebar() {
        // Line with multi-byte chars before column 20, and a sidebar gap after.
        // 10 em-dashes = 10 chars (30 bytes), then spaces, then sidebar.
        let line = "\u{2014}\u{2014}\u{2014}\u{2014}\u{2014}\u{2014}\u{2014}\u{2014}\u{2014}\u{2014}1234567890abcde         sidebar";
        let result = strip_sidebar_column(line);
        // Must not panic and must strip the sidebar.
        assert!(
            !result.contains("sidebar"),
            "sidebar should be stripped, got: {result}"
        );
    }

    #[test]
    fn test_strip_sidebar_column_short_line_no_panic() {
        // Line shorter than 20 chars — search_start clamped to end, no panic.
        let result = strip_sidebar_column("hi");
        assert_eq!(result, "hi");
    }

    #[test]
    fn test_strip_sidebar_column_empty_no_panic() {
        let result = strip_sidebar_column("");
        assert_eq!(result, "");
    }
}
