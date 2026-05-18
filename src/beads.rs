//! Beads project-level coordination helpers.
//!
//! Handles initialising the `.beads/` database in bare repos and writing the
//! redirect file that links worktrees back to the shared beads store.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;
use tracing::info;

/// Run `bd init` in `repo_path` (the bare repo directory) to create the
/// canonical `.beads/` database for the project.
///
/// Uses `--non-interactive` so the command never prompts.  The issue prefix is
/// set to the project short name so issue IDs look like `TM-abc`.
///
/// This is only called when `repo_path/.beads/` does not yet exist.
pub fn init_beads_in_repo(repo_path: &Path, project_short: &str) -> Result<()> {
    info!(
        "Running: bd init --prefix {} --non-interactive in {}",
        project_short,
        repo_path.display()
    );
    let output = Command::new("bd")
        .args(["init", "--prefix", project_short, "--non-interactive"])
        .current_dir(repo_path)
        .output()
        .context("Failed to run bd init (is bd installed?)")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let combined = format!("{}{}", stdout, stderr);
        // bd init exits non-zero when already initialised — treat as a no-op.
        if combined.contains("already initialized") || combined.contains("already exists") {
            return Ok(());
        }
        bail!("bd init failed: {}", combined.trim());
    }
    Ok(())
}

/// Write a `.beads/redirect` file in `worktree_path` pointing at the bare
/// repo's `.beads/` directory.
///
/// Worktrees are always direct children of the bare repo directory, so the
/// redirect path is always the fixed relative string `../.beads`.
pub fn write_beads_redirect(worktree_path: &Path) -> Result<()> {
    let beads_dir = worktree_path.join(".beads");
    std::fs::create_dir_all(&beads_dir)
        .with_context(|| format!("Failed to create directory '{}'", beads_dir.display()))?;

    let redirect_path = beads_dir.join("redirect");
    std::fs::write(&redirect_path, "../.beads")
        .with_context(|| format!("Failed to write '{}'", redirect_path.display()))?;

    info!(
        "Wrote .beads/redirect in '{}' -> ../.beads",
        worktree_path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_beads_redirect_creates_file_with_correct_content() {
        let root = tempfile::tempdir().expect("tempdir");
        let worktree = root.path().join("walnut");
        std::fs::create_dir_all(&worktree).unwrap();

        write_beads_redirect(&worktree).unwrap();

        let redirect_path = worktree.join(".beads").join("redirect");
        assert!(redirect_path.exists(), "redirect file should be created");
        let content = std::fs::read_to_string(&redirect_path).unwrap();
        assert_eq!(
            content, "../.beads",
            "redirect content must be exactly '../.beads', got: {}",
            content
        );
    }

    #[test]
    fn test_write_beads_redirect_creates_dot_beads_dir_if_missing() {
        let root = tempfile::tempdir().expect("tempdir");
        let worktree = root.path().join("secondary");
        std::fs::create_dir_all(&worktree).unwrap();
        // .beads/ does NOT exist yet — write_beads_redirect should create it.
        assert!(!worktree.join(".beads").exists());
        write_beads_redirect(&worktree).unwrap();
        assert!(
            worktree.join(".beads").join("redirect").exists(),
            ".beads/redirect must be created even when .beads/ was absent"
        );
    }

    #[test]
    fn test_write_beads_redirect_is_idempotent() {
        let root = tempfile::tempdir().expect("tempdir");
        let worktree = root.path().join("oak");
        std::fs::create_dir_all(&worktree).unwrap();

        // Call twice — should not error or corrupt the file.
        write_beads_redirect(&worktree).unwrap();
        write_beads_redirect(&worktree).unwrap();

        let content = std::fs::read_to_string(worktree.join(".beads").join("redirect")).unwrap();
        assert_eq!(
            content, "../.beads",
            "content must still be '../.beads' after second call"
        );
    }
}
