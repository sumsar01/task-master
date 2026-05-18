//! Git worktree lifecycle management.
//!
//! Handles creating, resetting, and removing git worktrees, as well as the
//! TOML mutations that keep `task-master.toml` in sync. Setup concerns
//! (beads, serena, agent configs, git identity) have been extracted into
//! their own modules.

use crate::agent_configs::install_agent_configs;
use crate::beads::{init_beads_in_repo, write_beads_redirect};
use crate::git_identity::write_git_identity_to_repo;
use crate::hooks;
use crate::registry::{self, Registry};
use crate::serena::{register_in_serena_config, write_serena_project_yml};
use crate::tmux;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use toml_edit::{value, DocumentMut, Item, Table};
use tracing::info;

// Re-export setup helpers used by other modules.
pub use crate::agent_configs::cmd_install_agent_configs;
pub use crate::git_identity::cmd_fix_git_identity;

// ---------------------------------------------------------------------------
// reset_worktree_to_master
// ---------------------------------------------------------------------------

/// Reset a git worktree to the tip of `master` (or `main`) at origin.
///
/// If `force` is `false` and the worktree has uncommitted changes, the
/// function returns an error.  Pass `force = true` to discard them.
pub fn reset_worktree_to_master(path: &Path, force: bool) -> Result<()> {
    let git = |args: &[&str]| -> Result<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .with_context(|| format!("Failed to run: git {}", args.join(" ")))?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };

    let git_ok = |args: &[&str]| -> Result<bool> {
        let status = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .status()
            .with_context(|| format!("Failed to run: git {}", args.join(" ")))?;
        Ok(status.success())
    };

    // 1. Check for uncommitted changes.
    let status_output = git(&["status", "--porcelain"])?;
    if !status_output.is_empty() {
        if !force {
            bail!(
                "Worktree '{}' has uncommitted changes. Clean up first or use --force to discard them.\n{}",
                path.display(),
                status_output
            );
        }
        // Hard reset + clean.
        git_ok(&["checkout", "-f", "HEAD"])?;
        git_ok(&["clean", "-fd"])?;
    }

    // 2. Reset to master (or main) at origin.
    //
    // Strategy A: try `git checkout <branch>` — works in plain clones and when the
    //             worktree is already on that branch (bare repo case where the branch
    //             isn't locked by another worktree).
    // Strategy B: if checkout fails (e.g. bare repo — "branch already used by worktree"),
    //             fall back to `git reset --hard FETCH_HEAD`.
    //
    // We fetch `origin <branch>` explicitly (not just `origin`) so that FETCH_HEAD is
    // written with the correct ref even in linked worktrees that have no configured
    // remote-tracking refspecs.
    let reset_to_master = |branch: &str| -> Result<bool> {
        // Fetch the specific branch so FETCH_HEAD is set correctly (non-fatal).
        let fetched = git_ok(&["fetch", "origin", branch])?;
        if !fetched {
            eprintln!(
                "Warning: git fetch origin {} failed in '{}'; will try local tip.",
                branch,
                path.display()
            );
        }

        // Try direct checkout first.
        if git_ok(&["checkout", branch])? {
            // Bring the branch up to date using FETCH_HEAD.
            if fetched && !git_ok(&["reset", "--hard", "FETCH_HEAD"])? {
                eprintln!(
                    "Warning: git reset --hard FETCH_HEAD failed after checkout {}; using local tip.",
                    branch
                );
            }
            return Ok(true);
        }
        // Checkout failed (e.g. branch locked by another worktree) — hard-reset to
        // FETCH_HEAD so the working tree content matches origin without switching branches.
        if fetched && git_ok(&["reset", "--hard", "FETCH_HEAD"])? {
            return Ok(true);
        }
        Ok(false)
    };

    if !reset_to_master("master")? {
        if !reset_to_master("main")? {
            bail!(
                "Could not reset worktree '{}' to 'master' or 'main'. \
                 Make sure the default branch exists and origin is reachable.",
                path.display()
            );
        }
    }

    // 3. Remove untracked files so the agent starts clean.
    git_ok(&["clean", "-fd"])?;

    Ok(())
}

// ---------------------------------------------------------------------------
// add-worktree
// ---------------------------------------------------------------------------

pub fn cmd_add_worktree(
    registry: &Registry,
    base_dir: &PathBuf,
    project_short: &str,
    worktree_name: &str,
    branch: Option<&str>,
) -> Result<String> {
    let project = registry.find_project(project_short).with_context(|| {
        format!(
            "Project '{}' not found. Run `task-master list` to see available projects.",
            project_short
        )
    })?;

    let window_name = format!("{}-{}", project.short, worktree_name);
    registry.assert_window_name_free(&window_name)?;

    let repo_path = base_dir.join(&project.repo);
    let worktree_path = repo_path.join(worktree_name);

    if worktree_path.exists() {
        bail!("Directory already exists: {}", worktree_path.display());
    }

    // git worktree add <name> [<branch>]
    let mut git_args = vec!["worktree", "add"];

    let worktree_path_str = worktree_path.to_string_lossy().to_string();
    git_args.push(&worktree_path_str);

    let branch_owned;
    if let Some(b) = branch {
        git_args.push("-b");
        branch_owned = b.to_string();
        git_args.push(&branch_owned);
    }

    info!("Running: git -C {} worktree add ...", repo_path.display());
    let output = Command::new("git")
        .arg("-C")
        .arg(&repo_path)
        .args(&git_args)
        .output()
        .context("Failed to run git worktree add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git worktree add failed: {}", stderr.trim());
    }

    // Append to task-master.toml
    let config_path = base_dir.join("task-master.toml");
    let contents = std::fs::read_to_string(&config_path)?;

    let new_toml = append_worktree_to_toml(&contents, project_short, worktree_name, false)
        .context("Failed to update task-master.toml")?;
    std::fs::write(&config_path, new_toml)?;

    info!(
        "Added {}. Spawn with: task-master spawn {} \"<prompt>\"",
        window_name, window_name
    );

    // Apply per-project git identity override (non-fatal).
    match write_git_identity_to_repo(
        &repo_path,
        project.git_name.as_deref(),
        project.git_email.as_deref(),
        project.git_signing_key.as_deref(),
    ) {
        Ok(()) => {}
        Err(e) => eprintln!(
            "Warning: could not set git identity for {}: {}. \
             Run `task-master fix-git-identity` manually later.",
            window_name, e
        ),
    }

    // Auto-install the QA post-push hook.
    match hooks::install_hook_for_single(&worktree_path, project_short) {
        Ok(()) => {}
        Err(e) => {
            eprintln!(
                "Warning: could not install QA hook for {}: {}",
                window_name, e
            );
            eprintln!("Run `task-master install-qa-hooks` manually later.");
        }
    }

    // Set up beads coordination.
    let repo_beads = repo_path.join(".beads");
    if !repo_beads.is_dir() {
        match init_beads_in_repo(&repo_path, project_short) {
            Ok(()) => {}
            Err(e) => eprintln!(
                "Warning: could not run bd init for {}: {}. \
                 Run `bd init --prefix {}` manually in '{}'.",
                window_name,
                e,
                project_short,
                repo_path.display()
            ),
        }
    }
    match write_beads_redirect(&worktree_path) {
        Ok(()) => {}
        Err(e) => eprintln!(
            "Warning: could not write .beads/redirect for {}: {}. \
             Run `bd init` manually in the worktree to share issues.",
            window_name, e
        ),
    }

    // Set up serena project detection.
    match write_serena_project_yml(&worktree_path, worktree_name, &project.language) {
        Ok(()) => {}
        Err(e) => eprintln!(
            "Warning: could not write .serena/project.yml for {}: {}. \
             Create it manually with project_name='{}' and languages: [{}].",
            window_name, e, worktree_name, project.language
        ),
    }
    match register_in_serena_config(&worktree_path) {
        Ok(()) => {}
        Err(e) => eprintln!(
            "Warning: could not register {} in serena_config.yml: {}. \
             Add '- {}' under the projects: key manually.",
            window_name,
            e,
            worktree_path.display()
        ),
    }

    // Install opencode agent configs.
    match install_agent_configs(base_dir, &worktree_path) {
        Ok(()) => {}
        Err(e) => eprintln!(
            "Warning: could not install agent configs for {}: {}. \
             Run `task-master install-agent-configs` manually later.",
            window_name, e
        ),
    }

    Ok(format!(
        "Added {}. Spawn with:\n  task-master spawn {} \"<prompt>\"",
        window_name, window_name
    ))
}

// ---------------------------------------------------------------------------
// create-ephemeral-worktree (internal helper for spawn --ephemeral)
// ---------------------------------------------------------------------------

/// Create a new ephemeral worktree for `project_short`, register it in the config
/// with `ephemeral = true`, and return the resolved `(window_name, abs_path)`.
pub fn create_ephemeral_worktree(
    registry: &Registry,
    base_dir: &PathBuf,
    project_short: &str,
    worktree_name: &str,
    branch_name: &str,
) -> Result<(String, std::path::PathBuf)> {
    let project = registry.find_project(project_short).with_context(|| {
        format!(
            "Project '{}' not found. Run `task-master list` to see available projects.",
            project_short
        )
    })?;

    let window_name = format!("{}-{}", project.short, worktree_name);
    registry.assert_window_name_free(&window_name)?;

    let repo_path = base_dir.join(&project.repo);
    let worktree_path = repo_path.join(worktree_name);

    if worktree_path.exists() {
        bail!("Directory already exists: {}", worktree_path.display());
    }

    // Create the worktree on a new branch.
    let worktree_path_str = worktree_path.to_string_lossy().to_string();
    info!(
        "Running: git -C {} worktree add {} -b {}",
        repo_path.display(),
        worktree_path_str,
        branch_name
    );
    let output = Command::new("git")
        .arg("-C")
        .arg(&repo_path)
        .args(["worktree", "add", &worktree_path_str, "-b", branch_name])
        .output()
        .context("Failed to run git worktree add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git worktree add failed: {}", stderr.trim());
    }

    // Append to task-master.toml with ephemeral = true.
    let config_path = base_dir.join("task-master.toml");
    let contents = std::fs::read_to_string(&config_path)?;
    let new_toml = append_worktree_to_toml(&contents, project_short, worktree_name, true)
        .context("Failed to update task-master.toml")?;
    std::fs::write(&config_path, new_toml)?;

    info!(
        "Created ephemeral worktree {} on branch {}",
        window_name, branch_name
    );

    // Apply per-project git identity override (non-fatal).
    match write_git_identity_to_repo(
        &repo_path,
        project.git_name.as_deref(),
        project.git_email.as_deref(),
        project.git_signing_key.as_deref(),
    ) {
        Ok(()) => {}
        Err(e) => eprintln!(
            "Warning: could not set git identity for {}: {}",
            window_name, e
        ),
    }

    // Install QA post-push hook (non-fatal).
    match hooks::install_hook_for_single(&worktree_path, project_short) {
        Ok(()) => {}
        Err(e) => {
            eprintln!(
                "Warning: could not install QA hook for {}: {}",
                window_name, e
            );
            eprintln!("Run `task-master install-qa-hooks` manually later.");
        }
    }

    // Set up beads redirect (non-fatal).
    let repo_beads = repo_path.join(".beads");
    if !repo_beads.is_dir() {
        match init_beads_in_repo(&repo_path, project_short) {
            Ok(()) => {}
            Err(e) => eprintln!(
                "Warning: could not run bd init for {}: {}. \
                 Run `bd init --prefix {}` manually in '{}'.",
                window_name,
                e,
                project_short,
                repo_path.display()
            ),
        }
    }
    match write_beads_redirect(&worktree_path) {
        Ok(()) => {}
        Err(e) => eprintln!(
            "Warning: could not write .beads/redirect for {}: {}",
            window_name, e
        ),
    }

    // Set up serena project.yml (non-fatal).
    match write_serena_project_yml(&worktree_path, worktree_name, &project.language) {
        Ok(()) => {}
        Err(e) => eprintln!(
            "Warning: could not write .serena/project.yml for {}: {}",
            window_name, e
        ),
    }
    match register_in_serena_config(&worktree_path) {
        Ok(()) => {}
        Err(e) => eprintln!(
            "Warning: could not register {} in serena_config.yml: {}",
            window_name, e
        ),
    }

    // Install opencode agent configs (non-fatal).
    match install_agent_configs(base_dir, &worktree_path) {
        Ok(()) => {}
        Err(e) => eprintln!(
            "Warning: could not install agent configs for {}: {}. \
             Run `task-master install-agent-configs` manually later.",
            window_name, e
        ),
    }

    Ok((window_name, worktree_path))
}

// ---------------------------------------------------------------------------
// remove-worktree
// ---------------------------------------------------------------------------

pub fn cmd_remove_worktree(
    registry: &Registry,
    base_dir: &PathBuf,
    window_name: &str,
    force: bool,
    keep_branch: bool,
) -> Result<()> {
    let worktree = registry.require_worktree(window_name)?;
    let window_base = tmux::base_window_name(window_name);

    // If a tmux window is active for this worktree and --force is not set, refuse.
    if let Ok(session) = tmux::current_session() {
        if tmux::find_window_index(&session, window_base).is_some() && !force {
            bail!(
                "Window '{}' is currently active in tmux. \
                 Stop the agent first, or pass --force to remove anyway.",
                window_base
            );
        }
    }

    let project = registry
        .find_project(&worktree.project_short)
        .with_context(|| format!("Project '{}' not found", worktree.project_short))?;
    let repo_path = base_dir.join(&project.repo);

    // Determine the current branch BEFORE removing the worktree.
    let branch = if !keep_branch && worktree.abs_path.exists() {
        let out = Command::new("git")
            .arg("-C")
            .arg(&worktree.abs_path)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output();
        match out {
            Ok(o) if o.status.success() => {
                let b = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if b == "master" || b == "main" || b == "HEAD" {
                    None
                } else {
                    Some(b)
                }
            }
            _ => None,
        }
    } else {
        None
    };

    // Run `git worktree remove [--force] <path>` from the bare repo root.
    let mut git_args = vec!["worktree", "remove"];
    if force {
        git_args.push("--force");
    }
    let abs_path_str = worktree.abs_path.to_string_lossy().to_string();
    git_args.push(&abs_path_str);

    info!(
        "Running: git -C {} worktree remove {}",
        repo_path.display(),
        abs_path_str
    );
    let git_output = Command::new("git")
        .arg("-C")
        .arg(&repo_path)
        .args(&git_args)
        .output()
        .context("Failed to run git worktree remove")?;

    if !git_output.status.success() {
        let stderr = String::from_utf8_lossy(&git_output.stderr)
            .trim()
            .to_string();
        bail!("git worktree remove failed: {}", stderr);
    }

    // Delete the remote branch (non-fatal).
    if let Some(ref b) = branch {
        let push_out = Command::new("git")
            .arg("-C")
            .arg(&repo_path)
            .args(["push", "origin", "--delete", b])
            .output();
        match push_out {
            Ok(o) if o.status.success() => {
                info!("Deleted remote branch '{}'.", b);
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                info!(
                    "Remote branch delete for '{}' failed (may already be gone): {}",
                    b,
                    stderr.trim()
                );
            }
            Err(e) => {
                info!("Could not run git push origin --delete '{}': {}", b, e);
            }
        }

        // Delete the local branch from the bare repo (non-fatal).
        let _ = Command::new("git")
            .arg("-C")
            .arg(&repo_path)
            .args(["branch", "-d", b])
            .output();
    }

    // Remove the entry from task-master.toml.
    let config_path = base_dir.join("task-master.toml");
    let contents =
        std::fs::read_to_string(&config_path).context("Failed to read task-master.toml")?;
    let new_toml = registry::remove_worktree_from_toml(
        &contents,
        &worktree.project_short,
        &worktree
            .window_name
            .trim_start_matches(&format!("{}-", worktree.project_short)),
    )
    .context("Failed to update task-master.toml")?;
    std::fs::write(&config_path, new_toml)?;

    println!("Removed worktree '{}'.", window_base);
    Ok(())
}

// ---------------------------------------------------------------------------
// TOML mutation helper (extracted for testability)
// ---------------------------------------------------------------------------

/// Append a new `[[projects.worktrees]]` entry to the TOML document string.
///
/// Finds the `[[projects]]` block whose `short` key matches `project_short`
/// (case-insensitive) and pushes a new worktree entry with the given name.
/// Returns the updated TOML as a `String`.
pub fn append_worktree_to_toml(
    toml_str: &str,
    project_short: &str,
    worktree_name: &str,
    ephemeral: bool,
) -> Result<String> {
    let mut doc = toml_str
        .parse::<DocumentMut>()
        .context("Failed to parse task-master.toml")?;

    let projects = doc["projects"]
        .as_array_of_tables_mut()
        .context("Missing [[projects]] in task-master.toml")?;

    let proj_entry = projects
        .iter_mut()
        .find(|p| {
            p.get("short")
                .and_then(|v| v.as_str())
                .map(|s| s.eq_ignore_ascii_case(project_short))
                .unwrap_or(false)
        })
        .with_context(|| format!("Project '{}' not found in config file", project_short))?;

    let worktrees = proj_entry
        .entry("worktrees")
        .or_insert(Item::ArrayOfTables(toml_edit::ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .context("worktrees is not an array of tables")?;

    let mut new_wt = Table::new();
    new_wt.insert("name", value(worktree_name));
    if ephemeral {
        new_wt.insert("ephemeral", value(true));
    }
    worktrees.push(new_wt);

    Ok(doc.to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const BASE_TOML: &str = r#"[[projects]]
name = "warehouse-integration-service"
short = "WIS"
repo = "projects/warehouse-integration-service"

[[projects.worktrees]]
name = "olive"
"#;

    #[test]
    fn test_append_worktree_adds_new_entry() {
        let result = append_worktree_to_toml(BASE_TOML, "WIS", "cedar", false).unwrap();
        assert!(result.contains("cedar"), "expected 'cedar' in:\n{}", result);
        assert!(
            result.contains("olive"),
            "expected 'olive' still in:\n{}",
            result
        );
    }

    #[test]
    fn test_append_worktree_is_valid_toml() {
        let result = append_worktree_to_toml(BASE_TOML, "WIS", "cedar", false).unwrap();
        let reg =
            registry::Registry::load_from_str(&result, std::path::PathBuf::from("/base")).unwrap();
        let names: Vec<&str> = reg
            .worktrees
            .iter()
            .map(|w| w.window_name.as_str())
            .collect();
        assert!(names.contains(&"WIS-olive"));
        assert!(names.contains(&"WIS-cedar"));
    }

    #[test]
    fn test_append_worktree_case_insensitive_project_match() {
        let result = append_worktree_to_toml(BASE_TOML, "wis", "birch", false).unwrap();
        assert!(result.contains("birch"));
    }

    #[test]
    fn test_append_worktree_unknown_project_returns_error() {
        let err = append_worktree_to_toml(BASE_TOML, "XYZ", "branch", false).unwrap_err();
        assert!(err.to_string().contains("XYZ"));
    }

    #[test]
    fn test_append_worktree_to_project_with_no_prior_worktrees() {
        let toml = r#"[[projects]]
name = "fresh-service"
short = "FS"
repo = "projects/fresh-service"
"#;
        let result = append_worktree_to_toml(toml, "FS", "main", false).unwrap();
        assert!(result.contains("main"));
        let reg =
            registry::Registry::load_from_str(&result, std::path::PathBuf::from("/base")).unwrap();
        assert_eq!(reg.worktrees.len(), 1);
        assert_eq!(reg.worktrees[0].window_name, "FS-main");
    }

    #[test]
    fn test_append_worktree_multiple_projects_correct_one_modified() {
        let toml = r#"[[projects]]
name = "alpha"
short = "A"
repo = "projects/alpha"

[[projects.worktrees]]
name = "existing"

[[projects]]
name = "beta"
short = "B"
repo = "projects/beta"
"#;
        let result = append_worktree_to_toml(toml, "B", "new-branch", false).unwrap();
        let reg =
            registry::Registry::load_from_str(&result, std::path::PathBuf::from("/base")).unwrap();
        let names: Vec<&str> = reg
            .worktrees
            .iter()
            .map(|w| w.window_name.as_str())
            .collect();
        assert!(
            names.contains(&"A-existing"),
            "A-existing should be untouched"
        );
        assert!(
            names.contains(&"B-new-branch"),
            "B-new-branch should be added"
        );
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn test_append_worktree_preserves_original_formatting() {
        let toml = "# top comment\n".to_string() + BASE_TOML;
        let result = append_worktree_to_toml(&toml, "WIS", "branch", false).unwrap();
        assert!(result.starts_with("# top comment\n"), "comment was lost");
    }

    #[test]
    fn test_append_worktree_ephemeral_true_writes_flag() {
        let result = append_worktree_to_toml(BASE_TOML, "WIS", "spruce-7f3a", true).unwrap();
        assert!(
            result.contains("spruce-7f3a"),
            "worktree name should appear in output"
        );
        assert!(
            result.contains("ephemeral = true"),
            "ephemeral = true should be written:\n{}",
            result
        );
        let reg =
            registry::Registry::load_from_str(&result, std::path::PathBuf::from("/base")).unwrap();
        let wt = reg.find_worktree("WIS-spruce-7f3a").unwrap();
        assert!(wt.ephemeral, "ephemeral flag should round-trip as true");
    }

    #[test]
    fn test_append_worktree_ephemeral_false_omits_flag() {
        let result = append_worktree_to_toml(BASE_TOML, "WIS", "cedar", false).unwrap();
        assert!(
            !result.contains("ephemeral = true"),
            "ephemeral = true should not appear for non-ephemeral worktree:\n{}",
            result
        );
    }

    // -------------------------------------------------------------------------
    // reset_worktree_to_master
    // -------------------------------------------------------------------------

    fn make_git_worktree(root: &std::path::Path) -> std::path::PathBuf {
        let bare = root.join("bare.git");
        let wt = root.join("wt");

        Command::new("git")
            .args(["init", "--bare"])
            .arg(&bare)
            .status()
            .unwrap();

        let checkout = root.join("checkout");
        Command::new("git")
            .args(["clone"])
            .arg(&bare)
            .arg(&checkout)
            .status()
            .unwrap();

        Command::new("git")
            .args(["-C"])
            .arg(&checkout)
            .args(["config", "user.email", "test@test.com"])
            .status()
            .unwrap();
        Command::new("git")
            .args(["-C"])
            .arg(&checkout)
            .args(["config", "user.name", "Test"])
            .status()
            .unwrap();

        std::fs::write(checkout.join("init.txt"), "init").unwrap();
        Command::new("git")
            .args(["-C"])
            .arg(&checkout)
            .args(["add", "."])
            .status()
            .unwrap();
        Command::new("git")
            .args(["-C"])
            .arg(&checkout)
            .args(["commit", "-m", "init"])
            .status()
            .unwrap();
        Command::new("git")
            .args(["-C"])
            .arg(&checkout)
            .args(["push", "origin", "HEAD:master"])
            .status()
            .unwrap();

        Command::new("git")
            .args(["-C"])
            .arg(&bare)
            .args(["worktree", "add"])
            .arg(&wt)
            .arg("master")
            .status()
            .unwrap();

        Command::new("git")
            .args(["-C"])
            .arg(&wt)
            .args(["config", "user.email", "test@test.com"])
            .status()
            .unwrap();
        Command::new("git")
            .args(["-C"])
            .arg(&wt)
            .args(["config", "user.name", "Test"])
            .status()
            .unwrap();

        wt
    }

    #[test]
    fn test_reset_clean_already_on_master() {
        let root = tempfile::tempdir().expect("tempdir");
        let wt = make_git_worktree(root.path());
        let result = reset_worktree_to_master(&wt, false);
        assert!(
            result.is_ok(),
            "clean worktree should reset ok: {:?}",
            result
        );
    }

    #[test]
    fn test_reset_dirty_no_force_returns_error() {
        let root = tempfile::tempdir().expect("tempdir");
        let wt = make_git_worktree(root.path());
        std::fs::write(wt.join("dirty.txt"), "dirty").unwrap();
        let err = reset_worktree_to_master(&wt, false).unwrap_err();
        assert!(
            err.to_string().contains("uncommitted changes"),
            "expected 'uncommitted changes' in error, got: {}",
            err
        );
    }

    #[test]
    fn test_reset_dirty_with_force_discards_changes() {
        let root = tempfile::tempdir().expect("tempdir");
        let wt = make_git_worktree(root.path());
        let dirty = wt.join("dirty.txt");
        std::fs::write(&dirty, "dirty").unwrap();
        let result = reset_worktree_to_master(&wt, true);
        assert!(result.is_ok(), "force should succeed: {:?}", result);
        assert!(
            !dirty.exists(),
            "untracked file should have been cleaned up"
        );
    }

    #[test]
    fn test_reset_pull_fails_warns_and_continues() {
        let root = tempfile::tempdir().expect("tempdir");

        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&repo)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&repo)
            .status()
            .unwrap();
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&repo)
            .status()
            .unwrap();

        let result = reset_worktree_to_master(&repo, false);
        assert!(
            result.is_ok(),
            "no-remote pull failure should warn+continue: {:?}",
            result
        );
    }
}
