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

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercise the HOOK_TEMPLATE placeholder substitution without hitting the filesystem.
    fn render_hook(bin: &str, worktree: &str) -> String {
        HOOK_TEMPLATE
            .replace("{bin}", bin)
            .replace("{worktree}", worktree)
    }

    #[test]
    fn test_hook_template_substitutes_bin_and_worktree() {
        let script = render_hook("/usr/local/bin/task-master", "WIS-olive");
        assert!(script.contains(r#"_BIN="/usr/local/bin/task-master""#));
        assert!(script.contains(r#"_WT="WIS-olive""#));
    }

    #[test]
    fn test_hook_template_no_leftover_placeholders() {
        let script = render_hook("/path/to/bin", "PROJ-branch");
        assert!(!script.contains("{bin}"));
        assert!(!script.contains("{worktree}"));
    }

    #[test]
    fn test_hook_template_is_bash_shebang() {
        let script = render_hook("/bin/tm", "X-y");
        assert!(script.starts_with("#!/usr/bin/env bash"));
    }

    #[test]
    fn test_hook_template_uses_nohup_for_background_spawn() {
        let script = render_hook("/bin/tm", "X-y");
        // The QA agent must be launched in the background so the push is not blocked.
        assert!(script.contains("nohup"));
        assert!(script.contains("&"));
    }

    #[test]
    fn test_hook_template_skips_delete_pushes() {
        // A push where lsha is all-zeros is a branch deletion; the hook must skip it.
        let script = render_hook("/bin/tm", "X-y");
        assert!(script.contains("0000000000000000000000000000000000000000"));
        assert!(script.contains("continue"));
    }

    #[test]
    fn test_hook_template_calls_gh_pr_view() {
        let script = render_hook("/bin/tm", "X-y");
        assert!(script.contains("gh pr view"));
    }

    #[test]
    fn test_hook_template_logs_to_tmp() {
        let script = render_hook("/bin/tm", "X-y");
        assert!(script.contains("/tmp/.qa-hook-"));
    }

    #[test]
    fn test_hook_template_bin_path_with_spaces_is_quoted() {
        // Paths with spaces must be inside double-quotes in the generated script.
        let script = render_hook("/home/my user/bin/task-master", "WIS-olive");
        // The _BIN variable is set with double quotes around the value.
        assert!(script.contains(r#"_BIN="/home/my user/bin/task-master""#));
    }

    // -------------------------------------------------------------------------
    // install_hook_for_worktree — requires a real git repo on disk
    // -------------------------------------------------------------------------

    /// Initialise a bare git repo in a tempdir and return the path.
    fn make_bare_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let status = std::process::Command::new("git")
            .args(["init", "--bare"])
            .arg(dir.path())
            .status()
            .expect("git init --bare");
        assert!(status.success(), "git init --bare failed");
        dir
    }

    #[test]
    fn test_install_hook_writes_script_to_correct_path() {
        let repo = make_bare_repo();

        install_hook_for_worktree(
            &repo.path().to_path_buf(),
            "WIS-olive",
            "/usr/local/bin/task-master",
        )
        .expect("install_hook_for_worktree");

        let hook_path = repo.path().join("hooks/post-push");
        assert!(
            hook_path.exists(),
            "hook file should exist at {:?}",
            hook_path
        );

        let content = std::fs::read_to_string(&hook_path).unwrap();
        assert!(content.contains(r#"_BIN="/usr/local/bin/task-master""#));
        assert!(content.contains(r#"_WT="WIS-olive""#));
        assert!(content.starts_with("#!/usr/bin/env bash"));
    }

    #[test]
    fn test_install_hook_is_executable() {
        use std::os::unix::fs::PermissionsExt;

        let repo = make_bare_repo();
        install_hook_for_worktree(&repo.path().to_path_buf(), "WIS-olive", "/bin/task-master")
            .expect("install_hook_for_worktree");

        let hook_path = repo.path().join("hooks/post-push");
        let meta = std::fs::metadata(&hook_path).unwrap();
        let mode = meta.permissions().mode();
        // owner, group, other execute bits must all be set (0o111)
        assert_eq!(
            mode & 0o111,
            0o111,
            "hook should be executable, mode={:o}",
            mode
        );
    }

    #[test]
    fn test_install_hook_overwrites_existing_hook() {
        let repo = make_bare_repo();
        let hook_path = repo.path().join("hooks/post-push");

        // Write a different hook first.
        std::fs::create_dir_all(repo.path().join("hooks")).unwrap();
        std::fs::write(&hook_path, "#!/bin/sh\necho old").unwrap();

        install_hook_for_worktree(
            &repo.path().to_path_buf(),
            "PROJ-branch",
            "/bin/task-master",
        )
        .expect("install_hook_for_worktree");

        let content = std::fs::read_to_string(&hook_path).unwrap();
        assert!(
            !content.contains("echo old"),
            "old hook should be overwritten"
        );
        assert!(content.contains("PROJ-branch"));
    }

    #[test]
    fn test_install_hook_different_worktree_names_produce_distinct_scripts() {
        let repo_a = make_bare_repo();
        let repo_b = make_bare_repo();

        install_hook_for_worktree(&repo_a.path().to_path_buf(), "PROJ-alpha", "/bin/tm").unwrap();
        install_hook_for_worktree(&repo_b.path().to_path_buf(), "PROJ-beta", "/bin/tm").unwrap();

        let script_a = std::fs::read_to_string(repo_a.path().join("hooks/post-push")).unwrap();
        let script_b = std::fs::read_to_string(repo_b.path().join("hooks/post-push")).unwrap();

        assert!(script_a.contains("PROJ-alpha"));
        assert!(!script_a.contains("PROJ-beta"));
        assert!(script_b.contains("PROJ-beta"));
        assert!(!script_b.contains("PROJ-alpha"));
    }
}
