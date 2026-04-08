use crate::hooks;
use crate::registry::{self, Registry};
use crate::tmux;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use toml_edit::{value, DocumentMut, Item, Table};
use tracing::info;

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

    // 2. Fetch latest from remote (non-fatal).
    if !git_ok(&["fetch", "origin"])? {
        eprintln!(
            "Warning: git fetch failed in '{}'; will reset to local branch tip.",
            path.display()
        );
    }

    // 3. Reset to master (or main) at origin.
    //
    // Strategy A: try `git checkout <branch>` — works in plain clones and when the
    //             worktree is already on that branch (bare repo case where the branch
    //             isn't locked by another worktree).
    // Strategy B: if checkout fails (e.g. bare repo — "branch already used by worktree"),
    //             fall back to `git reset --hard origin/<branch>`, which resets content
    //             without needing to switch the branch name.
    let reset_to_master = |branch: &str| -> Result<bool> {
        // Try direct checkout first.
        if git_ok(&["checkout", branch])? {
            // Bring the branch up to date with origin now that we fetched.
            if !git_ok(&["reset", "--hard", &format!("origin/{}", branch)])? {
                eprintln!(
                    "Warning: git reset --hard origin/{} failed; using local tip.",
                    branch
                );
            }
            return Ok(true);
        }
        // Checkout failed — try hard-reset to remote ref (bare-repo worktree path).
        let remote_ref = format!("origin/{}", branch);
        if git_ok(&["reset", "--hard", &remote_ref])? {
            return Ok(true);
        }
        // Neither worked for this branch name.
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

    // 4. Remove untracked files so the agent starts clean.
    git_ok(&["clean", "-fd"])?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Beads project-level coordination helpers
// ---------------------------------------------------------------------------

/// Returns the path to the primary `.beads/` directory for the given project,
/// by scanning the project's registered worktrees and returning the first one
/// that has a `.beads/` directory with **no** `redirect` file inside it.
///
/// Returns `None` when no worktrees have been initialised yet.
fn find_primary_beads_dir(registry: &Registry, project_short: &str) -> Option<PathBuf> {
    registry
        .worktrees
        .iter()
        .filter(|w| w.project_short.eq_ignore_ascii_case(project_short))
        .map(|w| w.abs_path.join(".beads"))
        .find(|beads| beads.is_dir() && !beads.join("redirect").exists())
}

/// Run `bd init` in `worktree_path` to create the primary `.beads/` database.
///
/// Uses `--non-interactive` so the command never prompts.  The issue prefix is
/// set to the project short name so issue IDs look like `TM-abc`.
fn init_beads_primary(worktree_path: &Path, project_short: &str) -> Result<()> {
    info!(
        "Running: bd init --prefix {} --non-interactive in {}",
        project_short,
        worktree_path.display()
    );
    let output = Command::new("bd")
        .args(["init", "--prefix", project_short, "--non-interactive"])
        .current_dir(worktree_path)
        .output()
        .context("Failed to run bd init (is bd installed?)")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        // bd init exits non-zero when the database already exists and prints a
        // helpful message.  Treat "already initialized" as a soft warning.
        let combined = format!("{}{}", stdout, stderr);
        if combined.contains("already initialized") || combined.contains("already exists") {
            eprintln!(
                "Note: bd init in '{}' reported the database already exists; skipping.",
                worktree_path.display()
            );
            return Ok(());
        }
        bail!("bd init failed: {}", combined.trim());
    }
    Ok(())
}

/// Write a `.beads/redirect` file in `secondary_path` pointing at `primary_beads`.
///
/// The redirect path is computed as a relative path from `secondary_path` to
/// `primary_beads` so the layout is portable if the project directory is moved.
fn write_beads_redirect(secondary_path: &Path, primary_beads: &Path) -> Result<()> {
    // Compute a relative path from the secondary worktree dir to the primary .beads/.
    let rel = pathdiff::diff_paths(primary_beads, secondary_path).with_context(|| {
        format!(
            "Could not compute relative path from '{}' to '{}'",
            secondary_path.display(),
            primary_beads.display()
        )
    })?;

    let beads_dir = secondary_path.join(".beads");
    std::fs::create_dir_all(&beads_dir)
        .with_context(|| format!("Failed to create directory '{}'", beads_dir.display()))?;

    let redirect_path = beads_dir.join("redirect");
    let redirect_content = rel.to_string_lossy().to_string();
    std::fs::write(&redirect_path, &redirect_content).with_context(|| {
        format!(
            "Failed to write redirect file '{}'",
            redirect_path.display()
        )
    })?;

    info!(
        "Wrote .beads/redirect in '{}' -> '{}'",
        secondary_path.display(),
        redirect_content
    );
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
    // With no branch: checks out HEAD (detached), then we immediately create a branch
    // With --branch: creates a new branch
    let mut git_args = vec!["worktree", "add"];

    let worktree_path_str = worktree_path.to_string_lossy().to_string();
    git_args.push(&worktree_path_str);

    let branch_owned;
    if let Some(b) = branch {
        git_args.push("-b");
        branch_owned = b.to_string();
        git_args.push(&branch_owned);
    }
    // no branch = uses HEAD

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

    let new_toml = append_worktree_to_toml(&contents, project_short, worktree_name)
        .context("Failed to update task-master.toml")?;
    std::fs::write(&config_path, new_toml)?;

    info!(
        "Added {}. Spawn with: task-master spawn {} \"<prompt>\"",
        window_name, window_name
    );

    // Auto-install the QA post-push hook for the new worktree.
    // Pass the project short name (e.g. "WIS"), not the full window name —
    // the hook detects the worktree leaf at runtime from $GIT_DIR.
    match hooks::install_hook_for_single(&worktree_path, project_short) {
        Ok(()) => {}
        Err(e) => {
            // Non-fatal: warn but don't fail add-worktree.
            eprintln!(
                "Warning: could not install QA hook for {}: {}",
                window_name, e
            );
            eprintln!("Run `task-master install-qa-hooks` manually later.");
        }
    }

    // Set up project-level beads coordination.
    // If a primary .beads/ already exists in a sibling worktree, write a
    // redirect file so the new worktree shares that database.
    // If no primary exists yet, run bd init in the new worktree to create one.
    match find_primary_beads_dir(registry, project_short) {
        Some(primary_beads) => match write_beads_redirect(&worktree_path, &primary_beads) {
            Ok(()) => eprintln!(
                "Note: wrote .beads/redirect in '{}' → primary at '{}'",
                worktree_path.display(),
                primary_beads.display()
            ),
            Err(e) => eprintln!(
                "Warning: could not write .beads/redirect for {}: {}. \
                     Run `bd init` manually in the worktree to share issues.",
                window_name, e
            ),
        },
        None => match init_beads_primary(&worktree_path, project_short) {
            Ok(()) => {}
            Err(e) => eprintln!(
                "Warning: could not run bd init for {}: {}. \
                     Run `bd init --prefix {}` manually.",
                window_name, e, project_short
            ),
        },
    }

    Ok(format!(
        "Added {}. Spawn with:\n  task-master spawn {} \"<prompt>\"",
        window_name, window_name
    ))
}

// ---------------------------------------------------------------------------
// remove-worktree
// ---------------------------------------------------------------------------

pub fn cmd_remove_worktree(
    registry: &Registry,
    base_dir: &PathBuf,
    window_name: &str,
    force: bool,
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

    // Check if this worktree is the primary beads host (has .beads/ but no redirect).
    let this_beads = worktree.abs_path.join(".beads");
    let is_primary = this_beads.is_dir() && !this_beads.join("redirect").exists();

    // Collect sibling worktrees for this project (excluding the one being removed).
    let siblings: Vec<PathBuf> = registry
        .worktrees
        .iter()
        .filter(|w| {
            w.project_short
                .eq_ignore_ascii_case(&worktree.project_short)
                && w.window_name != worktree.window_name
        })
        .map(|w| w.abs_path.clone())
        .collect();

    if is_primary && !siblings.is_empty() {
        // Re-elect the new primary: pick the first sibling alphabetically for
        // determinism.  We sort by window_name so the choice is predictable.
        let mut sorted_siblings = siblings.clone();
        sorted_siblings.sort();
        let new_primary_wt = &sorted_siblings[0];
        let new_primary_beads = new_primary_wt.join(".beads");

        eprintln!(
            "Note: removing primary beads worktree '{}'. \
             Re-electing '{}' as new primary.",
            window_base,
            new_primary_wt.display()
        );

        // Copy the primary .beads/ to the new primary, excluding runtime files.
        copy_beads_dir(&this_beads, &new_primary_beads).with_context(|| {
            format!(
                "Failed to copy .beads/ from '{}' to '{}'",
                this_beads.display(),
                new_primary_beads.display()
            )
        })?;

        // Update redirect files in all other siblings to point at the new primary.
        for sibling in &siblings {
            if sibling == new_primary_wt {
                // This is the new primary — remove any existing redirect file so it
                // becomes a proper primary (no redirect).
                let redirect = sibling.join(".beads").join("redirect");
                if redirect.exists() {
                    std::fs::remove_file(&redirect).with_context(|| {
                        format!("Failed to remove redirect file '{}'", redirect.display())
                    })?;
                }
            } else {
                // Repoint other secondaries at the new primary.
                match write_beads_redirect(sibling, &new_primary_beads) {
                    Ok(()) => {}
                    Err(e) => eprintln!(
                        "Warning: could not update .beads/redirect in '{}': {}",
                        sibling.display(),
                        e
                    ),
                }
            }
        }
    }

    // Run `git worktree remove [--force] <path>` from the bare repo root.
    // The bare repo is `base_dir/<project.repo>`.
    let project = registry
        .find_project(&worktree.project_short)
        .with_context(|| format!("Project '{}' not found", worktree.project_short))?;
    let repo_path = base_dir.join(&project.repo);

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
    let git_status = Command::new("git")
        .arg("-C")
        .arg(&repo_path)
        .args(&git_args)
        .status()
        .context("Failed to run git worktree remove")?;

    if !git_status.success() {
        bail!("git worktree remove failed");
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

/// Copy a `.beads/` directory to a new location, skipping runtime-only files
/// that should not be transferred (PID files, port files, locks, logs).
fn copy_beads_dir(src: &Path, dst: &Path) -> Result<()> {
    // Files that are runtime-only and must not be copied to the new primary.
    const SKIP_FILES: &[&str] = &[
        "dolt-server.pid",
        "dolt-server.port",
        "dolt-server.lock",
        "dolt-server.log",
        "dolt-server.activity",
        "dolt-config.log",
        // The redirect file from the old primary must not carry over.
        "redirect",
    ];

    std::fs::create_dir_all(dst)
        .with_context(|| format!("Failed to create '{}'", dst.display()))?;

    for entry in
        std::fs::read_dir(src).with_context(|| format!("Failed to read '{}'", src.display()))?
    {
        let entry = entry.with_context(|| format!("Failed to iterate '{}'", src.display()))?;
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();

        if SKIP_FILES.iter().any(|skip| *skip == name_str.as_ref()) {
            continue;
        }

        let src_path = entry.path();
        let dst_path = dst.join(&file_name);

        if src_path.is_dir() {
            copy_beads_dir(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path).with_context(|| {
                format!(
                    "Failed to copy '{}' to '{}'",
                    src_path.display(),
                    dst_path.display()
                )
            })?;
        }
    }
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
    worktrees.push(new_wt);

    Ok(doc.to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------
    // append_worktree_to_toml
    // -------------------------------------------------------------------------

    const BASE_TOML: &str = r#"[[projects]]
name = "warehouse-integration-service"
short = "WIS"
repo = "projects/warehouse-integration-service"

[[projects.worktrees]]
name = "olive"
"#;

    #[test]
    fn test_append_worktree_adds_new_entry() {
        let result = append_worktree_to_toml(BASE_TOML, "WIS", "cedar").unwrap();
        // The new worktree must appear in the output.
        assert!(result.contains("cedar"), "expected 'cedar' in:\n{}", result);
        // The existing worktree must still be there.
        assert!(
            result.contains("olive"),
            "expected 'olive' still in:\n{}",
            result
        );
    }

    #[test]
    fn test_append_worktree_is_valid_toml() {
        let result = append_worktree_to_toml(BASE_TOML, "WIS", "cedar").unwrap();
        // Round-trip: must parse without error and contain both worktrees.
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
        // "wis" should match the project with short = "WIS".
        let result = append_worktree_to_toml(BASE_TOML, "wis", "birch").unwrap();
        assert!(result.contains("birch"));
    }

    #[test]
    fn test_append_worktree_unknown_project_returns_error() {
        let err = append_worktree_to_toml(BASE_TOML, "XYZ", "branch").unwrap_err();
        assert!(err.to_string().contains("XYZ"));
    }

    #[test]
    fn test_append_worktree_to_project_with_no_prior_worktrees() {
        let toml = r#"[[projects]]
name = "fresh-service"
short = "FS"
repo = "projects/fresh-service"
"#;
        let result = append_worktree_to_toml(toml, "FS", "main").unwrap();
        assert!(result.contains("main"));
        // Validate it is parseable.
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
        let result = append_worktree_to_toml(toml, "B", "new-branch").unwrap();
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
        // Comments and blank lines that toml_edit preserves should not be clobbered.
        let toml = "# top comment\n".to_string() + BASE_TOML;
        let result = append_worktree_to_toml(&toml, "WIS", "branch").unwrap();
        assert!(result.starts_with("# top comment\n"), "comment was lost");
    }

    // -------------------------------------------------------------------------
    // reset_worktree_to_master
    // -------------------------------------------------------------------------

    /// Helper: initialise a bare git repo with one commit on master and return
    /// a linked worktree at `<root>/wt`.
    fn make_git_worktree(root: &std::path::Path) -> std::path::PathBuf {
        let bare = root.join("bare.git");
        let wt = root.join("wt");

        // Init bare repo and make an initial commit so master exists.
        Command::new("git")
            .args(["init", "--bare"])
            .arg(&bare)
            .status()
            .unwrap();

        // Clone into a temp checkout so we can commit.
        let checkout = root.join("checkout");
        Command::new("git")
            .args(["clone"])
            .arg(&bare)
            .arg(&checkout)
            .status()
            .unwrap();

        // Configure git identity for the commit.
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

        // Add a worktree linked to the bare repo.
        Command::new("git")
            .args(["-C"])
            .arg(&bare)
            .args(["worktree", "add"])
            .arg(&wt)
            .arg("master")
            .status()
            .unwrap();

        // Configure identity in the worktree too (needed for commits there).
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
        // Create an uncommitted file.
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
        // A worktree with no remote will have a failing pull; should still return Ok.
        let root = tempfile::tempdir().expect("tempdir");

        // Init a simple local repo (not bare, no remote).
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

        // No remote — pull will fail. reset should warn and return Ok.
        let result = reset_worktree_to_master(&repo, false);
        assert!(
            result.is_ok(),
            "no-remote pull failure should warn+continue: {:?}",
            result
        );
    }

    // -------------------------------------------------------------------------
    // Beads helpers
    // -------------------------------------------------------------------------

    #[test]
    fn test_write_beads_redirect_creates_file_with_relative_path() {
        let root = tempfile::tempdir().expect("tempdir");
        let primary = root.path().join("oak").join(".beads");
        let secondary = root.path().join("walnut");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&secondary).unwrap();

        write_beads_redirect(&secondary, &primary).unwrap();

        let redirect_path = secondary.join(".beads").join("redirect");
        assert!(redirect_path.exists(), "redirect file should be created");
        let content = std::fs::read_to_string(&redirect_path).unwrap();
        // Should be a relative path, not absolute.
        assert!(
            !content.starts_with('/'),
            "redirect path should be relative, got: {}",
            content
        );
        // Resolving the relative path from the secondary dir should give the primary.
        let resolved = secondary.join(&content);
        assert_eq!(
            resolved.canonicalize().unwrap(),
            primary.canonicalize().unwrap(),
            "resolved redirect path should point at primary .beads/"
        );
    }

    #[test]
    fn test_write_beads_redirect_creates_dot_beads_dir_if_missing() {
        let root = tempfile::tempdir().expect("tempdir");
        let primary = root.path().join("primary").join(".beads");
        let secondary = root.path().join("secondary");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&secondary).unwrap();
        // .beads/ does NOT exist in secondary yet — write_beads_redirect should create it.
        assert!(!secondary.join(".beads").exists());
        write_beads_redirect(&secondary, &primary).unwrap();
        assert!(
            secondary.join(".beads").join("redirect").exists(),
            ".beads/redirect must be created even when .beads/ was absent"
        );
    }

    #[test]
    fn test_find_primary_beads_dir_returns_worktree_without_redirect() {
        use crate::registry::Registry;
        let root = tempfile::tempdir().expect("tempdir");

        // Set up a simple two-worktree layout on disk.
        let oak_beads = root.path().join("projects/svc/oak/.beads");
        let walnut_beads = root.path().join("projects/svc/walnut/.beads");
        std::fs::create_dir_all(&oak_beads).unwrap();
        std::fs::create_dir_all(&walnut_beads).unwrap();
        // walnut has a redirect (it is secondary); oak does not (it is primary).
        std::fs::write(walnut_beads.join("redirect"), "../oak/.beads").unwrap();

        let toml = format!(
            r#"
[[projects]]
name = "service"
short = "SVC"
repo = "projects/svc"

[[projects.worktrees]]
name = "oak"

[[projects.worktrees]]
name = "walnut"
"#
        );
        let reg = Registry::load_from_str(&toml, root.path().to_path_buf()).unwrap();

        let primary = find_primary_beads_dir(&reg, "SVC");
        assert!(primary.is_some(), "should find a primary");
        assert_eq!(
            primary.unwrap(),
            root.path().join("projects/svc/oak/.beads"),
            "oak is the primary (no redirect)"
        );
    }

    #[test]
    fn test_find_primary_beads_dir_returns_none_when_no_beads_initialised() {
        use crate::registry::Registry;
        let root = tempfile::tempdir().expect("tempdir");

        let toml = r#"
[[projects]]
name = "service"
short = "SVC"
repo = "projects/svc"

[[projects.worktrees]]
name = "oak"
"#;
        // No .beads/ directories exist on disk at all.
        let reg = Registry::load_from_str(toml, root.path().to_path_buf()).unwrap();
        assert!(
            find_primary_beads_dir(&reg, "SVC").is_none(),
            "should return None when no .beads/ exists"
        );
    }

    #[test]
    fn test_copy_beads_dir_skips_runtime_files() {
        let root = tempfile::tempdir().expect("tempdir");
        let src = root.path().join("src_beads");
        let dst = root.path().join("dst_beads");
        std::fs::create_dir_all(&src).unwrap();

        // Write a data file and a runtime file.
        std::fs::write(src.join("metadata.json"), r#"{"project_id":"abc"}"#).unwrap();
        std::fs::write(src.join("dolt-server.pid"), "12345").unwrap();
        std::fs::write(src.join("redirect"), "../other/.beads").unwrap();

        copy_beads_dir(&src, &dst).unwrap();

        assert!(
            dst.join("metadata.json").exists(),
            "data file should be copied"
        );
        assert!(
            !dst.join("dolt-server.pid").exists(),
            "runtime PID file must be skipped"
        );
        assert!(
            !dst.join("redirect").exists(),
            "redirect file must be skipped (new primary has no redirect)"
        );
    }
}
