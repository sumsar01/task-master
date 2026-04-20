use crate::registry::Registry;
use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Command;
use tracing::info;

/// Clone a bare git repo and register it as a new project in task-master.toml.
pub fn cmd_add_project(base_dir: &PathBuf, name: &str, short: &str, url: &str) -> Result<()> {
    // Check short name not already taken
    let config_path = base_dir.join("task-master.toml");
    if config_path.exists() {
        if let Ok(existing) = Registry::load(base_dir.clone()) {
            if existing.find_project(short).is_some() {
                bail!("Project short name '{}' is already in use.", short);
            }
        }
    }

    let projects_dir = base_dir.join("projects");
    std::fs::create_dir_all(&projects_dir).context("Failed to create projects/ directory")?;

    let repo_path = projects_dir.join(name);
    if repo_path.exists() {
        bail!("Directory already exists: {}", repo_path.display());
    }

    info!("Cloning bare repo {} -> {}", url, repo_path.display());
    let status = Command::new("git")
        .args(["clone", "--bare", url])
        .arg(&repo_path)
        .status()
        .context("Failed to run git clone")?;

    if !status.success() {
        bail!("git clone failed");
    }

    // Append [[projects]] block to task-master.toml
    let repo_rel = format!("projects/{}", name);

    let new_block = format!(
        "\n[[projects]]\nname = \"{}\"\nshort = \"{}\"\nrepo = \"{}\"\n",
        name, short, repo_rel
    );

    let mut contents = if config_path.exists() {
        std::fs::read_to_string(&config_path)?
    } else {
        String::new()
    };
    contents.push_str(&new_block);
    std::fs::write(&config_path, &contents)?;

    println!(
        "Added project {} ({}). Add a worktree with:\n  task-master add-worktree {} olive",
        name, short, short
    );

    Ok(())
}
