use crate::registry::Registry;
use anyhow::{Context, Result};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use tracing::info;

/// The post-push hook script template.
///
/// Placeholders:
///   {bin}          – absolute path to the task-master binary
///   {worktree}     – e.g. "WIS-olive"
///
/// The generated script intentionally contains no references to "task-master"
/// so that it does not reveal tooling details to anyone who inspects the hook.
const HOOK_TEMPLATE: &str = r#"#!/usr/bin/env bash
set -euo pipefail

_BIN="{bin}"
_WT="{worktree}"

while read -r _lref _lsha _rref _rsha; do
    [ "$_lsha" = "0000000000000000000000000000000000000000" ] && continue
    _branch="${{_rref#refs/heads/}}"
    [ "$_branch" = "$_rref" ] && continue
    command -v gh &>/dev/null || continue
    _pr=$(gh pr view "$_branch" --json number --jq '.number' 2>/dev/null || true)
    [ -z "$_pr" ] && continue
    nohup "$_BIN" qa "$_WT" "$_pr" </dev/null >"/tmp/.qa-hook-${_pr}.log" 2>&1 &
done
"#;

/// Install the post-push QA hook into the git directory of the given worktree path.
fn install_hook_for_worktree(
    worktree_path: &PathBuf,
    worktree_name: &str,
    task_master_bin: &str,
) -> Result<()> {
    // For a git worktree the hooks live in the *main* repo's .git/hooks, not
    // in the worktree's .git file. But for bare repos used as worktree bases the
    // hooks dir is directly inside the bare repo. We handle both cases by
    // asking git where the common dir is.
    let common_dir = git_common_dir(worktree_path)?;
    let hooks_dir = PathBuf::from(&common_dir).join("hooks");

    fs::create_dir_all(&hooks_dir)
        .with_context(|| format!("Failed to create hooks dir: {}", hooks_dir.display()))?;

    let hook_path = hooks_dir.join("post-push");

    let script = HOOK_TEMPLATE
        .replace("{bin}", task_master_bin)
        .replace("{worktree}", worktree_name);

    fs::write(&hook_path, &script)
        .with_context(|| format!("Failed to write hook: {}", hook_path.display()))?;

    // Make executable: rwxr-xr-x
    fs::set_permissions(&hook_path, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("Failed to chmod hook: {}", hook_path.display()))?;

    info!(
        "[{}] Installed post-push hook at {}",
        worktree_name,
        hook_path.display()
    );
    println!(
        "  [{}] post-push hook -> {}",
        worktree_name,
        hook_path.display()
    );

    Ok(())
}

fn git_common_dir(worktree_path: &PathBuf) -> Result<String> {
    let output = std::process::Command::new("git")
        .args([
            "-C",
            &worktree_path.to_string_lossy(),
            "rev-parse",
            "--git-common-dir",
        ])
        .output()
        .context("Failed to run git rev-parse --git-common-dir")?;

    if !output.status.success() {
        anyhow::bail!(
            "git rev-parse --git-common-dir failed in {}",
            worktree_path.display()
        );
    }

    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // The output may be relative to the worktree dir; canonicalize it.
    let path = if std::path::Path::new(&raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        worktree_path.join(&raw)
    };

    path.canonicalize()
        .map(|p| p.to_string_lossy().to_string())
        .with_context(|| format!("Failed to canonicalize git common dir: {}", path.display()))
}

/// Resolve the path to the currently running task-master binary.
fn current_binary() -> Result<String> {
    std::env::current_exe()
        .context("Failed to determine current executable path")?
        .canonicalize()
        .context("Failed to canonicalize executable path")
        .map(|p| p.to_string_lossy().to_string())
}

/// Install QA post-push hooks into every registered worktree.
pub fn cmd_install_qa_hooks(registry: &Registry) -> Result<()> {
    let bin = current_binary()?;

    println!("Installing post-push hooks (binary: {}):", bin);

    let mut count = 0;
    for wt in &registry.worktrees {
        if !wt.abs_path.exists() {
            println!(
                "  [{}] skipped (worktree directory does not exist: {})",
                wt.window_name,
                wt.abs_path.display()
            );
            continue;
        }
        install_hook_for_worktree(&wt.abs_path, &wt.window_name, &bin)?;
        count += 1;
    }

    println!("\nDone. Installed hooks in {} worktree(s).", count);

    Ok(())
}

/// Install QA post-push hook for a single worktree (called from add-worktree).
pub fn install_hook_for_single(worktree_path: &PathBuf, worktree_name: &str) -> Result<()> {
    let bin = current_binary()?;
    install_hook_for_worktree(worktree_path, worktree_name, &bin)
}
