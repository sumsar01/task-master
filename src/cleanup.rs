use crate::registry::{self, Registry};
use crate::tmux;
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Command;
use tracing::info;

// ---------------------------------------------------------------------------
// cmd_cleanup
// ---------------------------------------------------------------------------

/// Remove ephemeral worktrees whose branch has been merged (or PR closed).
///
/// - `merged`: only remove worktrees whose branch is merged / PR is MERGED or CLOSED.
/// - `all`:    remove all ephemeral worktrees regardless of merge status.
/// - `force`:  skip the confirmation prompt when `all` is true.
///
/// When called non-interactively (e.g. from the supervisor), pass `force = true`.
pub fn cmd_cleanup(
    registry: &Registry,
    base_dir: &PathBuf,
    merged: bool,
    all: bool,
    force: bool,
) -> Result<()> {
    if !merged && !all {
        // Neither flag given — default to --merged behaviour.
        return cmd_cleanup(registry, base_dir, true, false, force);
    }

    // Collect all ephemeral worktrees.
    let ephemeral: Vec<_> = registry.worktrees.iter().filter(|w| w.ephemeral).collect();

    if ephemeral.is_empty() {
        println!("No ephemeral worktrees registered.");
        return Ok(());
    }

    // Determine which ones are candidates for removal.
    let mut candidates: Vec<(&crate::registry::Worktree, String)> = Vec::new();

    for wt in &ephemeral {
        if !wt.abs_path.exists() {
            // Directory is already gone — always a candidate (just clean up TOML).
            candidates.push((wt, "(directory already removed)".to_string()));
            continue;
        }

        if all {
            // All ephemerals are candidates regardless of merge status.
            let branch = current_branch(&wt.abs_path).unwrap_or_else(|_| "unknown".to_string());
            candidates.push((wt, branch));
        } else {
            // Only include if merged or PR closed.
            let branch = match current_branch(&wt.abs_path) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("  [{}] could not determine branch: {}", wt.window_name, e);
                    continue;
                }
            };

            match is_branch_merged_or_closed(registry, wt, &branch) {
                Ok(true) => candidates.push((wt, branch)),
                Ok(false) => {
                    info!(
                        "[{}] branch '{}' is not yet merged — skipping",
                        wt.window_name, branch
                    );
                }
                Err(e) => {
                    eprintln!(
                        "  [{}] could not check merge status for '{}': {} — skipping",
                        wt.window_name, branch, e
                    );
                }
            }
        }
    }

    if candidates.is_empty() {
        println!("No ephemeral worktrees to remove (none merged or all flag not set).");
        return Ok(());
    }

    // When --all and not --force, confirm with the user.
    if all && !force {
        println!("The following ephemeral worktrees will be removed:");
        for (wt, branch) in &candidates {
            println!("  {} (branch: {})", wt.window_name, branch);
        }
        print!("Continue? [y/N] ");
        use std::io::Write;
        std::io::stdout().flush().ok();
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).ok();
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }

    let mut removed = 0usize;

    for (wt, branch) in &candidates {
        if let Err(e) = remove_single(registry, base_dir, wt, branch) {
            eprintln!("  [{}] removal failed: {} — continuing.", wt.window_name, e);
        } else {
            removed += 1;
        }
    }

    println!("Removed {} ephemeral worktree(s).", removed);
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Remove a single ephemeral worktree: kill tmux window, delete remote branch,
/// run git worktree remove, update TOML.
fn remove_single(
    registry: &Registry,
    base_dir: &PathBuf,
    wt: &crate::registry::Worktree,
    branch: &str,
) -> Result<()> {
    let window_base = tmux::base_window_name(&wt.window_name);

    // Kill the tmux window if it exists (non-fatal).
    if let Ok(session) = tmux::current_session() {
        if let Some(idx) = tmux::find_window_index(&session, window_base) {
            let target = format!("{}:{}", session, idx);
            let _ = Command::new("tmux")
                .args(["kill-window", "-t", &target])
                .status();
            info!("[{}] Killed tmux window.", wt.window_name);
        }
    }

    // Delete the remote branch (non-fatal).
    let project = registry
        .find_project(&wt.project_short)
        .with_context(|| format!("Project '{}' not found", wt.project_short))?;
    let repo_path = base_dir.join(&project.repo);

    if branch != "unknown" && branch != "(directory already removed)" {
        let push_out = Command::new("git")
            .arg("-C")
            .arg(&repo_path)
            .args(["push", "origin", "--delete", branch])
            .output();
        match push_out {
            Ok(o) if o.status.success() => {
                info!("[{}] Deleted remote branch '{}'.", wt.window_name, branch);
            }
            Ok(o) => {
                // Non-fatal — branch may already be deleted on remote.
                let stderr = String::from_utf8_lossy(&o.stderr);
                info!(
                    "[{}] Remote branch delete for '{}' failed (may already be gone): {}",
                    wt.window_name,
                    branch,
                    stderr.trim()
                );
            }
            Err(e) => {
                info!(
                    "[{}] Could not run git push origin --delete '{}': {}",
                    wt.window_name, branch, e
                );
            }
        }
    }

    // Remove the git worktree (only if directory still exists).
    if wt.abs_path.exists() {
        let abs_path_str = wt.abs_path.to_string_lossy().to_string();
        let git_out = Command::new("git")
            .arg("-C")
            .arg(&repo_path)
            .args(["worktree", "remove", "--force", &abs_path_str])
            .output()
            .context("Failed to run git worktree remove")?;

        if !git_out.status.success() {
            let stderr = String::from_utf8_lossy(&git_out.stderr);
            anyhow::bail!("git worktree remove failed: {}", stderr.trim());
        }
    }

    // Remove from task-master.toml.
    let config_path = base_dir.join("task-master.toml");
    let contents =
        std::fs::read_to_string(&config_path).context("Failed to read task-master.toml")?;
    let leaf = wt
        .window_name
        .trim_start_matches(&format!("{}-", wt.project_short));
    let new_toml = registry::remove_worktree_from_toml(&contents, &wt.project_short, leaf)
        .context("Failed to update task-master.toml")?;
    std::fs::write(&config_path, new_toml)?;

    println!(
        "Removed ephemeral worktree '{}' (branch: {}).",
        wt.window_name, branch
    );
    Ok(())
}

/// Get the current branch name for a worktree.
fn current_branch(abs_path: &std::path::Path) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(abs_path)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .context("Failed to run git rev-parse")?;
    if !out.status.success() {
        anyhow::bail!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Check whether a branch is merged or its PR is closed/merged.
///
/// Strategy:
/// 1. Try `gh pr view <branch>` — if state is MERGED or CLOSED → true.
/// 2. Fall back to `git branch --merged master/main` — if branch appears → true.
fn is_branch_merged_or_closed(
    registry: &Registry,
    wt: &crate::registry::Worktree,
    branch: &str,
) -> Result<bool> {
    // Strategy 1: gh CLI check (most accurate for PR-based workflows).
    let gh_out = Command::new("gh")
        .args(["pr", "view", branch, "--json", "state", "--jq", ".state"])
        .current_dir(&wt.abs_path)
        .output();

    if let Ok(out) = gh_out {
        if out.status.success() {
            let state = String::from_utf8_lossy(&out.stdout).trim().to_uppercase();
            if state == "MERGED" || state == "CLOSED" {
                return Ok(true);
            }
            if state == "OPEN" {
                return Ok(false);
            }
            // Empty output or unexpected state — fall through to git check.
        }
    }

    // Strategy 2: git branch --merged check.
    let project = registry
        .find_project(&wt.project_short)
        .with_context(|| format!("Project '{}' not found", wt.project_short))?;
    let repo_path = registry.base_dir.join(&project.repo);

    for default_branch in &["master", "main"] {
        let git_out = Command::new("git")
            .arg("-C")
            .arg(&repo_path)
            .args(["branch", "--merged", default_branch])
            .output();
        if let Ok(out) = git_out {
            if out.status.success() {
                let branches = String::from_utf8_lossy(&out.stdout);
                for line in branches.lines() {
                    // `git branch --merged` output has leading "* " or "  " — strip them.
                    let trimmed = line.trim().trim_start_matches('*').trim();
                    if trimmed == branch {
                        return Ok(true);
                    }
                }
            }
        }
    }

    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registry;
    use std::path::PathBuf;

    fn registry_from_toml(toml: &str) -> Registry {
        Registry::load_from_str(toml, PathBuf::from("/fake/base")).unwrap()
    }

    #[test]
    fn test_cmd_cleanup_no_ephemeral_worktrees_prints_message() {
        let toml = r#"
[[projects]]
name = "my-service"
short = "SVC"
repo = "projects/my-service"

[[projects.worktrees]]
name = "main"
"#;
        let reg = registry_from_toml(toml);
        // Should succeed and print "No ephemeral worktrees registered."
        // We can't easily capture stdout in unit tests but we can verify it returns Ok.
        let base_dir = PathBuf::from("/fake/base");
        let result = cmd_cleanup(&reg, &base_dir, true, false, false);
        assert!(
            result.is_ok(),
            "cleanup with no ephemerals should return Ok"
        );
    }

    #[test]
    fn test_current_branch_invalid_path_returns_error() {
        let path = PathBuf::from("/nonexistent/path/that/does/not/exist");
        let result = current_branch(&path);
        assert!(result.is_err(), "invalid path should return error");
    }
}
