use crate::registry::Registry;
use crate::tmux;
use anyhow::Result;

/// Print a status table of all registered worktrees, showing their live tmux
/// phase when available.
///
/// Format (aligned columns):
/// ```
/// WORKTREE             PHASE      PATH
/// WIS-olive            dev        projects/warehouse-integration-service/olive
/// WIS-cedar            idle       projects/warehouse-integration-service/cedar
/// OTH-main             ?          projects/other-service/main
/// ```
///
/// Phase values:
/// - "idle"  — worktree registered but no tmux window found
/// - "?"     — not running inside tmux (or tmux query failed)
/// - <phase> — the active phase suffix (dev, qa, review, blocked, etc.)
pub fn cmd_status(registry: &Registry) -> Result<()> {
    // Try to get the current tmux session. If we're not in tmux, all phases
    // will show "?" instead of erroring out.
    let session_opt = tmux::current_session().ok();

    // Collect rows: (window_name, phase, rel_path)
    let rows: Vec<(String, String, String)> = registry
        .worktrees
        .iter()
        .map(|wt| {
            let phase = match &session_opt {
                None => "?".to_string(),
                Some(session) => {
                    // Query live tmux for the window. find_window_index returns
                    // the index if the window exists (under any phase suffix).
                    // We need the actual current phase, so list windows again.
                    find_live_phase(session, &wt.window_name).unwrap_or_else(|| "idle".to_string())
                }
            };
            (wt.window_name.clone(), phase, wt.rel_path.clone())
        })
        .collect();

    if rows.is_empty() {
        println!("No worktrees registered. Add one with `task-master add-worktree`.");
        return Ok(());
    }

    // Calculate column widths.
    let w_name = rows
        .iter()
        .map(|(n, _, _)| n.len())
        .max()
        .unwrap_or(8)
        .max("WORKTREE".len());
    let w_phase = rows
        .iter()
        .map(|(_, p, _)| p.len())
        .max()
        .unwrap_or(4)
        .max("PHASE".len());

    println!(
        "{:<w_name$}  {:<w_phase$}  PATH",
        "WORKTREE",
        "PHASE",
        w_name = w_name,
        w_phase = w_phase,
    );
    println!(
        "{:-<w_name$}  {:-<w_phase$}  ----",
        "",
        "",
        w_name = w_name,
        w_phase = w_phase,
    );
    for (name, phase, path) in &rows {
        println!(
            "{:<w_name$}  {:<w_phase$}  {}",
            name,
            phase,
            path,
            w_name = w_name,
            w_phase = w_phase,
        );
    }

    Ok(())
}

/// Query tmux for the current phase of a window whose base name is `base_name`.
///
/// Returns `Some(phase)` if a window is found (where phase may be an empty
/// string if the window has no phase suffix), or `None` if no window matches.
pub fn find_live_phase(session: &str, base_name: &str) -> Option<String> {
    use std::process::Command;

    let output = Command::new("tmux")
        .args(["list-windows", "-t", session, "-F", "#{window_name}"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let full_name = line.trim();
        let base = tmux::base_window_name(full_name);
        if base == base_name {
            // Extract the phase: everything after the first ':'.
            let phase = full_name
                .find(':')
                .map(|i| full_name[i + 1..].to_string())
                .unwrap_or_default();
            // Return "idle" for no phase suffix.
            return Some(if phase.is_empty() {
                "idle".to_string()
            } else {
                phase
            });
        }
    }

    None // window not found — worktree has no tmux window
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_live_phase_parses_phase() {
        // We can't call tmux in unit tests; instead test the phase-extraction logic
        // extracted inline here to verify the string manipulation.
        fn extract_phase(full: &str, base: &str) -> Option<String> {
            let b = tmux::base_window_name(full);
            if b != base {
                return None;
            }
            let phase = full
                .find(':')
                .map(|i| full[i + 1..].to_string())
                .unwrap_or_default();
            Some(if phase.is_empty() {
                "idle".to_string()
            } else {
                phase
            })
        }

        assert_eq!(
            extract_phase("WIS-olive:dev", "WIS-olive"),
            Some("dev".to_string())
        );
        assert_eq!(
            extract_phase("WIS-olive:qa", "WIS-olive"),
            Some("qa".to_string())
        );
        assert_eq!(
            extract_phase("WIS-olive:review", "WIS-olive"),
            Some("review".to_string())
        );
        // No phase suffix → "idle"
        assert_eq!(
            extract_phase("WIS-olive", "WIS-olive"),
            Some("idle".to_string())
        );
        // Different base name → None
        assert_eq!(extract_phase("WIS-cedar:dev", "WIS-olive"), None);
    }
}
