//! Agent config installation helpers.
//!
//! Handles copying `plan.md`, `qa.md`, `e2e.md`, and `opencode.json` from the
//! task-master source tree into registered worktrees so that
//! `opencode --agent plan/qa/e2e` works when running inside those worktrees.

use crate::registry::Registry;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing::info;

/// Copy `plan.md`, `qa.md`, and `e2e.md` from the task-master project's
/// `.opencode/agents/` directory into `<worktree_path>/.opencode/agents/`,
/// and copy `.opencode/opencode.json` into `<worktree_path>/opencode.json`.
///
/// The agent `.md` files are the opencode agent configurations consumed by
/// `opencode --agent plan/qa/e2e` when running inside the target worktree.
/// They must be present in the *worktree's own directory* because opencode
/// looks for agent configs relative to its current working directory, not
/// relative to the task-master project root.
///
/// The `opencode.json` is the project-level permission config. It pre-approves
/// `/tmp/**` under `external_directory` so that agents (including default dev
/// sessions spawned by `task-master spawn`) can read/write task-master
/// coordination files in `/tmp` without triggering permission prompts on every
/// access. It is placed at the worktree root (not inside `.opencode/`) because
/// opencode resolves `opencode.json` from the project working directory.
///
/// `base_dir` is the task-master project root (source of configs).
/// `worktree_path` is the target worktree directory (destination).
///
/// Only files that actually exist in the source are copied — missing source
/// files are silently skipped. Existing destination files are always overwritten
/// so updates propagate when `task-master install-agent-configs` is re-run.
pub fn install_agent_configs(base_dir: &Path, worktree_path: &Path) -> Result<()> {
    let src_agents_dir = base_dir.join(".opencode").join("agents");
    let dst_agents_dir = worktree_path.join(".opencode").join("agents");

    std::fs::create_dir_all(&dst_agents_dir).with_context(|| {
        format!(
            "Failed to create agent config directory '{}'",
            dst_agents_dir.display()
        )
    })?;

    let agents = ["plan.md", "qa.md", "e2e.md", "orchestrate.md"];
    let mut installed = Vec::new();
    for name in &agents {
        let src = src_agents_dir.join(name);
        if !src.exists() {
            continue;
        }
        let dst = dst_agents_dir.join(name);
        std::fs::copy(&src, &dst).with_context(|| {
            format!("Failed to copy '{}' to '{}'", src.display(), dst.display())
        })?;
        installed.push(*name);
    }

    // Distribute opencode.json (permission config) to the worktree root.
    // This pre-approves /tmp access for all agents, including default dev sessions.
    let src_opencode_json = base_dir.join(".opencode").join("opencode.json");
    if src_opencode_json.exists() {
        let dst_opencode_json = worktree_path.join("opencode.json");
        std::fs::copy(&src_opencode_json, &dst_opencode_json).with_context(|| {
            format!(
                "Failed to copy '{}' to '{}'",
                src_opencode_json.display(),
                dst_opencode_json.display()
            )
        })?;
        installed.push("opencode.json");
    }

    if !installed.is_empty() {
        info!(
            "Installed agent configs {:?} into '{}'",
            installed,
            worktree_path.display()
        );
    }
    Ok(())
}

/// Install agent configs into every registered worktree.
///
/// Iterates all worktrees in the registry and calls [`install_agent_configs`]
/// for each one, copying `plan.md`, `qa.md`, and `e2e.md` from the
/// task-master source directory into the worktree's `.opencode/agents/`.
///
/// Returns a summary string suitable for printing to the user.
pub fn cmd_install_agent_configs(registry: &Registry, base_dir: &PathBuf) -> Result<String> {
    let mut updated = 0usize;
    let mut skipped = 0usize;

    for wt in &registry.worktrees {
        if !wt.abs_path.exists() {
            skipped += 1;
            info!("Skipping '{}' — directory does not exist", wt.window_name);
            continue;
        }
        match install_agent_configs(base_dir, &wt.abs_path) {
            Ok(()) => updated += 1,
            Err(e) => {
                eprintln!(
                    "Warning: could not install agent configs for '{}': {}",
                    wt.window_name, e
                );
                skipped += 1;
            }
        }
    }

    Ok(format!(
        "Agent configs and permissions installed in {} worktree(s){skipped_note}.",
        updated,
        skipped_note = if skipped > 0 {
            format!(" ({} skipped — see warnings above)", skipped)
        } else {
            String::new()
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_install_agent_configs_copies_files() {
        let root = tempfile::tempdir().expect("tempdir");
        let src_dir = root.path().join("task-master");
        let dst_dir = root.path().join("worktree");
        let agents_src = src_dir.join(".opencode").join("agents");
        std::fs::create_dir_all(&agents_src).unwrap();
        std::fs::create_dir_all(&dst_dir).unwrap();

        // Write source agent configs.
        std::fs::write(agents_src.join("plan.md"), "plan content").unwrap();
        std::fs::write(agents_src.join("qa.md"), "qa content").unwrap();
        std::fs::write(agents_src.join("e2e.md"), "e2e content").unwrap();

        install_agent_configs(&src_dir, &dst_dir).unwrap();

        let agents_dst = dst_dir.join(".opencode").join("agents");
        assert_eq!(
            std::fs::read_to_string(agents_dst.join("plan.md")).unwrap(),
            "plan content"
        );
        assert_eq!(
            std::fs::read_to_string(agents_dst.join("qa.md")).unwrap(),
            "qa content"
        );
        assert_eq!(
            std::fs::read_to_string(agents_dst.join("e2e.md")).unwrap(),
            "e2e content"
        );
    }

    #[test]
    fn test_install_agent_configs_creates_dest_dir_if_missing() {
        let root = tempfile::tempdir().expect("tempdir");
        let src_dir = root.path().join("task-master");
        let dst_dir = root.path().join("worktree");
        let agents_src = src_dir.join(".opencode").join("agents");
        std::fs::create_dir_all(&agents_src).unwrap();
        // dst_dir/.opencode/agents does NOT exist yet.
        std::fs::create_dir_all(&dst_dir).unwrap();

        std::fs::write(agents_src.join("plan.md"), "plan").unwrap();
        install_agent_configs(&src_dir, &dst_dir).unwrap();

        assert!(dst_dir
            .join(".opencode")
            .join("agents")
            .join("plan.md")
            .exists());
    }

    #[test]
    fn test_install_agent_configs_skips_missing_source_files() {
        let root = tempfile::tempdir().expect("tempdir");
        let src_dir = root.path().join("task-master");
        let dst_dir = root.path().join("worktree");
        let agents_src = src_dir.join(".opencode").join("agents");
        std::fs::create_dir_all(&agents_src).unwrap();
        std::fs::create_dir_all(&dst_dir).unwrap();

        // Only plan.md exists in source; qa.md and e2e.md are absent.
        std::fs::write(agents_src.join("plan.md"), "plan").unwrap();
        install_agent_configs(&src_dir, &dst_dir).unwrap();

        let agents_dst = dst_dir.join(".opencode").join("agents");
        assert!(
            agents_dst.join("plan.md").exists(),
            "plan.md should be copied"
        );
        assert!(
            !agents_dst.join("qa.md").exists(),
            "qa.md should be skipped"
        );
        assert!(
            !agents_dst.join("e2e.md").exists(),
            "e2e.md should be skipped"
        );
    }

    #[test]
    fn test_install_agent_configs_overwrites_existing() {
        let root = tempfile::tempdir().expect("tempdir");
        let src_dir = root.path().join("task-master");
        let dst_dir = root.path().join("worktree");
        let agents_src = src_dir.join(".opencode").join("agents");
        let agents_dst = dst_dir.join(".opencode").join("agents");
        std::fs::create_dir_all(&agents_src).unwrap();
        std::fs::create_dir_all(&agents_dst).unwrap();

        // Pre-populate destination with old content.
        std::fs::write(agents_dst.join("plan.md"), "old content").unwrap();
        std::fs::write(agents_src.join("plan.md"), "new content").unwrap();

        install_agent_configs(&src_dir, &dst_dir).unwrap();

        assert_eq!(
            std::fs::read_to_string(agents_dst.join("plan.md")).unwrap(),
            "new content",
            "install_agent_configs should overwrite stale files"
        );
    }

    #[test]
    fn test_install_agent_configs_empty_source_is_ok() {
        let root = tempfile::tempdir().expect("tempdir");
        let src_dir = root.path().join("task-master");
        let dst_dir = root.path().join("worktree");
        // Source has NO .opencode/agents directory at all.
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(&dst_dir).unwrap();

        // Should not error even when no files exist to copy.
        install_agent_configs(&src_dir, &dst_dir).unwrap();
    }

    #[test]
    fn test_install_agent_configs_copies_opencode_json() {
        let root = tempfile::tempdir().expect("tempdir");
        let src_dir = root.path().join("task-master");
        let dst_dir = root.path().join("worktree");
        let agents_src = src_dir.join(".opencode").join("agents");
        std::fs::create_dir_all(&agents_src).unwrap();
        std::fs::create_dir_all(&dst_dir).unwrap();

        // Write opencode.json in source .opencode/ dir.
        let opencode_json_content = r#"{"$schema":"https://opencode.ai/config.json"}"#;
        std::fs::write(
            src_dir.join(".opencode").join("opencode.json"),
            opencode_json_content,
        )
        .unwrap();

        install_agent_configs(&src_dir, &dst_dir).unwrap();

        // opencode.json must be placed at the worktree root, not inside .opencode/.
        let dst_json = dst_dir.join("opencode.json");
        assert!(
            dst_json.exists(),
            "opencode.json should be copied to worktree root"
        );
        assert_eq!(
            std::fs::read_to_string(&dst_json).unwrap(),
            opencode_json_content
        );
    }

    #[test]
    fn test_install_agent_configs_skips_opencode_json_when_absent() {
        let root = tempfile::tempdir().expect("tempdir");
        let src_dir = root.path().join("task-master");
        let dst_dir = root.path().join("worktree");
        let agents_src = src_dir.join(".opencode").join("agents");
        std::fs::create_dir_all(&agents_src).unwrap();
        std::fs::create_dir_all(&dst_dir).unwrap();

        // No opencode.json in source — install should succeed without error.
        std::fs::write(agents_src.join("plan.md"), "plan").unwrap();
        install_agent_configs(&src_dir, &dst_dir).unwrap();

        // opencode.json must NOT be created in the destination.
        assert!(
            !dst_dir.join("opencode.json").exists(),
            "opencode.json should not appear when source is absent"
        );
    }

    #[test]
    fn test_install_agent_configs_overwrites_opencode_json() {
        let root = tempfile::tempdir().expect("tempdir");
        let src_dir = root.path().join("task-master");
        let dst_dir = root.path().join("worktree");
        let agents_src = src_dir.join(".opencode").join("agents");
        std::fs::create_dir_all(&agents_src).unwrap();
        std::fs::create_dir_all(&dst_dir).unwrap();

        // Pre-populate destination with old content.
        std::fs::write(dst_dir.join("opencode.json"), "old").unwrap();
        std::fs::write(src_dir.join(".opencode").join("opencode.json"), "new").unwrap();

        install_agent_configs(&src_dir, &dst_dir).unwrap();

        assert_eq!(
            std::fs::read_to_string(dst_dir.join("opencode.json")).unwrap(),
            "new",
            "install_agent_configs should overwrite stale opencode.json"
        );
    }
}
